//! Full-screen viewport zoom (magnifier) with pointer-follow panning.

use std::time::Duration;

use niri_config::ViewportZoom as ViewportZoomConfig;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Size};

use crate::utils::output_size;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportZoomState {
    pub active: bool,
    pub scale: f64,
    pub pan: Point<f64, Logical>,
    /// Last known pointer position in output-local screen coordinates.
    edge_pointer: Point<f64, Logical>,
    /// Last frame time used by velocity-based edge panning.
    edge_pan_frame: Option<Duration>,
}

impl Default for ViewportZoomState {
    fn default() -> Self {
        Self {
            active: false,
            scale: 1.,
            pan: Point::from((0., 0.)),
            edge_pointer: Point::from((0., 0.)),
            edge_pan_frame: None,
        }
    }
}

impl ViewportZoomState {
    pub fn toggle(
        &mut self,
        config: &ViewportZoomConfig,
        output: &Output,
        pointer: Point<f64, Logical>,
    ) {
        if self.active {
            self.active = false;
            self.scale = 1.;
            self.pan = Point::from((0., 0.));
            self.edge_pan_frame = None;
        } else {
            self.active = true;
            self.scale = config.default_scale.clamp(config.min_scale, config.max_scale);
            self.edge_pointer = pointer;
            self.edge_pan_frame = None;
            // Anchor the point under the pointer on activation.
            self.set_pan_full_follow(pointer);
            self.clamp_pan(output);
        }
    }

    pub fn zoom_in(
        &mut self,
        config: &ViewportZoomConfig,
        output: &Output,
        anchor: Point<f64, Logical>,
    ) {
        if !self.active {
            return;
        }
        let new_scale = (self.scale + config.step).clamp(config.min_scale, config.max_scale);
        self.set_scale_at(output, anchor, new_scale);
    }

    pub fn zoom_out(
        &mut self,
        config: &ViewportZoomConfig,
        output: &Output,
        anchor: Point<f64, Logical>,
    ) {
        if !self.active {
            return;
        }
        let new_scale = (self.scale - config.step).clamp(config.min_scale, config.max_scale);
        self.set_scale_at(output, anchor, new_scale);
    }

    pub fn scroll_zoom(
        &mut self,
        config: &ViewportZoomConfig,
        output: &Output,
        anchor: Point<f64, Logical>,
        delta: f64,
    ) {
        if !self.active || delta == 0. {
            return;
        }
        let change = -delta * config.wheel_step;
        let new_scale = (self.scale + change).clamp(config.min_scale, config.max_scale);
        self.set_scale_at(output, anchor, new_scale);
    }

    fn set_scale_at(&mut self, output: &Output, anchor: Point<f64, Logical>, new_scale: f64) {
        if (new_scale - self.scale).abs() < f64::EPSILON {
            return;
        }
        let world = self.screen_to_world(anchor);
        self.scale = new_scale;
        self.pan.x = world.x - anchor.x / self.scale;
        self.pan.y = world.y - anchor.y / self.scale;
        self.clamp_pan(output);
        self.edge_pan_frame = None;
    }

    fn follow_factor(&self) -> f64 {
        1. - 1. / self.scale
    }

    fn set_pan_full_follow(&mut self, pointer: Point<f64, Logical>) {
        let follow = self.follow_factor();
        self.pan.x = pointer.x * follow;
        self.pan.y = pointer.y * follow;
    }

    /// Signed edge depth: -1 at the leading edge, +1 at the trailing edge, 0 outside the zone.
    fn edge_direction(position: f64, extent: f64, margin: f64) -> f64 {
        let margin = margin.min(extent / 2.);
        if margin <= 0. {
            0.
        } else if position < margin {
            -((margin - position) / margin).clamp(0., 1.)
        } else if position > extent - margin {
            ((position - (extent - margin)) / margin).clamp(0., 1.)
        } else {
            0.
        }
    }

    fn edge_vector(&self, output: &Output, config: &ViewportZoomConfig) -> Point<f64, Logical> {
        let size = output_size(output);
        Point::from((
            Self::edge_direction(self.edge_pointer.x, size.w, config.edge_margin),
            Self::edge_direction(self.edge_pointer.y, size.h, config.edge_margin),
        ))
    }

    /// With `edge_pan` off the view follows the cursor. With `edge_pan` on the magnified
    /// frame stays locked in the center; pan only when pushing the cursor to a screen edge.
    pub fn update_pan_for_pointer(
        &mut self,
        config: &ViewportZoomConfig,
        output: &Output,
        pointer: Point<f64, Logical>,
    ) {
        if !self.active {
            return;
        }

        if config.edge_pan {
            let was_at_edge = self.edge_vector(output, config) != Point::from((0., 0.));
            self.edge_pointer = pointer;
            let at_edge = self.edge_vector(output, config) != Point::from((0., 0.));
            if !at_edge || !was_at_edge {
                self.edge_pan_frame = None;
            }
        } else {
            self.set_pan_full_follow(pointer);
            self.clamp_pan(output);
            self.edge_pan_frame = None;
        }
    }

