use std::ffi::c_void;

use smithay_client_toolkit::{
    delegate_layer, delegate_pointer, delegate_registry, delegate_seat,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{BTN_LEFT, PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    },
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    protocol::{wl_compositor, wl_pointer, wl_seat, wl_surface},
};

use crate::renderer::{GlassParams, Renderer};

// Current output: 3440x1440 @ scale 1.2 -> logical 2867x1200.
// Layer Shell works in logical coordinates.
pub const OUTPUT_WIDTH: i32 = 2867;
pub const OUTPUT_HEIGHT: i32 = 1200;

pub struct App {
    pub registry_state: RegistryState,
    pub seat_state: SeatState,

    pub layer_surface: LayerSurface,
    pub pointer: Option<wl_pointer::WlPointer>,

    pub renderer: Option<Renderer>,

    pub dragging: bool,
    pub grab_position: (f64, f64),

    pub position_top: i32,
    pub position_left: i32,

    pub surface_width: i32,
    pub surface_height: i32,

    pub running: bool,
}

impl App {
    fn ensure_renderer(&mut self, connection: &Connection) {
        if self.renderer.is_some() {
            return;
        }
        let display_ptr = connection.backend().display_ptr() as *mut c_void;
        let surface_ptr = self.layer_surface.wl_surface().id().as_ptr() as *mut c_void;

        self.renderer = Some(Renderer::new(
            display_ptr,
            surface_ptr,
            self.surface_width as u32,
            self.surface_height as u32,
            GlassParams::default(),
        ));
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
        connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _layer: &LayerSurface,
        _configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        self.ensure_renderer(connection);
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.render();
        }
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
                .expect("failed to obtain Wayland pointer");
            self.pointer = Some(pointer);
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

                if delta_x.abs() >= 0.5 || delta_y.abs() >= 0.5 {
                    let max_left = (OUTPUT_WIDTH - self.surface_width).max(0);
                    let max_top = (OUTPUT_HEIGHT - self.surface_height).max(0);

                    self.position_left = (self.position_left as f64 + delta_x).round() as i32;
                    self.position_top = (self.position_top as f64 + delta_y).round() as i32;

                    self.position_left = self.position_left.clamp(0, max_left);
                    self.position_top = self.position_top.clamp(0, max_top);

                    self.layer_surface
                        .set_margin(self.position_top, 0, 0, self.position_left);
                    self.layer_surface.commit();

                    // Update the reference only when the surface actually moved,
                    // so sub-threshold motion accumulates instead of being lost.
                    self.grab_position = (pointer_x, pointer_y);
                }
            }
        }

        if left_button_released {
            self.dragging = false;
        }
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
delegate_layer!(App);
