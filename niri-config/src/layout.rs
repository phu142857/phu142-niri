use knuffel::errors::DecodeError;
use niri_ipc::{ColumnDisplay, SizeChange};

use crate::appearance::{
    Border, FocusRing, InsertHint, Shadow, TabIndicator, DEFAULT_BACKGROUND_COLOR,
};
use crate::utils::{expect_only_children, Flag, MergeWith};
use crate::{BorderRule, Color, FloatOrInt, InsertHintPart, ShadowRule, TabIndicatorPart};

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub focus_ring: FocusRing,
    pub border: Border,
    pub shadow: Shadow,
    pub tab_indicator: TabIndicator,
    pub insert_hint: InsertHint,
    pub preset_column_widths: Vec<PresetSize>,
    pub default_column_width: Option<PresetSize>,
    pub preset_window_heights: Vec<PresetSize>,
    pub center_focused_column: CenterFocusedColumn,
    pub always_center_single_column: bool,
    pub empty_workspace_above_first: bool,
    pub default_column_display: ColumnDisplay,
    pub gaps: f64,
    pub struts: Struts,
    pub background_color: Color,
    pub stage_manager: Option<StageManagerConfig>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            focus_ring: FocusRing::default(),
            border: Border::default(),
            shadow: Shadow::default(),
            tab_indicator: TabIndicator::default(),
            insert_hint: InsertHint::default(),
            preset_column_widths: vec![
                PresetSize::Proportion(1. / 3.),
                PresetSize::Proportion(0.5),
                PresetSize::Proportion(2. / 3.),
            ],
            default_column_width: Some(PresetSize::Proportion(0.5)),
            center_focused_column: CenterFocusedColumn::Never,
            always_center_single_column: false,
            empty_workspace_above_first: false,
            default_column_display: ColumnDisplay::Normal,
            gaps: 16.,
            struts: Struts::default(),
            preset_window_heights: vec![
                PresetSize::Proportion(1. / 3.),
                PresetSize::Proportion(0.5),
                PresetSize::Proportion(2. / 3.),
            ],
            background_color: DEFAULT_BACKGROUND_COLOR,
            stage_manager: None,
        }
    }
}

