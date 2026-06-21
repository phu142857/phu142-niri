//! Full-screen viewport zoom (magnifier) with pointer-follow panning.

use niri_config::ViewportZoom as ViewportZoomConfig;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Size};

use crate::utils::output_size;

/// Screen-edge strip width for macOS Magnifier-style panning (logical px).
const EDGE_PAN_MARGIN: f64 = 96.;

/// How quickly pan approaches the edge target (0–1). Lower is slower.
const EDGE_PAN_SPEED: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportZoomState {
    pub active: bool,
    pub scale: f64,
    pub pan: Point<f64, Logical>,
    /// Pan value when the cursor last entered an edge margin on each axis.
    edge_pan_base: Point<f64, Logical>,
    in_edge_x: bool,
    in_edge_y: bool,
    /// Last edge depth per axis; pan only updates when depth increases (pushing toward edge).
    last_edge_depth_x: f64,
    last_edge_depth_y: f64,
}

impl Default for ViewportZoomState {
    fn default() -> Self {
        Self {
            active: false,
            scale: 1.,
            pan: Point::from((0., 0.)),
            edge_pan_base: Point::from((0., 0.)),
            in_edge_x: false,
            in_edge_y: false,
            last_edge_depth_x: 0.,
            last_edge_depth_y: 0.,
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
            self.in_edge_x = false;
            self.in_edge_y = false;
            self.last_edge_depth_x = 0.;
            self.last_edge_depth_y = 0.;
        } else {
            self.active = true;
            self.scale = config.default_scale.clamp(config.min_scale, config.max_scale);
            self.in_edge_x = false;
            self.in_edge_y = false;
            self.last_edge_depth_x = 0.;
            self.last_edge_depth_y = 0.;
            // Anchor the point under the pointer on activation.
            self.set_pan_full_follow(pointer);
            self.clamp_pan(output);
            if config.edge_pan {
                self.update_pan_edge(output, pointer);
            }
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
        self.in_edge_x = false;
        self.in_edge_y = false;
        self.last_edge_depth_x = 0.;
        self.last_edge_depth_y = 0.;
    }

    fn reset_edge_pan_tracking(&mut self) {
        self.in_edge_x = false;
        self.in_edge_y = false;
        self.last_edge_depth_x = 0.;
        self.last_edge_depth_y = 0.;
    }

    fn update_pan_edge_axis(
        pan: &mut f64,
        edge_pan_base: &mut f64,
        in_edge: &mut bool,
        last_depth: &mut f64,
        pointer_axis: f64,
        depth: f64,
        follow: f64,
    ) {
        if depth == 0. {
            *in_edge = false;
            *last_depth = 0.;
            return;
        }

        if !*in_edge {
            *edge_pan_base = *pan;
            *in_edge = true;
            *last_depth = 0.;
        }

        // Only scroll when pushing deeper toward the screen edge, not when pulling back.
        if depth >= *last_depth {
            let target = pointer_axis * follow;
            let desired = *edge_pan_base + depth * (target - *edge_pan_base);
            *pan += (desired - *pan) * EDGE_PAN_SPEED;
        }

        *last_depth = depth;
    }

    fn follow_factor(&self) -> f64 {
        1. - 1. / self.scale
    }

    fn set_pan_full_follow(&mut self, pointer: Point<f64, Logical>) {
        let follow = self.follow_factor();
        self.pan.x = pointer.x * follow;
        self.pan.y = pointer.y * follow;
    }

    /// How deep the cursor is into the screen-edge strip (0 = inner edge, 1 = screen edge).
    fn edge_depth_x(x: f64, width: f64, margin: f64) -> f64 {
        if x <= margin {
            (margin - x) / margin
        } else if x >= width - margin {
            (x - (width - margin)) / margin
        } else {
            0.
        }
    }

    fn edge_depth_y(y: f64, height: f64, margin: f64) -> f64 {
        Self::edge_depth_x(y, height, margin)
    }

    /// macOS Magnifier-style: frame locked in the center, pan when pushing the cursor to a
    /// screen edge. Pan stays latched when pulling back toward the center.
    fn update_pan_edge(&mut self, output: &Output, pointer: Point<f64, Logical>) {
        let size = output_size(output);
        let follow = self.follow_factor();
        let margin = EDGE_PAN_MARGIN;

        let depth_x = Self::edge_depth_x(pointer.x, size.w, margin);
        let depth_y = Self::edge_depth_y(pointer.y, size.h, margin);

        Self::update_pan_edge_axis(
            &mut self.pan.x,
            &mut self.edge_pan_base.x,
            &mut self.in_edge_x,
            &mut self.last_edge_depth_x,
            pointer.x,
            depth_x,
            follow,
        );
        Self::update_pan_edge_axis(
            &mut self.pan.y,
            &mut self.edge_pan_base.y,
            &mut self.in_edge_y,
            &mut self.last_edge_depth_y,
            pointer.y,
            depth_y,
            follow,
        );

        self.clamp_pan(output);
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
            self.update_pan_edge(output, pointer);
        } else {
            self.set_pan_full_follow(pointer);
            self.clamp_pan(output);
            self.reset_edge_pan_tracking();
        }
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
