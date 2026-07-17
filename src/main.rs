use smithay_client_toolkit::{
    delegate_layer, delegate_shm,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};

use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_compositor, wl_registry, wl_shm, wl_surface},
};

const BYTES_PER_PIXEL: i32 = 4;

const POSITION_TOP: i32 = 120;
const POSITION_LEFT: i32 = 120;

struct App {
    shm: Shm,
}

struct GlassStyle {
    width: i32,
    height: i32,
    radius: f32,

    // Обычный, ещё не предумноженный цвет: red, green, blue.
    // Значения находятся в диапазоне 0.0..=1.0.
    tint_rgb: [f32; 3],

    base_alpha: f32,
    border_width: f32,
    highlight_strength: f32,
    shadow_strength: f32,
    sheen_strength: f32,
}

impl GlassStyle {
    fn stride(&self) -> i32 {
        self.width * BYTES_PER_PIXEL
    }

    fn buffer_size(&self) -> usize {
        (self.stride() * self.height) as usize
    }
}

fn smoothstep(edge_start: f32, edge_end: f32, value: f32) -> f32 {
    let progress = ((value - edge_start) / (edge_end - edge_start)).clamp(0.0, 1.0);

    progress * progress * (3.0 - 2.0 * progress)
}

fn rounded_rectangle_distance(x: f32, y: f32, width: f32, height: f32, radius: f32) -> f32 {
    let center_x = width / 2.0;
    let center_y = height / 2.0;

    let local_x = (x - center_x).abs();
    let local_y = (y - center_y).abs();

    let half_width = width / 2.0;
    let half_height = height / 2.0;

    let distance_x = local_x - (half_width - radius);
    let distance_y = local_y - (half_height - radius);

    let outside_x = distance_x.max(0.0);
    let outside_y = distance_y.max(0.0);

    let outside_distance = (outside_x * outside_x + outside_y * outside_y).sqrt();
    let inside_distance = distance_x.max(distance_y).min(0.0);

    outside_distance + inside_distance - radius
}

fn premultiplied_bgra(rgb: [f32; 3], alpha: f32, coverage: f32) -> [u8; 4] {
    let final_alpha = (alpha * coverage).clamp(0.0, 1.0);

    let red = (rgb[0].clamp(0.0, 1.0) * final_alpha * 255.0).round() as u8;
    let green = (rgb[1].clamp(0.0, 1.0) * final_alpha * 255.0).round() as u8;
    let blue = (rgb[2].clamp(0.0, 1.0) * final_alpha * 255.0).round() as u8;
    let alpha = (final_alpha * 255.0).round() as u8;

    // Формат ARGB8888 на little-endian машине хранится как BGRA.
    [blue, green, red, alpha]
}