impl MergeWith<LayoutPart> for Layout {
    fn merge_with(&mut self, part: &LayoutPart) {
        merge!(
            (self, part),
            focus_ring,
            border,
            shadow,
            tab_indicator,
            insert_hint,
            always_center_single_column,
            empty_workspace_above_first,
            gaps,
        );

        merge_clone!(
            (self, part),
            preset_column_widths,
            preset_window_heights,
            center_focused_column,
            default_column_display,
            struts,
            background_color,
        );

        if let Some(x) = part.default_column_width {
            self.default_column_width = x.0;
        }

        if self.preset_column_widths.is_empty() {
            self.preset_column_widths = Layout::default().preset_column_widths;
        }

        if self.preset_window_heights.is_empty() {
            self.preset_window_heights = Layout::default().preset_window_heights;
        }

        if let Some(part) = &part.stage_manager {
            self.stage_manager = Some(StageManagerConfig::from_part(part));
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct LayoutPart {
    #[knuffel(child)]
    pub focus_ring: Option<BorderRule>,
    #[knuffel(child)]
    pub border: Option<BorderRule>,
    #[knuffel(child)]
    pub shadow: Option<ShadowRule>,
    #[knuffel(child)]
    pub tab_indicator: Option<TabIndicatorPart>,
    #[knuffel(child)]
    pub insert_hint: Option<InsertHintPart>,
    #[knuffel(child, unwrap(children))]
    pub preset_column_widths: Option<Vec<PresetSize>>,
    #[knuffel(child)]
    pub default_column_width: Option<DefaultPresetSize>,
    #[knuffel(child, unwrap(children))]
    pub preset_window_heights: Option<Vec<PresetSize>>,
    #[knuffel(child, unwrap(argument))]
    pub center_focused_column: Option<CenterFocusedColumn>,
    #[knuffel(child)]
    pub always_center_single_column: Option<Flag>,
    #[knuffel(child)]
    pub empty_workspace_above_first: Option<Flag>,
    #[knuffel(child, unwrap(argument, str))]
    pub default_column_display: Option<ColumnDisplay>,
    #[knuffel(child, unwrap(argument))]
    pub gaps: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child)]
    pub struts: Option<Struts>,
    #[knuffel(child)]
    pub background_color: Option<Color>,
    #[knuffel(child)]
    pub stage_manager: Option<StageManagerPart>,
}

/// Where the cast stack strip is placed relative to the stage.
#[derive(knuffel::DecodeScalar, Debug, Default, PartialEq, Eq, Clone, Copy)]
pub enum StackPosition {
    /// Vertical cast strip on the left (default).
    #[default]
    Left,
    /// Vertical cast strip on the right.
    Right,
    /// Horizontal cast strip at the top.
    Top,
    /// Horizontal cast strip at the bottom.
    Bottom,
}

impl StackPosition {
    pub fn is_vertical(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
}

/// Stage Manager layout mode configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct StageManagerConfig {
    /// Where the cast stack strip is placed.
    pub stack_position: StackPosition,
    /// Fraction of the screen width occupied by the stage area (0.1–0.9).
    pub proportion: f64,
    /// Maximum number of app groups visible in the cast strip (overflow goes hidden).
    pub max_cast_groups: usize,
    /// Cast thumbnail scale relative to monitor width (0.1–0.3).
    pub thumb_scale: f64,
    /// Group cast strip slots by app ID (macOS-style). When false, each window gets its own slot.
    pub stack_by_app: bool,
    /// After focusing a cast strip slot, promote it to main (keeps saved window size).
    pub auto_use_as_main: bool,
    /// Dwell time before auto-use-as-main triggers (milliseconds).
    pub auto_use_as_main_delay_ms: u32,
}

impl Default for StageManagerConfig {
    fn default() -> Self {
        Self {
            stack_position: StackPosition::default(),
            proportion: 0.7,
            max_cast_groups: 6,
            thumb_scale: 0.15,
            stack_by_app: false,
            auto_use_as_main: false,
            auto_use_as_main_delay_ms: 2000,
        }
    }
}

impl StageManagerConfig {
    fn from_part(part: &StageManagerPart) -> Self {
        let proportion = part
            .proportion
            .map(|p| p.0)
            .unwrap_or(Self::default().proportion);
        let max_cast_groups = part
            .max_cast_groups
            .map(|n| n.0 as usize)
            .unwrap_or(Self::default().max_cast_groups);
        let thumb_scale = part
            .thumb_scale
            .map(|p| p.0)
            .unwrap_or(Self::default().thumb_scale);
        let stack_by_app = part
            .stack_by_app
            .map(|f| f.0)
            .unwrap_or(Self::default().stack_by_app);
        let auto_use_as_main = part
            .auto_use_as_main
            .map(|f| f.0)
            .unwrap_or(Self::default().auto_use_as_main);
        let auto_use_as_main_delay_ms = part
            .auto_use_as_main_delay_ms
            .map(|n| n.0 as u32)
            .unwrap_or(Self::default().auto_use_as_main_delay_ms);
        let stack_position = part
            .stack_position
            .unwrap_or(Self::default().stack_position);
        Self {
            stack_position,
            proportion: proportion.clamp(0.1, 0.9),
            max_cast_groups: max_cast_groups.clamp(1, 12),
            thumb_scale: thumb_scale.clamp(0.1, 0.3),
            stack_by_app,
            auto_use_as_main,
            auto_use_as_main_delay_ms: auto_use_as_main_delay_ms.clamp(0, 60_000),
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct StageManagerPart {
    #[knuffel(child, unwrap(argument))]
    pub stack_position: Option<StackPosition>,
    #[knuffel(child, unwrap(argument))]
    pub proportion: Option<FloatOrInt<0, 1>>,
    #[knuffel(child, unwrap(argument))]
    pub max_cast_groups: Option<FloatOrInt<1, 12>>,
    #[knuffel(child, unwrap(argument))]
    pub thumb_scale: Option<FloatOrInt<0, 1>>,
    #[knuffel(child)]
    pub stack_by_app: Option<crate::utils::Flag>,
    #[knuffel(child)]
    pub auto_use_as_main: Option<crate::utils::Flag>,
    #[knuffel(child, unwrap(argument))]
    pub auto_use_as_main_delay_ms: Option<FloatOrInt<0, 60000>>,
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub enum PresetSize {
    Proportion(#[knuffel(argument)] f64),
    Fixed(#[knuffel(argument)] i32),
}

impl From<PresetSize> for SizeChange {
    fn from(value: PresetSize) -> Self {
        match value {
            PresetSize::Proportion(prop) => SizeChange::SetProportion(prop * 100.),
            PresetSize::Fixed(fixed) => SizeChange::SetFixed(fixed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DefaultPresetSize(pub Option<PresetSize>);

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct Struts {
    #[knuffel(child, unwrap(argument), default)]
    pub left: FloatOrInt<-65535, 65535>,
    #[knuffel(child, unwrap(argument), default)]
    pub right: FloatOrInt<-65535, 65535>,
    #[knuffel(child, unwrap(argument), default)]
    pub top: FloatOrInt<-65535, 65535>,
    #[knuffel(child, unwrap(argument), default)]
    pub bottom: FloatOrInt<-65535, 65535>,
}

#[derive(knuffel::DecodeScalar, Debug, Default, PartialEq, Eq, Clone, Copy)]
pub enum CenterFocusedColumn {
    /// Focusing a column will not center the column.
    #[default]
    Never,
    /// The focused column will always be centered.
    Always,
    /// Focusing a column will center it if it doesn't fit on the screen together with the
    /// previously focused column.
    OnOverflow,
}

impl<S> knuffel::Decode<S> for DefaultPresetSize
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        expect_only_children(node, ctx);

        let mut children = node.children();

        if let Some(child) = children.next() {
            if let Some(unwanted_child) = children.next() {
                ctx.emit_error(DecodeError::unexpected(
                    unwanted_child,
                    "node",
                    "expected no more than one child",
                ));
            }
            PresetSize::decode_node(child, ctx).map(Some).map(Self)
        } else {
            Ok(Self(None))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn stage_manager_parses() {
        let config = Config::parse_mem(
            r#"
            layout {
                stage-manager {
                    proportion 0.7
                }
            }
            "#,
        )
        .unwrap();

        let sm = config.layout.stage_manager.unwrap();
        assert!((sm.proportion - 0.7).abs() < f64::EPSILON);
        assert_eq!(sm.max_cast_groups, 6);
        assert!((sm.thumb_scale - 0.15).abs() < f64::EPSILON);
        assert!(!sm.stack_by_app);
        assert!(!sm.auto_use_as_main);
        assert_eq!(sm.auto_use_as_main_delay_ms, 2000);
    }

    #[test]
    fn stage_manager_stack_by_app_parses() {
        let config = Config::parse_mem(
            r#"
            layout {
                stage-manager {
                    stack-by-app true
                }
            }
            "#,
        )
        .unwrap();

        assert!(config.layout.stage_manager.unwrap().stack_by_app);
    }

    #[test]
    fn stage_manager_proportion_is_clamped() {
        let config = Config::parse_mem(
            r#"
            layout {
                stage-manager {
                    proportion 0.05
                }
            }
            "#,
        )
        .unwrap();

        let sm = config.layout.stage_manager.unwrap();
        assert!((sm.proportion - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn stage_manager_stack_position_parses() {
        for (value, expected) in [
            ("left", StackPosition::Left),
            ("right", StackPosition::Right),
            ("top", StackPosition::Top),
            ("bottom", StackPosition::Bottom),
        ] {
            let config = Config::parse_mem(&format!(
                r#"
                layout {{
                    stage-manager {{
                        stack-position "{value}"
                    }}
                }}
                "#
            ))
            .unwrap();

            assert_eq!(
                config.layout.stage_manager.unwrap().stack_position,
                expected
            );
        }
    }
}
