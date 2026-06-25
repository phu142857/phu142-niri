//! Keyboard-driven pointer control built into niri (warpd-like normal mode).

use niri_config::{
    CornerRadius, Gradient, KeyboardPointer as KeyboardPointerConfig, Modifiers,
};
use smithay::backend::input::{Axis, AxisSource, ButtonState};
use smithay::desktop::utils::bbox_from_surface_tree;
use smithay::input::pointer::{AxisFrame, ButtonEvent};
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Size, SERIAL_COUNTER};
use smithay::input::keyboard::Keysym;

use crate::cursor::{RenderCursor, XCursor};
use crate::niri::{KeyboardFocus, Niri, State};
use crate::render_helpers::border::BorderRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::utils::get_monotonic_time;

const TICK_MS: u64 = 10;
/// Scroll step per tick in logical pixels.
const SCROLL_STEP: f64 = 4.;
/// Mouse wheel notch size, used only to compute matching v120 values.
const WHEEL_NOTCH: f64 = 15.;
/// Padding between the cursor and the keyboard-pointer indicator ring.
const INDICATOR_PADDING: f64 = 4.;
/// Indicator ring stroke width.
const INDICATOR_BORDER_WIDTH: f32 = 1.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardPointerKeyResult {
    Continue,
    Exit,
    Click(u32),
    ButtonPress(u32),
    ButtonRelease(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyboardPointerState {
    pub active: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    scroll_left: bool,
    scroll_right: bool,
    scroll_up: bool,
    scroll_down: bool,
    dragging: bool,
    velocity: f64,
}

impl Default for KeyboardPointerState {
    fn default() -> Self {
        Self {
            active: false,
            left: false,
            right: false,
            up: false,
            down: false,
            scroll_left: false,
            scroll_right: false,
            scroll_up: false,
            scroll_down: false,
            dragging: false,
            velocity: 0.,
        }
    }
}

impl KeyboardPointerState {
    pub fn handle_key(
        &mut self,
        keysym: Keysym,
        pressed: bool,
        modifiers: Modifiers,
        config: &KeyboardPointerConfig,
    ) -> KeyboardPointerKeyResult {
        if keysym == Keysym::Escape && pressed {
            return KeyboardPointerKeyResult::Exit;
        }

        if pressed {
            match keysym {
                Keysym::Return if modifiers.contains(Modifiers::SHIFT) => {
                    return KeyboardPointerKeyResult::Click(0x111);
                }
                Keysym::Return => return KeyboardPointerKeyResult::Click(0x110),
                Keysym::space if !self.dragging => {
                    self.dragging = true;
                    return KeyboardPointerKeyResult::ButtonPress(0x110);
                }
                _ => (),
            }
        } else if keysym == Keysym::space && self.dragging {
            self.dragging = false;
            return KeyboardPointerKeyResult::ButtonRelease(0x110);
        }

        match keysym {
            Keysym::a | Keysym::A => self.left = pressed,
            Keysym::d | Keysym::D => self.right = pressed,
            Keysym::w | Keysym::W => self.up = pressed,
            Keysym::s | Keysym::S => self.down = pressed,
            Keysym::j | Keysym::J => self.scroll_left = pressed,
            Keysym::l | Keysym::L => self.scroll_right = pressed,
            Keysym::i | Keysym::I => self.scroll_up = pressed,
            Keysym::k | Keysym::K => self.scroll_down = pressed,
            _ => (),
        }

        if pressed && (self.left || self.right || self.up || self.down) {
            self.velocity = config.speed / 1000.;
        }

        KeyboardPointerKeyResult::Continue
    }

    /// Returns cursor delta in logical pixels for this tick, if any movement is needed.
    pub fn tick(&mut self, config: &KeyboardPointerConfig, dt_ms: f64) -> Option<Point<f64, Logical>> {
        let dx = f64::from(self.right as u8) - f64::from(self.left as u8);
        let dy = f64::from(self.down as u8) - f64::from(self.up as u8);
        if dx == 0. && dy == 0. {
            self.velocity = config.speed / 1000.;
            return None;
        }

        let len = (dx * dx + dy * dy).sqrt();
        let step = self.velocity * dt_ms;
        self.velocity = (self.velocity + config.acceleration / 1_000_000. * dt_ms)
            .min(config.max_speed / 1000.);

        Some(Point::from((dx / len * step, dy / len * step)))
    }

    /// Returns scroll delta in logical pixels for this tick, if any scrolling is needed.
    pub fn scroll_tick(
        &self,
        _config: &KeyboardPointerConfig,
        dt_ms: f64,
    ) -> Option<Point<f64, Logical>> {
        let dx = f64::from(self.scroll_right as u8) - f64::from(self.scroll_left as u8);
        let dy = f64::from(self.scroll_down as u8) - f64::from(self.scroll_up as u8);
        if dx == 0. && dy == 0. {
            return None;
        }

        let len = (dx * dx + dy * dy).sqrt();
        let step = SCROLL_STEP * (dt_ms / TICK_MS as f64);
        Some(Point::from((dx / len * step, dy / len * step)))
    }
}

impl State {
    pub fn toggle_keyboard_pointer(&mut self) {
        if self.niri.keyboard_pointer.active {
            self.deactivate_keyboard_pointer();
        } else {
            self.activate_keyboard_pointer();
        }
    }

    pub fn activate_keyboard_pointer(&mut self) {
        if self.niri.keyboard_pointer.active {
            return;
        }

        self.niri.keyboard_pointer = KeyboardPointerState {
            active: true,
            velocity: self.niri.config.borrow().keyboard_pointer.speed / 1000.,
            ..KeyboardPointerState::default()
        };
        self.niri.pointer_visibility = crate::niri::PointerVisibility::Visible;
        self.start_keyboard_pointer_timer();
        self.update_keyboard_focus();
        self.niri.queue_redraw_all();
    }

    pub fn deactivate_keyboard_pointer(&mut self) {
        if !self.niri.keyboard_pointer.active {
            return;
        }

        if self.niri.keyboard_pointer.dragging {
            self.keyboard_pointer_button(0x110, ButtonState::Released);
        }

        self.niri.keyboard_pointer = KeyboardPointerState::default();
        if let Some(token) = self.niri.keyboard_pointer_timer.take() {
            self.niri.event_loop.remove(token);
        }
        self.update_keyboard_focus();
        self.niri.queue_redraw_all();
    }

    pub fn keyboard_pointer_handle_key(
        &mut self,
        keysym: Keysym,
        pressed: bool,
        modifiers: Modifiers,
    ) -> KeyboardPointerKeyResult {
        let config = self.niri.config.borrow().keyboard_pointer;
        let result = self
            .niri
            .keyboard_pointer
            .handle_key(keysym, pressed, modifiers, &config);
        match result {
            KeyboardPointerKeyResult::Exit => self.deactivate_keyboard_pointer(),
            KeyboardPointerKeyResult::Click(button) => {
                self.keyboard_pointer_click(button);
            }
            KeyboardPointerKeyResult::ButtonPress(button) => {
                self.keyboard_pointer_button(button, ButtonState::Pressed);
            }
            KeyboardPointerKeyResult::ButtonRelease(button) => {
                self.keyboard_pointer_button(button, ButtonState::Released);
            }
            _ => (),
        }
        result
    }

    fn keyboard_pointer_button(&mut self, button: u32, state: ButtonState) {
        let pointer = self.niri.seat.get_pointer().unwrap();
        let serial = SERIAL_COUNTER.next_serial();
        let time = get_monotonic_time().as_millis() as u32;
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state,
                serial,
                time,
            },
        );
        pointer.frame(self);
        self.niri.queue_redraw_all();
    }

    fn keyboard_pointer_click(&mut self, button: u32) {
        let pointer = self.niri.seat.get_pointer().unwrap();
        let serial = SERIAL_COUNTER.next_serial();
        let time = get_monotonic_time().as_millis() as u32;
        for state in [ButtonState::Pressed, ButtonState::Released] {
            pointer.button(
                self,
                &ButtonEvent {
                    button,
                    state,
                    serial,
                    time,
                },
            );
        }
        pointer.frame(self);
        self.niri.queue_redraw_all();
    }

    fn start_keyboard_pointer_timer(&mut self) {
        if let Some(token) = self.niri.keyboard_pointer_timer.take() {
            self.niri.event_loop.remove(token);
        }

        let duration = std::time::Duration::from_millis(TICK_MS);
        let timer = smithay::reexports::calloop::timer::Timer::from_duration(duration);
        let token = self
            .niri
            .event_loop
            .insert_source(timer, move |_, _, state| {
                state.keyboard_pointer_tick();
                smithay::reexports::calloop::timer::TimeoutAction::ToDuration(duration)
            })
            .unwrap();
        self.niri.keyboard_pointer_timer = Some(token);
    }

    fn keyboard_pointer_tick(&mut self) {
        if !self.niri.keyboard_pointer.active {
            return;
        }

        let config = self.niri.config.borrow().keyboard_pointer;
        if let Some(delta) = self
            .niri
            .keyboard_pointer
            .tick(&config, TICK_MS as f64)
        {
            let pointer = self.niri.seat.get_pointer().unwrap();
            let mut pos = pointer.current_location();
            pos += delta;

            if let Some(bounds) = self.keyboard_pointer_bounds() {
                let bounds = bounds.to_f64();
                pos.x = pos.x.clamp(bounds.loc.x, bounds.loc.x + bounds.size.w - 1.);
                pos.y = pos.y.clamp(bounds.loc.y, bounds.loc.y + bounds.size.h - 1.);
            }

            self.move_cursor(pos);
        }

        if let Some(scroll) = self
            .niri
            .keyboard_pointer
            .scroll_tick(&config, TICK_MS as f64)
        {
            self.keyboard_pointer_scroll(scroll.x, scroll.y);
        }
    }

    fn keyboard_pointer_scroll(&mut self, horizontal: f64, vertical: f64) {
        if horizontal == 0. && vertical == 0. {
            return;
        }

        self.update_pointer_contents();

        let config = self.niri.config.borrow();
        let (h_factor, v_factor) = config
            .input
            .mouse
            .scroll_factor
            .map(|x| x.h_v_factors())
            .unwrap_or((1.0, 1.0));
        drop(config);

        let horizontal = horizontal * h_factor;
        let vertical = vertical * v_factor;

        if vertical != 0. {
            let pointer = self.niri.seat.get_pointer().unwrap();
            let pos = pointer.current_location();
            let stage_scroll = self
                .niri
                .output_under(pos)
                .map(|(output, pos_within_output)| (output.clone(), pos_within_output));
            if let Some((output, pos_within_output)) = stage_scroll {
                if self.niri.layout.stage_manager_scroll(
                    &output,
                    pos_within_output,
                    vertical,
                ) {
                    self.niri.queue_redraw_all();
                    return;
                }
            }
        }

        let pointer = self.niri.seat.get_pointer().unwrap();
        let time = get_monotonic_time().as_millis() as u32;
        let mut frame = AxisFrame::new(time).source(AxisSource::Wheel);

        if horizontal != 0. {
            let v120 = ((horizontal / WHEEL_NOTCH) * 120.).round() as i32;
            let v120 = if v120 == 0 {
                horizontal.signum() as i32 * 32
            } else {
                v120
            };
            frame = frame.value(Axis::Horizontal, horizontal);
            frame = frame.v120(Axis::Horizontal, v120);
        }
        if vertical != 0. {
            let v120 = ((vertical / WHEEL_NOTCH) * 120.).round() as i32;
            let v120 = if v120 == 0 {
                vertical.signum() as i32 * 32
            } else {
                v120
            };
            frame = frame.value(Axis::Vertical, vertical);
            frame = frame.v120(Axis::Vertical, v120);
        }

        pointer.axis(self, frame);
        pointer.frame(self);
    }

    fn keyboard_pointer_bounds(&self) -> Option<Rectangle<i32, Logical>> {
        self.niri.global_space.outputs().fold(
            None,
            |acc: Option<Rectangle<i32, Logical>>, output| {
                self.niri
                    .global_space
                    .output_geometry(output)
                    .map(|geo| acc.map(|acc| acc.merge(geo)).unwrap_or(geo))
            },
        )
    }
}

