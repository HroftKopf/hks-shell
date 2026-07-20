use smithay_client_toolkit::{
    delegate_layer, delegate_pointer, delegate_registry, delegate_seat, delegate_shm,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{BTN_LEFT, PointerEvent, PointerEventKind, PointerHandler},
    },
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
    globals::registry_queue_init,
    protocol::{wl_compositor, wl_pointer, wl_seat, wl_shm, wl_surface},
};

const BYTES_PER_PIXEL: i32 = 4;

// Текущий вывод: 3440x1440 при scale 1.2.
// Layer Shell использует логические координаты.
const OUTPUT_WIDTH: i32 = 2867;
const OUTPUT_HEIGHT: i32 = 1200;

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    shm: Shm,

    layer_surface: LayerSurface,
    pointer: Option<wl_pointer::WlPointer>,

    dragging: bool,
    grab_position: (f64, f64),

    position_top: i32,
    position_left: i32,

    surface_width: i32,
    surface_height: i32,

    running: bool,
}

struct GlassStyle {
    width: i32,
    height: i32,

    // ВАЖНО: должен совпадать с `geometry-corner-radius` в layer-rule
    // конкретно для namespace="^hks-shell$" в config.kdl. Niri обрезает
    // саму geometry поверхности этим значением НЕЗАВИСИМО от того, что
    // нарисовано в буфере — если там стоит 14, а тут 140, видимая форма
    // всё равно будет обрезана по 14px композитором раньше, чем дойдёт
    // до альфа-маски из render_glass().
    radius: f32,

    // 2.0 = обычный круглый угол.
    // 4.0–6.0 = более плавный continuous/squircle-угол.
    // Диапазон расширен до 24.0: при близких к half_width радиусах
    // (почти круг) высокий corner_power визуально почти не отличим от
    // низкого, так что практического смысла ограничивать до 8.0 не было —
    // но сильно выше 24 superellipse начинает давать заметные артефакты
    // на прямых сторонах.
    corner_power: f32,

    // Насколько мягко исчезает внешний край формы.
    edge_feather: f32,

    // Ширина плавного появления материала от края к центру.
    material_fade_width: f32,

    // Сколько материала остаётся прямо у края.
    edge_alpha_scale: f32,

    // Обычный RGB в диапазоне 0.0..=1.0.
    tint_rgb: [f32; 3],

    base_alpha: f32,

