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

struct App {
    shm: Shm,
}

struct GlassStyle {
    width: i32,
    height: i32,
    radius: i32,

    // Порядок байтов в памяти: blue, green, red, alpha.
    // Цвет уже должен быть предумножен на alpha.
    tint_bgra: [u8; 4],
}

impl GlassStyle {
    fn stride(&self) -> i32 {
        self.width * BYTES_PER_PIXEL
    }

    fn buffer_size(&self) -> usize {
        (self.stride() * self.height) as usize
    }
}

fn render_glass(canvas: &mut [u8], style: &GlassStyle) {
    let radius = style.radius as f32;

    let inner_right = style.width as f32 - radius - 1.0;
    let inner_bottom = style.height as f32 - radius - 1.0;

    for y in 0..style.height {
        for x in 0..style.width {
            let x = x as f32;
            let y = y as f32;

            let nearest_x = x.clamp(radius, inner_right);
            let nearest_y = y.clamp(radius, inner_bottom);

            let dx = x - nearest_x;
            let dy = y - nearest_y;

            let distance = (dx * dx + dy * dy).sqrt();

            let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0);

            let pixel_color = [
                (style.tint_bgra[0] as f32 * coverage).round() as u8,
                (style.tint_bgra[1] as f32 * coverage).round() as u8,
                (style.tint_bgra[2] as f32 * coverage).round() as u8,
                (style.tint_bgra[3] as f32 * coverage).round() as u8,
            ];

            let offset = ((y as i32 * style.width + x as i32) * BYTES_PER_PIXEL) as usize;

            let pixel_end = offset + BYTES_PER_PIXEL as usize;

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
        radius: 14,
        tint_bgra: [0x40, 0x3C, 0x3A, 0x40],
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
    layer_surface.set_margin(120, 0, 0, 120);

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

    println!("Стеклянная поверхность отрисована");

    loop {
        event_queue
            .blocking_dispatch(&mut app)
            .expect("ошибка обработки Wayland-событий");
    }
}
