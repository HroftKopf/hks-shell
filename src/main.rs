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

struct App {
    shm: Shm,
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

    println!("Базовая wl_surface создана");

    let layer_shell =
        LayerShell::bind(&globals, &queue_handle).expect("Niri не предоставляет Layer Shell");

    let layer_surface = layer_shell.create_layer_surface(
        &queue_handle,
        surface,
        Layer::Overlay,
        Some("hks-shell"),
        None,
    );

    let mut pool = SlotPool::new(240 * 240 * 4, &shm).expect("не удалось создать SHM-пул");

    let mut app = App { shm };

    layer_surface.set_size(240, 240);
    layer_surface.set_anchor(Anchor::TOP | Anchor::LEFT);
    layer_surface.set_margin(120, 0, 0, 120);

    layer_surface.commit();

    event_queue
        .roundtrip(&mut app)
        .expect("не удалось получить configure от Niri");

    let (buffer, canvas) = pool
        .create_buffer(240, 240, 240 * 4, wl_shm::Format::Argb8888)
        .expect("не удалось создать пиксельный буфер");

    let color: u32 = 0x40_3A_3C_40;

    for pixel in canvas.chunks_exact_mut(4) {
        pixel.copy_from_slice(&color.to_le_bytes());
    }

    layer_surface.wl_surface().damage_buffer(0, 0, 240, 240);

    buffer
        .attach_to(layer_surface.wl_surface())
        .expect("не удалось прикрепить буфер");

    layer_surface.commit();

    println!("SHM-буфер отрисован");

    loop {
        event_queue
            .blocking_dispatch(&mut app)
            .expect("ошибка обработки Wayland-событий");
    }
}