    // Ширина полосы вдоль края, в которой считаются highlight/shadow.
    // Раньше было 1.0 — практически невидимая полоса в 1px. Теперь это
    // реальная толщина "кромки стекла" в пикселях.
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

fn smootherstep(edge_start: f32, edge_end: f32, value: f32) -> f32 {
    let progress = ((value - edge_start) / (edge_end - edge_start)).clamp(0.0, 1.0);

    progress * progress * progress * (progress * (progress * 6.0 - 15.0) + 10.0)
}

// Rounded rectangle with continuous/squircle-like corners.
//
// corner_power:
// 2.0 = обычная круглая дуга;
// 4.0–6.0 = угол начинает изгибаться заметно раньше и без резкого стыка.
fn continuous_rounded_rectangle_distance(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    corner_power: f32,
) -> f32 {
    let center_x = width * 0.5;
    let center_y = height * 0.5;

    let half_width = width * 0.5;
    let half_height = height * 0.5;

    let safe_radius = radius.clamp(1.0, half_width.min(half_height).max(1.0) - 0.5);

    // Было: .clamp(2.0, 8.0) — молча резало любое значение выше 8.0,
    // включая ранее переданные 40.5, без предупреждения.
    let power = corner_power.clamp(2.0, 24.0);

    let local_x = (x - center_x).abs();
    let local_y = (y - center_y).abs();

    let qx = local_x - (half_width - safe_radius);
    let qy = local_y - (half_height - safe_radius);

    let outside_x = qx.max(0.0);
    let outside_y = qy.max(0.0);

    // Вместо окружности используем superellipse-норму.
    // Она даёт более длинный и мягкий переход от прямой стороны к углу.
    let outside_distance = (outside_x.powf(power) + outside_y.powf(power)).powf(1.0 / power);

    let inside_distance = qx.max(qy).min(0.0);

    outside_distance + inside_distance - safe_radius
}

fn premultiplied_bgra(rgb: [f32; 3], alpha: f32, coverage: f32) -> [u8; 4] {
    let final_alpha = (alpha * coverage).clamp(0.0, 1.0);

    let red = (rgb[0].clamp(0.0, 1.0) * final_alpha * 255.0).round() as u8;
    let green = (rgb[1].clamp(0.0, 1.0) * final_alpha * 255.0).round() as u8;
    let blue = (rgb[2].clamp(0.0, 1.0) * final_alpha * 255.0).round() as u8;
    let alpha = (final_alpha * 255.0).round() as u8;

    // ARGB8888 на little-endian машине находится в памяти как BGRA.
    [blue, green, red, alpha]
}

fn render_glass(canvas: &mut [u8], style: &GlassStyle) {
    let width = style.width as f32;
    let height = style.height as f32;

    for y in 0..style.height {
        for x in 0..style.width {
            let pixel_x = x as f32 + 0.5;
            let pixel_y = y as f32 + 0.5;

            let distance = continuous_rounded_rectangle_distance(
                pixel_x,
                pixel_y,
                width,
                height,
                style.radius,
                style.corner_power,
            );

            // Широкий smootherstep вместо жёсткого 1–2 px перехода.
            // Это сглаживает сам силуэт и убирает резкое начало закругления.
            let feather = style.edge_feather.max(0.5);

            let coverage = 1.0 - smootherstep(-feather, feather, distance);

            let offset = ((y * style.width + x) * BYTES_PER_PIXEL) as usize;
            let pixel_end = offset + BYTES_PER_PIXEL as usize;

            if coverage <= 0.0 {
                canvas[offset..pixel_end].fill(0);
                continue;
            }

            let normalized_x = pixel_x / width;
            let normalized_y = pixel_y / height;
            let distance_inside = (-distance).max(0.0);

            // Материал появляется не мгновенно на самой границе,
            // а плавно набирает плотность по мере движения внутрь.
            let material_fade =
                smootherstep(0.0, style.material_fade_width.max(1.0), distance_inside);

            let edge_alpha_scale = style.edge_alpha_scale.clamp(0.0, 1.0);

            let edge_material_scale = edge_alpha_scale + (1.0 - edge_alpha_scale) * material_fade;

            // edge_factor: 1.0 прямо на границе, плавно к 0.0 на расстоянии
            // border_width пикселей внутрь. Это ЛОКАЛИЗОВАННАЯ величина —
            // используется дальше, чтобы highlight/shadow были кромкой,
            // а не заливкой всей поверхности.
            let edge_factor =
                1.0 - smootherstep(0.0, style.border_width.max(0.01), distance_inside);

            // Направленный блик у верхне-левого края — как будто источник
            // света сверху-слева. Это ЕДИНСТВЕННЫЙ источник яркости в кромке,
            // намеренно anisotropic (не равномерный), чтобы не выглядеть
            // как ambient-глоу по всему периметру.
            let top_left_direction = (1.0 - normalized_x) * 0.45 + (1.0 - normalized_y) * 0.55;

            let border_highlight =
                edge_factor * top_left_direction.powf(2.2) * style.highlight_strength;

            let bottom_right_direction = normalized_x * 0.45 + normalized_y * 0.55;

            let border_shadow =
                edge_factor * bottom_right_direction.powf(2.0) * style.shadow_strength;

            let sheen_position = normalized_x * 0.78 + normalized_y * 0.22;
            let sheen_distance = (sheen_position - 0.28).abs();

            let diagonal_sheen = (1.0 - smoothstep(0.0, 0.18, sheen_distance))
                * (1.0 - normalized_y).powf(0.8)
                * style.sheen_strength;

            // Было: top_glow/bottom_shade — захардкоженные величины,
            // зависящие только от normalized_y (позиции по вертикали),
            // НЕ от edge_factor. Из-за этого они действовали как
            // равномерная заливка светлее сверху / темнее снизу по ВСЕЙ
            // площади поверхности — визуально это и есть "глоу", а не
            // кромка, о которой шла речь.
            //
            // Стало: домножены на edge_factor, чтобы вклад был только
            // рядом с границей формы, как и остальные термы кромки.
            let top_glow = edge_factor * (1.0 - normalized_y).powf(2.0) * 0.008;
            let bottom_shade = edge_factor * normalized_y.powf(2.0) * 0.018 * style.shadow_strength;

            let total_light = (border_highlight + diagonal_sheen + top_glow).clamp(0.0, 1.0);

            // Раньше bottom_shade не был умножен на shadow_strength — то
            // есть тёмный ореол оставался даже при shadow_strength: 0.0.
            // Теперь оба тёмных терма честно завязаны на shadow_strength.
            let total_shadow = (border_shadow + bottom_shade).clamp(0.0, 1.0);

            let mut rgb = style.tint_rgb;

            for channel in &mut rgb {
                *channel += (1.0 - *channel) * total_light;
                *channel *= 1.0 - total_shadow * 0.70;
            }

            let alpha = (style.base_alpha * edge_material_scale
                + border_highlight * 0.12
                + diagonal_sheen * 0.08
                + border_shadow * 0.06)
                .clamp(0.0, 0.65);

            let pixel_color = premultiplied_bgra(rgb, alpha, coverage);
            canvas[offset..pixel_end].copy_from_slice(&pixel_color);
        }
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
        self.running = false;
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

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
    ) {
    }

    fn new_capability(
        &mut self,
        _connection: &Connection,
        queue_handle: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            let pointer = self
                .seat_state
                .get_pointer(queue_handle, &seat)
                .expect("не удалось получить Wayland-указатель");

            self.pointer = Some(pointer);
            println!("Мышь подключена: зажми ЛКМ внутри квадрата и тащи");
        }
    }