impl KeyboardFocus {
    pub fn is_keyboard_pointer(&self) -> bool {
        matches!(self, KeyboardFocus::KeyboardPointer)
    }
}

fn cursor_bbox(
    render_cursor: &RenderCursor,
    hotspot_pos: Point<f64, Logical>,
    cursor_time_ms: u32,
) -> Rectangle<f64, Logical> {
    match render_cursor {
        RenderCursor::Hidden => Rectangle::new(
            hotspot_pos - Point::from((8., 8.)),
            Size::from((16., 16.)),
        ),
        RenderCursor::Surface { hotspot, surface } => {
            let loc = hotspot_pos - hotspot.to_f64();
            let bbox = bbox_from_surface_tree(surface, loc.to_i32_round());
            Rectangle::new(loc, Size::from((bbox.size.w as f64, bbox.size.h as f64)))
        }
        RenderCursor::Named { scale, cursor, .. } => {
            let (_, frame) = cursor.frame(cursor_time_ms);
            let hotspot = XCursor::hotspot(frame).to_logical(*scale);
            let size = Size::from((
                frame.width as f64 / f64::from(*scale),
                frame.height as f64 / f64::from(*scale),
            ));
            Rectangle::new(hotspot_pos - hotspot.to_f64(), size)
        }
    }
}

