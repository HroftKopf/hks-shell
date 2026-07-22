use std::ffi::c_void;
use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    delegate_keyboard, delegate_layer, delegate_pointer, delegate_registry, delegate_seat,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
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

use crate::renderer::{BAR_H, GlassParams, RESULTS_TOP, ROW_H, Renderer};
use crate::search::{Search, SearchResult};

/// Max result rows shown (scroll comes later).
const MAX_ROWS: usize = 10;

/// Smooth caret opacity (0..1): mostly solid, with one smooth dip to 0 per
/// period (fade out, fade in, then hold solid before the next dip).
fn caret_alpha(elapsed: Duration) -> f32 {
    let period = 1.2_f32;
    let x = (elapsed.as_secs_f32() / period).fract();
    let dip = smoothstep(0.40, 0.50, x) - smoothstep(0.50, 0.60, x);
    (1.0 - dip).clamp(0.0, 1.0)
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

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
    /// Reference time for the caret blink phase (reset on keystroke).
    pub caret_clock: Instant,

    pub search: Search,
    pub results: Vec<SearchResult>,
    pub selected: usize,
    /// Index of the first visible result row (scroll position).
    pub scroll: usize,

    pub renderer: Option<Renderer>,

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
                    if self.selected < self.scroll {
                        self.scroll = self.selected;
                    }
                    self.refresh();
                }
            }
            Keysym::Down => {
                if self.selected + 1 < self.results.len() {
                    self.selected += 1;
                    if self.selected >= self.scroll + MAX_ROWS {
                        self.scroll = self.selected + 1 - MAX_ROWS;
                    }
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
        self.scroll = 0;
        self.caret_clock = Instant::now(); // caret solid right after typing
        self.sync_panel();
    }

    /// Update the caret opacity from the blink phase (called each frame tick).
    pub fn update_caret(&mut self) {
        let alpha = caret_alpha(self.caret_clock.elapsed());
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_caret_alpha(alpha);
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
        let scroll_max = self.results.len().saturating_sub(MAX_ROWS);
        self.scroll = self.scroll.min(scroll_max);
        let scroll = self.scroll;

        let mut titles = Vec::new();
        let mut subtitles = Vec::new();
        let mut icons = Vec::new();
        for result in self.results.iter().skip(scroll).take(MAX_ROWS) {
            titles.push(result.title.clone());
            subtitles.push(result.subtitle.clone().unwrap_or_default());
            icons.push(result.icon.clone());
        }
        let visible = titles.len();
        let selection = if visible > 0 && self.selected >= scroll && self.selected < scroll + visible {
            Some(self.selected - scroll)
        } else {
            None
        };
        // Scrollbar thumb geometry (logical px); hidden when everything fits.
        let total = self.results.len();
        let (sb_top, sb_h) = if total > MAX_ROWS {
            let track_top = RESULTS_TOP;
            let track_h = ((self.surface_height as f32 - 8.0) - track_top).max(1.0);
            let thumb_h = (MAX_ROWS as f32 / total as f32 * track_h).max(24.0);
            let frac = (scroll as f32 / (total - MAX_ROWS) as f32).clamp(0.0, 1.0);
            (track_top + frac * (track_h - thumb_h), thumb_h)
        } else {
            (0.0, 0.0)
        };

        let caret = caret_alpha(self.caret_clock.elapsed());
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_text(&text, placeholder);
            renderer.set_results(&titles, &subtitles, &icons);
            renderer.set_selection(selection);
            renderer.set_caret_alpha(caret);
            renderer.set_scrollbar(sb_top, sb_h);
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
        let mut scroll_delta: i32 = 0;

        for event in events {
            if &event.surface != self.layer_surface.wl_surface() {
                continue;
            }

            if let PointerEventKind::Axis { vertical, .. } = &event.kind {
                // value120 is the modern high-res field (deprecated `discrete`
                // is 0 on niri); fall back to the pixel amount.
                let step = if vertical.value120 != 0 {
                    vertical.value120.signum()
                } else if vertical.discrete != 0 {
                    vertical.discrete.signum()
                } else if vertical.absolute > 0.5 {
                    1
                } else if vertical.absolute < -0.5 {
                    -1
                } else {
                    0
                };
                scroll_delta += step * 3; // rows per notch
            }
        }

        if scroll_delta != 0 && self.results.len() > MAX_ROWS {
            self.scroll = (self.scroll as i64 + scroll_delta as i64).max(0) as usize;
            self.refresh();
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