    fn remove_capability(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }

            self.dragging = false;
        }
    }

    fn remove_seat(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
    ) {
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let mut latest_motion = None;
        let mut left_button_released = false;

        for event in events {
            if &event.surface != self.layer_surface.wl_surface() {
                continue;
            }

            match &event.kind {
                PointerEventKind::Press { button, .. } if *button == BTN_LEFT => {
                    self.dragging = true;
                    self.grab_position = event.position;
                }

                PointerEventKind::Motion { .. } if self.dragging => {
                    // В одном pointer frame может быть несколько Motion.
                    // Берём последнее, иначе движение может суммироваться дважды.
                    latest_motion = Some(event.position);
                }

                PointerEventKind::Release { button, .. } if *button == BTN_LEFT => {
                    left_button_released = true;
                }

                _ => {}
            }
        }

        if self.dragging {
            if let Some((pointer_x, pointer_y)) = latest_motion {
                let delta_x = pointer_x - self.grab_position.0;
                let delta_y = pointer_y - self.grab_position.1;

                // grab_position раньше выставлялся только в Press и больше
                // никогда не обновлялся. delta каждый раз считалась от точки
                // первоначального нажатия, хотя pointer-координаты приходят
                // surface-local и "уезжают" вместе с самой поверхностью при
                // каждом set_margin — это давало дрейф/ускорение при драге.
                // Обновляем точку отсчёта на каждое обработанное Motion.
                self.grab_position = (pointer_x, pointer_y);

                if delta_x.abs() >= 0.5 || delta_y.abs() >= 0.5 {
                    let max_left = (OUTPUT_WIDTH - self.surface_width).max(0);
                    let max_top = (OUTPUT_HEIGHT - self.surface_height).max(0);

                    self.position_left = (self.position_left as f64 + delta_x).round() as i32;
                    self.position_top = (self.position_top as f64 + delta_y).round() as i32;

                    self.position_left = self.position_left.clamp(0, max_left);
                    self.position_top = self.position_top.clamp(0, max_top);

                    self.layer_surface
                        .set_margin(self.position_top, 0, 0, self.position_left);

                    // Буфер менять не нужно: двигается вся Layer Shell-поверхность.
                    self.layer_surface.commit();
                }
            }
        }

        if left_button_released {
            self.dragging = false;
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![SeatState];
}

delegate_registry!(App);
delegate_seat!(App);
delegate_pointer!(App);
delegate_shm!(App);
delegate_layer!(App);

fn main() {
    let style = GlassStyle {
        width: 300,
        height: 300,

        // ВАЖНО: синхронизируй с geometry-corner-radius в config.kdl
        // для layer-rule namespace="^hks-shell$". Композитор режет свой
        // эффект по КРУГУ этого радиуса; если буфер клиента рисует форму
        // крупнее/иначе — за кругом остаётся плоский материал клиента без
        // блюра/рефракции (был квадратный ореол вокруг круга).
        radius: 140.0,

        // 2.0 = чистый круглый угол (евклидов), совпадает с обрезкой
        // композитора. Выше 2.0 даёт squircle, который вылезает за круг
        // композитора углами и снова даёт ореол — не трогать, пока
        // композитор не научится резать по squircle.
        corner_power: 2.0,
        edge_feather: 3.5,
        material_fade_width: 22.0,
        edge_alpha_scale: 0.18,

        tint_rgb: [0.93, 0.96, 1.0],

        base_alpha: 0.035,

        // Было 1.0 (практически невидимая полоса в 1px). Увеличено, чтобы
        // направленный блик/тень были заметны как реальная толщина кромки,
        // а не терялись в один пиксель.
        border_width: 10.0,

        // Увеличено, чтобы направленный блик был виден на глаз —
        // при 0.012 он был на уровне погрешности округления в 8bpc.
        highlight_strength: 0.06,

        shadow_strength: 0.0,
        sheen_strength: 0.0,
    };

    let initial_left = ((OUTPUT_WIDTH - style.width) / 2).max(0);
    let initial_top = ((OUTPUT_HEIGHT - style.height) / 2).max(0);

    let connection =
        Connection::connect_to_env().expect("не удалось подключиться к Wayland-композитору");

    let (globals, mut event_queue) =
        registry_queue_init::<App>(&connection).expect("не удалось получить Wayland registry");

    let queue_handle = event_queue.handle();

    let registry_state = RegistryState::new(&globals);
    let seat_state = SeatState::new(&globals, &queue_handle);

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

    let mut app = App {
        registry_state,
        seat_state,
        shm,

        layer_surface,
        pointer: None,

        dragging: false,
        grab_position: (0.0, 0.0),

        position_top: initial_top,
        position_left: initial_left,

        surface_width: style.width,
        surface_height: style.height,

        running: true,
    };

    app.layer_surface
        .set_size(style.width as u32, style.height as u32);

    app.layer_surface.set_anchor(Anchor::TOP | Anchor::LEFT);

    app.layer_surface
        .set_margin(app.position_top, 0, 0, app.position_left);

    // Первый пустой commit запрашивает configure от Niri.
    app.layer_surface.commit();

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

    app.layer_surface
        .wl_surface()
        .damage_buffer(0, 0, style.width, style.height);

    buffer
        .attach_to(app.layer_surface.wl_surface())
        .expect("не удалось прикрепить буфер");

    app.layer_surface.commit();

    println!(
        "Стеклянная поверхность отрисована: {}x{}",
        style.width, style.height
    );

    while app.running {
        event_queue
            .blocking_dispatch(&mut app)
            .expect("ошибка обработки Wayland-событий");
    }
}