/// Draws a circular ring around the cursor while keyboard-pointer mode is active.
pub fn render_indicator<R: NiriRenderer>(
    niri: &Niri,
    renderer: &mut R,
    output: &Output,
    hotspot_pos: Point<f64, Logical>,
    render_cursor: &RenderCursor,
    cursor_time_ms: u32,
    push: &mut dyn FnMut(BorderRenderElement),
) {
    if !niri.keyboard_pointer.active {
        return;
    }

    if !BorderRenderElement::has_shader(renderer) {
        return;
    }

    let bbox = cursor_bbox(render_cursor, hotspot_pos, cursor_time_ms);
    let center = hotspot_pos;
    let max_dist = [
        bbox.loc,
        Point::from((bbox.loc.x + bbox.size.w, bbox.loc.y)),
        Point::from((bbox.loc.x, bbox.loc.y + bbox.size.h)),
        Point::from((bbox.loc.x + bbox.size.w, bbox.loc.y + bbox.size.h)),
    ]
    .iter()
    .map(|corner| {
        let dx = corner.x - hotspot_pos.x;
        let dy = corner.y - hotspot_pos.y;
        (dx * dx + dy * dy).sqrt()
    })
    .fold(0., f64::max);
    let radius = max_dist + INDICATOR_PADDING;
    let diameter = radius * 2.;
    let ring_loc = Point::from((center.x - radius, center.y - radius));
    let ring_size = Size::from((diameter, diameter));

    let scale = output.current_scale().fractional_scale();
    let color = niri.config.borrow().layout.focus_ring.active_color;
    let corner_radius =
        CornerRadius::from((radius as f32).min(diameter as f32 / 2.)).fit_to(diameter as f32, diameter as f32);
    let gradient = Gradient::from(color);

    let elem = BorderRenderElement::new(
        ring_size,
        Rectangle::new(Point::default(), ring_size),
        gradient.in_,
        color,
        color,
        0.,
        Rectangle::new(Point::default(), ring_size),
        INDICATOR_BORDER_WIDTH,
        corner_radius,
        scale as f32,
        1.,
    )
    .with_location(ring_loc);

    push(elem);
}
