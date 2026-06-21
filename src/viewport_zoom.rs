//! Full-screen viewport zoom (magnifier) with realtime mouse panning.

use niri_config::ViewportZoom as ViewportZoomConfig;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Size};

use crate::utils::output_size;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportZoomState {
    pub active: bool,
    pub scale: f64,
    pub pan: Point<f64, Logical>,
}

impl Default for ViewportZoomState {
    fn default() -> Self {
        Self {
            active: false,
            scale: 1.,
            pan: Point::from((0., 0.)),
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
        } else {
            self.active = true;
            self.scale = config.default_scale.clamp(config.min_scale, config.max_scale);
            self.pan = Point::from((0., 0.));
            self.update_pan_for_pointer(output, pointer);
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
    }

    /// Keep the viewport centered on the pointer (realtime panning).
    pub fn update_pan_for_pointer(&mut self, output: &Output, pointer: Point<f64, Logical>) {
        if !self.active {
            return;
        }
        let size = output_size(output);
        self.pan.x = pointer.x / self.scale - size.w / (2. * self.scale);
        self.pan.y = pointer.y / self.scale - size.h / (2. * self.scale);
        self.clamp_pan(output);
    }

    fn clamp_pan(&mut self, output: &Output) {
        let size = output_size(output);
        let max_x = (size.w * (1. - 1. / self.scale)).max(0.);
        let max_y = (size.h * (1. - 1. / self.scale)).max(0.);
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

    /// Apply viewport transform to a layout geometry rectangle.
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
