mod app;
mod renderer;

use smithay_client_toolkit::{
    registry::RegistryState,
    seat::SeatState,
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, Layer, LayerShell},
    },
};
use wayland_client::{
    Connection, globals::registry_queue_init, protocol::wl_compositor,
};

use app::{App, OUTPUT_HEIGHT, OUTPUT_WIDTH};

const SURFACE_WIDTH: i32 = 400;
const SURFACE_HEIGHT: i32 = 200;

fn main() {
    let initial_left = ((OUTPUT_WIDTH - SURFACE_WIDTH) / 2).max(0);
    let initial_top = ((OUTPUT_HEIGHT - SURFACE_HEIGHT) / 2).max(0);

    let connection =
        Connection::connect_to_env().expect("failed to connect to the Wayland compositor");

    let (globals, mut event_queue) =
        registry_queue_init::<App>(&connection).expect("failed to init Wayland registry");
    let queue_handle = event_queue.handle();

    let registry_state = RegistryState::new(&globals);
    let seat_state = SeatState::new(&globals, &queue_handle);

    let compositor: wl_compositor::WlCompositor = globals
        .bind(&queue_handle, 1..=6, ())
        .expect("compositor not available");
    let surface = compositor.create_surface(&queue_handle, ());

    let layer_shell = LayerShell::bind(&globals, &queue_handle).expect("layer shell not available");
    let layer_surface = layer_shell.create_layer_surface(
        &queue_handle,
        surface,
        Layer::Overlay,
        Some("hks-shell"),
        None,
    );

    let mut app = App {
        registry_state,
        seat_state,

        layer_surface,
        pointer: None,

        renderer: None,

        dragging: false,
        grab_position: (0.0, 0.0),

        position_top: initial_top,
        position_left: initial_left,

        surface_width: SURFACE_WIDTH,
        surface_height: SURFACE_HEIGHT,

        running: true,
    };

    app.layer_surface
        .set_size(SURFACE_WIDTH as u32, SURFACE_HEIGHT as u32);
    app.layer_surface.set_anchor(Anchor::TOP | Anchor::LEFT);
    app.layer_surface
        .set_margin(app.position_top, 0, 0, app.position_left);

    // First commit requests a configure from the compositor; the renderer is
    // created and first frame drawn in the configure handler.
    app.layer_surface.commit();

    while app.running {
        event_queue
            .blocking_dispatch(&mut app)
            .expect("Wayland event dispatch error");
    }
}