fn render_glass(canvas: &mut [u8], style: &GlassStyle) {
    let width = style.width as f32;
    let height = style.height as f32;

    for y in 0..style.height {
        for x in 0..style.width {
            // Работаем с центром пикселя, а не с его верхним левым углом.
            let pixel_x = x as f32 + 0.5;
            let pixel_y = y as f32 + 0.5;

            let distance =
                rounded_rectangle_distance(pixel_x, pixel_y, width, height, style.radius);

            // distance < 0: внутри фигуры.
            // distance > 0: снаружи фигуры.
            let coverage = 1.0 - smoothstep(-0.75, 0.75, distance);

            let offset = ((y * style.width + x) * BYTES_PER_PIXEL) as usize;
            let pixel_end = offset + BYTES_PER_PIXEL as usize;

            if coverage <= 0.0 {
                canvas[offset..pixel_end].fill(0);
                continue;
            }

            let normalized_x = pixel_x / width;
            let normalized_y = pixel_y / height;

            // Насколько близко пиксель расположен к внутренней границе.
            let distance_inside = (-distance).max(0.0);

            let edge_factor = 1.0 - smoothstep(0.0, style.border_width, distance_inside);

            // Верхняя и левая части границы получают больше света.
            let top_left_direction = (1.0 - normalized_x) * 0.45 + (1.0 - normalized_y) * 0.55;

            let border_highlight =
                edge_factor * top_left_direction.powf(2.2) * style.highlight_strength;

            // Нижняя и правая части границы слегка затемняются.
            let bottom_right_direction = normalized_x * 0.45 + normalized_y * 0.55;

            let border_shadow =
                edge_factor * bottom_right_direction.powf(2.0) * style.shadow_strength;

            // Широкая мягкая диагональная полоса блика.
            let sheen_position = normalized_x * 0.78 + normalized_y * 0.22;
            let sheen_distance = (sheen_position - 0.28).abs();

            let diagonal_sheen = (1.0 - smoothstep(0.0, 0.18, sheen_distance))
                * (1.0 - normalized_y).powf(0.8)
                * style.sheen_strength;

            // Верх материала чуть светлее, низ немного темнее.
            let top_glow = (1.0 - normalized_y).powf(2.0) * 0.045;
            let bottom_shade = normalized_y.powf(2.0) * 0.035;

            let total_light = (border_highlight + diagonal_sheen + top_glow).clamp(0.0, 1.0);

            let total_shadow = (border_shadow + bottom_shade).clamp(0.0, 1.0);

            let mut rgb = style.tint_rgb;

            for channel in &mut rgb {
                // Смешиваем исходный цвет с белым.
                *channel += (1.0 - *channel) * total_light;

                // Затем слегка затемняем противоположную сторону.
                *channel *= 1.0 - total_shadow * 0.70;
            }

            let alpha = (style.base_alpha
                + border_highlight * 0.20
                + diagonal_sheen * 0.12
                + border_shadow * 0.10)
                .clamp(0.0, 0.65);

            let pixel_color = premultiplied_bgra(rgb, alpha, coverage);

            canvas[offset..pixel_end].copy_from_slice(&pixel_color);
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for App {
    fn event(
        _state: &mut Self,
        _compositor: &wl_compositor::WlCompositor,
        _event: wl_compositor::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for App {
    fn event(
        _state: &mut Self,
        _surface: &wl_surface::WlSurface,
        _event: wl_surface::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
    }
}

impl LayerShellHandler for App {
    fn closed(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _layer: &LayerSurface,
    ) {
    }

    fn configure(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        serial: u32,
    ) {
        println!(
            "Niri настроил поверхность: {}x{}, serial={serial}",
            configure.new_size.0, configure.new_size.1,
        );
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_shm!(App);
delegate_layer!(App);

fn main() {
    let style = GlassStyle {
        width: 240,
        height: 240,
        radius: 14.0,

        // Очень светлый холодный оттенок.
        tint_rgb: [0.90, 0.94, 1.0],

        base_alpha: 0.04,
        border_width: 6.4,
        highlight_strength: 0.25,
        shadow_strength: 0.28,
        sheen_strength: 0.05,
    };

    let connection =
        Connection::connect_to_env().expect("не удалось подключиться к Wayland-композитору");

    let (globals, mut event_queue) =
        registry_queue_init::<App>(&connection).expect("не удалось получить Wayland registry");

    let queue_handle = event_queue.handle();

    let shm = Shm::bind(&globals, &queue_handle).expect("Niri не предоставляет wl_shm");

    let compositor: wl_compositor::WlCompositor = globals
        .bind(&queue_handle, 1..=6, ())
        .expect("Niri не предоставляет wl_compositor");

    let surface = compositor.create_surface(&queue_handle, ());

    let layer_shell =
        LayerShell::bind(&globals, &queue_handle).expect("Niri не предоставляет Layer Shell");

    let layer_surface = layer_shell.create_layer_surface(
        &queue_handle,
        surface,
        Layer::Overlay,
        Some("hks-shell"),
        None,
    );

    let mut pool = SlotPool::new(style.buffer_size(), &shm).expect("не удалось создать SHM-пул");

    let mut app = App { shm };

    layer_surface.set_size(style.width as u32, style.height as u32);
    layer_surface.set_anchor(Anchor::TOP | Anchor::LEFT);
    layer_surface.set_margin(POSITION_TOP, 0, 0, POSITION_LEFT);

    // Первый commit просит Niri настроить поверхность.
    layer_surface.commit();

    event_queue
        .roundtrip(&mut app)
        .expect("не удалось получить configure от Niri");

    let (buffer, canvas) = pool
        .create_buffer(
            style.width,
            style.height,
            style.stride(),
            wl_shm::Format::Argb8888,
        )
        .expect("не удалось создать пиксельный буфер");

    render_glass(canvas, &style);

    layer_surface
        .wl_surface()
        .damage_buffer(0, 0, style.width, style.height);

    buffer
        .attach_to(layer_surface.wl_surface())
        .expect("не удалось прикрепить буфер");

    layer_surface.commit();

    println!("Стеклянный материал отрисован");

    loop {
        event_queue
            .blocking_dispatch(&mut app)
            .expect("ошибка обработки Wayland-событий");
    }
}
