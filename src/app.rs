use std::ffi::c_void;

use smithay_client_toolkit::{
    delegate_keyboard, delegate_layer, delegate_pointer, delegate_registry, delegate_seat,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{BTN_LEFT, PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    },
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    protocol::{wl_compositor, wl_keyboard, wl_pointer, wl_seat, wl_surface},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use crate::renderer::{BAR_H, GlassParams, ROW_H, Renderer};
use crate::search::{Search, SearchResult};

/// Max result rows shown (scroll comes later).
const MAX_ROWS: usize = 10;

// Current output: 3440x1440 @ scale 1.2 -> logical 2867x1200.
// Layer Shell works in logical coordinates.
pub const OUTPUT_WIDTH: i32 = 2867;
pub const OUTPUT_HEIGHT: i32 = 1200;

pub struct App {
    pub registry_state: RegistryState,
    pub seat_state: SeatState,

    pub layer_surface: LayerSurface,
    pub viewport: WpViewport,
    pub pointer: Option<wl_pointer::WlPointer>,
    pub keyboard: Option<wl_keyboard::WlKeyboard>,

    /// Current search query text.
    pub query: String,
    /// Blinking caret visibility.
    pub cursor_on: bool,

    pub search: Search,
    pub results: Vec<SearchResult>,
    pub selected: usize,

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

    /// Apply a key press to the search query. Shared by press and key-repeat.
    fn handle_key(&mut self, event: &KeyEvent) {
        match event.keysym {
            Keysym::Escape => {
                // For now Escape closes the launcher; later it will just hide it.
                self.running = false;
            }
            Keysym::Return | Keysym::KP_Enter => {
                if let Some(result) = self.results.get(self.selected) {
                    result.action.run();
                }
                self.running = false;
            }
            Keysym::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.refresh();
                }
            }
            Keysym::Down => {
                let visible = self.results.len().min(MAX_ROWS);
                if visible > 0 && self.selected + 1 < visible {
                    self.selected += 1;
                    self.refresh();
                }
            }
            Keysym::BackSpace => {
                self.query.pop();
                self.on_query_changed();
            }
            _ => {
                if let Some(text) = &event.utf8 {
                    if !text.is_empty() && !text.chars().any(char::is_control) {
                        self.query.push_str(text);
                        self.on_query_changed();
                    }
                }
            }
        }
    }

    /// Re-run the search after the query changed, resize the panel and redraw.
    fn on_query_changed(&mut self) {
        self.results = self.search.query(&self.query);
        self.selected = 0;
        self.cursor_on = true; // keep the caret solid right after typing
        self.sync_panel();
    }

    /// Flip the caret (called on the blink timer) and redraw.
    pub fn toggle_cursor(&mut self) {
        self.cursor_on = !self.cursor_on;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_caret(self.cursor_on);
            renderer.render();
        }
    }

    /// Height the panel needs to show the current results (grows downward).
    fn target_height(&self) -> i32 {
        let rows = self.results.len().min(MAX_ROWS);
        if rows == 0 {
            BAR_H as i32
        } else {
            (BAR_H + rows as f32 * ROW_H + 16.0) as i32
        }
    }

    /// Resize the panel to fit the results, or just redraw if the height is
    /// unchanged. On resize the compositor's configure drives the redraw.
    fn sync_panel(&mut self) {
        let target = self.target_height();
        if target != self.surface_height {
            self.surface_height = target;
            self.layer_surface
                .set_size(self.surface_width as u32, target as u32);
            self.layer_surface.commit();
        } else {
            self.refresh();
        }
    }

    /// Push the current query + results to the renderer and redraw.
    fn refresh(&mut self) {
        let (text, placeholder) = if self.query.is_empty() {
            ("Search".to_string(), true)
        } else {
            (self.query.clone(), false)
        };
        let mut titles = Vec::new();
        let mut subtitles = Vec::new();
        let mut icons = Vec::new();
        for result in self.results.iter().take(MAX_ROWS) {
            titles.push(result.title.clone());
            subtitles.push(result.subtitle.clone().unwrap_or_default());
            icons.push(result.icon.clone());
        }
        let visible = titles.len();
        let selection = if visible == 0 {
            None
        } else {
            Some(self.selected.min(visible - 1))
        };
        let cursor_on = self.cursor_on;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_text(&text, placeholder);
            renderer.set_results(&titles, &subtitles, &icons);
            renderer.set_selection(selection);
            renderer.set_caret(cursor_on);
            renderer.render();
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
        connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _layer: &LayerSurface,
        _configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        self.ensure_renderer(connection);
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.resize(self.surface_width as u32, self.surface_height as u32);
        }
        // Update the viewport destination together with the resized buffer (the
        // render below commits both atomically), avoiding a stretched frame.
        self.viewport
            .set_destination(self.surface_width, self.surface_height);
        self.refresh();
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
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let keyboard = self
                .seat_state
                .get_keyboard(queue_handle, &seat, None)
                .expect("failed to obtain Wayland keyboard");
            self.keyboard = Some(keyboard);
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
        if capability == Capability::Keyboard {
            if let Some(keyboard) = self.keyboard.take() {
                keyboard.release();
            }
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

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.handle_key(&event);
    }

    fn repeat_key(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.handle_key(&event);
    }

    fn release_key(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
    }
}

impl Dispatch<WpViewporter, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &WpViewporter,
        _event: <WpViewporter as Proxy>::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpViewport, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &WpViewport,
        _event: <WpViewport as Proxy>::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
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
delegate_keyboard!(App);
delegate_layer!(App);