    /// Advance edge panning using elapsed time rather than input-event frequency.
    pub fn advance_edge_pan(
        &mut self,
        config: &ViewportZoomConfig,
        output: &Output,
        now: Duration,
    ) -> bool {
        if !self.active || !config.edge_pan {
            self.edge_pan_frame = None;
            return false;
        }

        let direction = self.edge_vector(output, config);
        if direction == Point::from((0., 0.)) {
            self.edge_pan_frame = None;
            return false;
        }

        let Some(previous) = self.edge_pan_frame.replace(now) else {
            return false;
        };
        let dt = now.saturating_sub(previous).as_secs_f64().min(0.05);
        if dt == 0. {
            return false;
        }

        let old = self.pan;
        let world_speed = config.edge_pan_speed / self.scale;
        self.pan.x += direction.x * world_speed * dt;
        self.pan.y += direction.y * world_speed * dt;
        self.clamp_pan(output);
        self.pan != old
    }

    pub fn edge_pan_ongoing(&self, config: &ViewportZoomConfig, output: &Output) -> bool {
        if !self.active || !config.edge_pan {
            return false;
        }

        let direction = self.edge_vector(output, config);
        let size = output_size(output);
        let max_x = (size.w - size.w / self.scale).max(0.);
        let max_y = (size.h - size.h / self.scale).max(0.);
        (direction.x < 0. && self.pan.x > 0.)
            || (direction.x > 0. && self.pan.x < max_x)
            || (direction.y < 0. && self.pan.y > 0.)
            || (direction.y > 0. && self.pan.y < max_y)
    }

    fn clamp_pan(&mut self, output: &Output) {
        let size = output_size(output);
        let view_w = size.w / self.scale;
        let view_h = size.h / self.scale;
        let max_x = (size.w - view_w).max(0.);
        let max_y = (size.h - view_h).max(0.);
        self.pan.x = self.pan.x.clamp(0., max_x);
        self.pan.y = self.pan.y.clamp(0., max_y);
    }

    pub fn screen_to_world(&self, screen: Point<f64, Logical>) -> Point<f64, Logical> {
        if !self.active {
            return screen;
        }
        Point::from((
            self.pan.x + screen.x / self.scale,
            self.pan.y + screen.y / self.scale,
        ))
    }

    pub fn world_to_screen(&self, world: Point<f64, Logical>) -> Point<f64, Logical> {
        if !self.active {
            return world;
        }
        Point::from((
            (world.x - self.pan.x) * self.scale,
            (world.y - self.pan.y) * self.scale,
        ))
    }

    /// Smithay's pointer focus accepts only a translated surface origin, while viewport zoom also
    /// scales coordinates. Return a per-motion synthetic origin that makes
    /// `screen_pointer - focus_origin` equal the correct unscaled surface-local position.
    pub fn pointer_focus_origin(
        &self,
        screen_pointer: Point<f64, Logical>,
        world_surface_origin: Point<f64, Logical>,
    ) -> Point<f64, Logical> {
        let world_pointer = self.screen_to_world(screen_pointer);
        screen_pointer - (world_pointer - world_surface_origin)
    }

    pub fn transform_geo(&self, geo: Rectangle<f64, Logical>) -> Rectangle<f64, Logical> {
        if !self.active {
            return geo;
        }
        Rectangle::new(
            Point::from((
                (geo.loc.x - self.pan.x) * self.scale,
                (geo.loc.y - self.pan.y) * self.scale,
            )),
            Size::from((geo.size.w * self.scale, geo.size.h * self.scale)),
        )
    }

    pub fn multiply_zoom(&self, zoom: f64) -> f64 {
        if self.active {
            zoom * self.scale
        } else {
            zoom
        }
    }
}

#[cfg(test)]
mod tests {
    use smithay::output::{Mode, PhysicalProperties, Subpixel};

    use super::*;

    fn output() -> Output {
        let output = Output::new(
            "zoom-test".into(),
            PhysicalProperties {
                size: Size::from((1000, 800)),
                subpixel: Subpixel::Unknown,
                make: String::new(),
                model: String::new(),
                serial_number: String::new(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: Size::from((1000, 800)),
                refresh: 60_000,
            }),
            None,
            None,
            None,
        );
        output
    }

    #[test]
    fn screen_and_world_coordinates_round_trip() {
        let mut state = ViewportZoomState::default();
        state.active = true;
        state.scale = 2.;
        state.pan = Point::from((125., 80.));
        let screen = Point::from((640., 360.));
        assert_eq!(state.world_to_screen(state.screen_to_world(screen)), screen);
    }

    #[test]
    fn pointer_focus_origin_preserves_unscaled_surface_local_coordinates() {
        let mut state = ViewportZoomState::default();
        state.active = true;
        state.scale = 2.;
        state.pan = Point::from((125., 80.));
        let screen_pointer = Point::from((640., 360.));
        let surface_world_origin = Point::from((300., 150.));
        let focus_origin = state.pointer_focus_origin(screen_pointer, surface_world_origin);

        assert_eq!(
            screen_pointer - focus_origin,
            state.screen_to_world(screen_pointer) - surface_world_origin
        );
    }

    #[test]
    fn edge_pan_advances_by_elapsed_time() {
        let output = output();
        let config = ViewportZoomConfig {
            edge_pan: true,
            edge_margin: 100.,
            edge_pan_speed: 1000.,
            default_scale: 2.,
            ..Default::default()
        };
        let mut state = ViewportZoomState::default();
        state.toggle(&config, &output, Point::from((500., 400.)));
        state.update_pan_for_pointer(&config, &output, Point::from((1000., 400.)));

        assert!(!state.advance_edge_pan(&config, &output, Duration::ZERO));
        let before = state.pan.x;
        assert!(state.advance_edge_pan(&config, &output, Duration::from_millis(16)));
        assert!((state.pan.x - before - 8.).abs() < 0.001);
    }
}
