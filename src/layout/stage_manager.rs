//! Stage Manager layout mode (macOS-style).
//!
//! Windows are organized into groups: one **active** group on the stage and up to N **cast**
//! groups shown as single live thumbnails in a vertical strip (macOS-style). Additional groups
//! are kept in **hidden** overflow.

use std::collections::HashMap;
use std::time::Duration;

use niri_config::StageManagerConfig;
use niri_ipc::SizeChange;
use smithay::utils::{Logical, Point, Rectangle, Size};

use super::workspace::Workspace;
use super::LayoutElement;
use crate::utils::id::IdCounter;
use crate::utils::transaction::Transaction;

static GROUP_ID_COUNTER: IdCounter = IdCounter::new();

/// 16:9 default aspect ratio for windows without a saved size.
const ASPECT_RATIO: f64 = 9.0 / 16.0;

/// Gap between the main stage window and the right screen edge.
const STAGE_RIGHT_PADDING: f64 = 2.;

/// Gap between cast strip thumbnails and the left screen edge.
const CAST_STRIP_LEFT_PADDING: f64 = 4.;

/// Hit-test padding around cast thumbnails.
const CAST_HIT_PADDING: f64 = 8.;

/// Maximum windows shown on stage after explicit drag-merge.
const MAX_PARALLEL_STAGE: usize = 2;

/// A group of windows sharing a cast stack slot (typically same app).
#[derive(Debug)]
pub struct StageGroup<W: LayoutElement> {
    pub id: u64,
    /// Windows in this group; front (`[0]`) is shown as the cast thumbnail.
    pub windows: Vec<W::Id>,
}

impl<W: LayoutElement> StageGroup<W> {
    fn new(window: W::Id) -> Self {
        Self {
            id: GROUP_ID_COUNTER.next(),
            windows: vec![window],
        }
    }

    fn contains(&self, id: &W::Id) -> bool {
        self.windows.iter().any(|w| w == id)
    }

    fn remove(&mut self, id: &W::Id) -> bool {
        let len_before = self.windows.len();
        self.windows.retain(|w| w != id);
        self.windows.len() != len_before
    }

    fn bring_to_front(&mut self, id: &W::Id) {
        if let Some(idx) = self.windows.iter().position(|w| w == id) {
            if idx != 0 {
                let win = self.windows.remove(idx);
                self.windows.insert(0, win);
            }
        }
    }
}

/// Layout metadata for a cast thumbnail slot.
#[derive(Debug)]
pub struct CastGroupLayout {
    pub rect: Rectangle<f64, Logical>,
}

/// Tracks stage manager groups and layout state.
#[derive(Debug)]
pub struct StageManagerState<W: LayoutElement> {
    /// Group currently displayed on the stage.
    pub active_group: Option<StageGroup<W>>,
    /// Recently used cast groups (newest first), capped by config.
    pub cast_groups: Vec<StageGroup<W>>,
    /// Groups displaced when cast exceeds `max_cast_groups`.
    pub hidden_groups: Vec<StageGroup<W>>,
    /// Vertical scroll offset when cast groups overflow the strip height.
    pub cast_scroll_offset: f64,
    /// Hovered cast group index.
    pub hovered_cast: Option<usize>,
    /// Cast group index (keyboard focus) tracked for auto-use-as-main.
    interaction_target: Option<usize>,
    /// When [interaction_target] was last set.
    interaction_since: Option<Duration>,
    /// Cached layout for pointer hit-testing.
    pub cast_group_layouts: Vec<CastGroupLayout>,
    /// Windows just added to the cast strip; skip promoting them on the first focus request.
    new_cast_windows: Vec<W::Id>,
}

impl<W: LayoutElement> StageManagerState<W> {
    pub fn new() -> Self {
        Self {
            active_group: None,
            cast_groups: Vec::new(),
            hidden_groups: Vec::new(),
            cast_scroll_offset: 0.,
            hovered_cast: None,
            interaction_target: None,
            interaction_since: None,
            cast_group_layouts: Vec::new(),
            new_cast_windows: Vec::new(),
        }
    }

    pub fn auto_use_as_main_timer_active(&self, config: &StageManagerConfig) -> bool {
        config.auto_use_as_main && self.interaction_target.is_some()
    }

    pub fn clear_auto_use_as_main_timer(&mut self) {
        self.interaction_target = None;
        self.interaction_since = None;
    }

    fn cast_index_for(&self, id: &W::Id) -> Option<usize> {
        self.cast_groups
            .iter()
            .position(|g| g.contains(id))
            .or_else(|| {
                self.hidden_groups
                    .iter()
                    .position(|g| g.contains(id))
                    .map(|idx| self.cast_groups.len() + idx)
            })
    }

    pub fn from_workspace(workspace: &Workspace<W>) -> Self {
        let mut state = Self::new();
        let window_ids: Vec<W::Id> = workspace.windows().map(|w| w.id().clone()).collect();
        if window_ids.is_empty() {
            return state;
        }

        let groups = build_groups_from_windows(workspace, &window_ids);
        let active_id = workspace
            .active_window()
            .map(|w| w.id().clone())
            .unwrap_or_else(|| window_ids[0].clone());

        for group in groups {
            if group.contains(&active_id) {
                state.active_group = Some(StageGroup::new(active_id.clone()));
                let siblings: Vec<W::Id> = group
                    .windows
                    .into_iter()
                    .filter(|w| w != &active_id)
                    .collect();
                if !siblings.is_empty() {
                    state.cast_groups.push(StageGroup {
                        id: group.id,
                        windows: siblings,
                    });
                }
            } else {
                state.cast_groups.push(group);
            }
        }

        if state.active_group.is_none() {
            if !state.cast_groups.is_empty() {
                let group = state.cast_groups.remove(0);
                if let Some(id) = group.windows.first().cloned() {
                    state.active_group = Some(StageGroup::new(id));
                    if group.windows.len() > 1 {
                        state.cast_groups.insert(0, StageGroup {
                            id: group.id,
                            windows: group.windows.into_iter().skip(1).collect(),
                        });
                    }
                }
            }
        }

        state
    }

    pub fn on_window_added(
        &mut self,
        workspace: &Workspace<W>,
        id: W::Id,
        max_cast: usize,
    ) {

        if self.active_group.is_none() && self.cast_groups.is_empty() && self.hidden_groups.is_empty()
        {
            self.active_group = Some(StageGroup::new(id));
            return;
        }

        if self.is_stage_window(&id) {
            if let Some(active) = &mut self.active_group {
                active.bring_to_front(&id);
            }
            return;
        }

        let key = workspace.window_stack_group_key(&id);
        self.new_cast_windows.retain(|w| w != &id);
        self.new_cast_windows.push(id.clone());
        self.insert_into_cast(workspace, id, key);
        self.enforce_cast_limit(max_cast);
    }

    pub fn on_window_removed(&mut self, id: &W::Id, max_cast: usize) {
        self.new_cast_windows.retain(|w| w != id);
        if let Some(group) = &mut self.active_group {
            if group.remove(id) && group.windows.is_empty() {
                self.active_group = None;
            }
        }

        Self::remove_from_group_list(&mut self.cast_groups, id);
        Self::remove_from_group_list(&mut self.hidden_groups, id);

        if self.active_group.is_none() {
            if !self.cast_groups.is_empty() {
                self.active_group = Some(self.cast_groups.remove(0));
            } else if !self.hidden_groups.is_empty() {
                self.active_group = Some(self.hidden_groups.remove(0));
            }
        }

        self.enforce_cast_limit(max_cast);

        if let Some(hovered) = self.hovered_cast {
            if hovered >= self.total_strip_groups() {
                self.hovered_cast = None;
            }
        }
    }

    /// Focus a cast window without promoting it to the stage (used for newly opened windows).
    pub fn on_window_activated(
        &mut self,
        workspace: &mut Workspace<W>,
        id: &W::Id,
        max_cast: usize,
    ) -> bool {

        if self.is_stage_window(id) {
            return false;
        }

        if self.new_cast_windows.iter().any(|w| w == id) {
            self.new_cast_windows.retain(|w| w != id);
            return false;
        }

        self.promote_cast_to_stage(workspace, id, max_cast)
    }

    fn cast_group_front_for(&self, id: &W::Id) -> Option<W::Id> {
        for group in self
            .cast_groups
            .iter()
            .chain(self.hidden_groups.iter())
        {
            if group.contains(id) {
                return group.windows.first().cloned();
            }
        }
        None
    }

    /// Explicitly move a cast thumbnail onto the stage (click, task switcher, etc.).
    pub fn promote_cast_to_stage(
        &mut self,
        workspace: &mut Workspace<W>,
        id: &W::Id,
        max_cast: usize,
    ) -> bool {
        if self.is_stage_window(id) {
            return false;
        }

        let target_id = self.cast_group_front_for(id).unwrap_or_else(|| id.clone());

        self.new_cast_windows.retain(|w| w != &target_id);

        if let Some(cast_idx) = self
            .cast_groups
            .iter()
            .position(|g| g.contains(&target_id))
        {
            self.cast_groups[cast_idx].remove(&target_id);
            if self.cast_groups[cast_idx].windows.is_empty() {
                self.cast_groups.remove(cast_idx);
            }
        } else if let Some(hidden_idx) = self
            .hidden_groups
            .iter()
            .position(|g| g.contains(&target_id))
        {
            self.hidden_groups[hidden_idx].remove(&target_id);
            if self.hidden_groups[hidden_idx].windows.is_empty() {
                self.hidden_groups.remove(hidden_idx);
            }
        } else {
            return false;
        }

        self.set_stage_single(workspace, target_id, max_cast);
        true
    }

    /// Win+G: promote the focused app to sole main, or demote it back to the cast strip.
    pub fn toggle_main(
        &mut self,
        workspace: &mut Workspace<W>,
        id: &W::Id,
        max_cast: usize,
    ) -> bool {
        let target_id = self.cast_group_front_for(id).unwrap_or_else(|| id.clone());

        if self.is_cast_window(&target_id) {
            self.set_stage_single(workspace, target_id, max_cast);
            return true;
        }

        if self.is_stage_window(&target_id) {
            if self.active_group.as_ref().is_some_and(|g| {
                g.windows.len() == 1 && g.windows[0] == target_id
            }) {
                return self.on_window_dragged_to_cast(workspace, target_id, max_cast);
            }

            self.set_stage_single(workspace, target_id, max_cast);
            return true;
        }

        false
    }

    /// Win+Shift+G: show the focused cast app in parallel on stage (max 2).
    pub fn promote_parallel(
        &mut self,
        workspace: &mut Workspace<W>,
        id: W::Id,
        max_cast: usize,
    ) -> bool {
        if self.is_stage_window(&id) {
            return false;
        }

        let target_id = self.cast_group_front_for(&id).unwrap_or(id);

        if !self.is_cast_window(&target_id) {
            return false;
        }

        self.on_window_dragged_to_stage(workspace, target_id, max_cast)
    }

    /// Drag a cast window onto the stage: merge into the active group (max 2 on stage).
    pub fn on_window_dragged_to_stage(
        &mut self,
        workspace: &Workspace<W>,
        id: W::Id,
        max_cast: usize,
    ) -> bool {
        if self.is_stage_window(&id) {
            return false;
        }

        Self::remove_from_group_list(&mut self.cast_groups, &id);
        Self::remove_from_group_list(&mut self.hidden_groups, &id);

        let mut demoted = Vec::new();
        if let Some(active) = &mut self.active_group {
            while active.windows.len() >= MAX_PARALLEL_STAGE {
                demoted.push(active.windows.pop().unwrap());
            }
            if !active.contains(&id) {
                active.windows.insert(0, id);
            }
        } else {
            self.active_group = Some(StageGroup::new(id));
        }

        for win in demoted {
            let key = workspace.window_stack_group_key(&win);
            self.insert_into_cast(workspace, win, key);
        }

        self.enforce_cast_limit(max_cast);
        true
    }

    /// Drag a stage window to the cast strip.
    pub fn on_window_dragged_to_cast(
        &mut self,
        workspace: &mut Workspace<W>,
        id: W::Id,
        max_cast: usize,
    ) -> bool {
        if !self.is_stage_window(&id) {
            return false;
        }

        if let Some(active) = &mut self.active_group {
            active.remove(&id);
            if active.windows.is_empty() {
                self.active_group = None;
            }
        }

        let key = workspace.window_stack_group_key(&id);
        self.insert_into_cast(workspace, id, key);
        self.enforce_cast_limit(max_cast);
        true
    }

    pub fn is_stage_window(&self, id: &W::Id) -> bool {
        self.active_group
            .as_ref()
            .is_some_and(|g| g.contains(id))
    }

    pub fn is_cast_window(&self, id: &W::Id) -> bool {
        self.cast_groups.iter().any(|g| g.contains(id))
            || self.hidden_groups.iter().any(|g| g.contains(id))
    }

    fn group_at_layout_index(&self, idx: usize) -> Option<&StageGroup<W>> {
        if idx < self.cast_groups.len() {
            self.cast_groups.get(idx)
        } else {
            self.hidden_groups.get(idx - self.cast_groups.len())
        }
    }

    fn total_strip_groups(&self) -> usize {
        self.cast_groups.len() + self.hidden_groups.len()
    }

    pub fn active_windows(&self) -> Vec<W::Id> {
        self.active_group
            .as_ref()
            .map(|g| g.windows.clone())
            .unwrap_or_default()
    }

    pub fn set_hovered_cast(&mut self, hovered: Option<usize>) -> bool {
        if self.hovered_cast == hovered {
            return false;
        }
        self.hovered_cast = hovered;
        true
    }

    fn find_cast_group_by_key(&self, workspace: &Workspace<W>, key: &str) -> Option<usize> {
        self.cast_groups.iter().position(|g| {
            g.windows
                .first()
                .is_some_and(|id| workspace.window_stack_group_key(id) == key)
        })
    }

    /// Replace the stage with a single window; demote everything else to cast.
    fn set_stage_single(
        &mut self,
        workspace: &mut Workspace<W>,
        id: W::Id,
        max_cast: usize,
    ) {
        if self.active_group.as_ref().is_some_and(|g| {
            g.windows.len() == 1 && g.windows[0] == id
        }) {
            return;
        }

        Self::remove_from_group_list(&mut self.cast_groups, &id);
        Self::remove_from_group_list(&mut self.hidden_groups, &id);
        self.demote_active_to_cast(workspace);
        Self::remove_from_group_list(&mut self.cast_groups, &id);
        Self::remove_from_group_list(&mut self.hidden_groups, &id);

        self.active_group = Some(StageGroup::new(id));
        self.enforce_cast_limit(max_cast);
    }

    fn insert_into_cast(&mut self, workspace: &Workspace<W>, id: W::Id, key: String) {
        if !workspace.stage_manager_stack_by_app() {
            Self::remove_from_group_list(&mut self.cast_groups, &id);
            Self::remove_from_group_list(&mut self.hidden_groups, &id);
            self.cast_groups.insert(0, StageGroup::new(id));
            return;
        }

        if let Some(idx) = self.find_cast_group_by_key(workspace, &key) {
            let group = &mut self.cast_groups[idx];
            if !group.contains(&id) {
                group.windows.insert(0, id);
            } else {
                group.bring_to_front(&id);
            }
            let group = self.cast_groups.remove(idx);
            self.cast_groups.insert(0, group);
        } else {
            self.cast_groups.insert(0, StageGroup::new(id));
        }
    }

    fn demote_active_to_cast(&mut self, workspace: &Workspace<W>) {
        if let Some(group) = self.active_group.take() {
            for win in group.windows {
                let key = workspace.window_stack_group_key(&win);
                self.insert_into_cast(workspace, win, key);
            }
        }
    }

    fn enforce_cast_limit(&mut self, max_cast: usize) {
        while self.cast_groups.len() > max_cast {
            if let Some(group) = self.cast_groups.pop() {
                self.hidden_groups.insert(0, group);
            }
        }
    }

    fn remove_from_group_list(groups: &mut Vec<StageGroup<W>>, id: &W::Id) {
        groups.retain_mut(|g| {
            g.remove(id);
            !g.windows.is_empty()
        });
    }

    fn all_managed_windows(&self) -> Vec<W::Id> {
        let mut ids = Vec::new();
        if let Some(g) = &self.active_group {
            ids.extend(g.windows.iter().cloned());
        }
        for g in &self.cast_groups {
            ids.extend(g.windows.iter().cloned());
        }
        for g in &self.hidden_groups {
            ids.extend(g.windows.iter().cloned());
        }
        ids
    }
}

fn build_groups_from_windows<W: LayoutElement>(
    workspace: &Workspace<W>,
    window_ids: &[W::Id],
) -> Vec<StageGroup<W>> {
    let mut groups: HashMap<String, StageGroup<W>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for id in window_ids {

        let key = workspace.window_stack_group_key(id);
        if !groups.contains_key(&key) {
            order.push(key.clone());
            groups.insert(key.clone(), StageGroup {
                id: GROUP_ID_COUNTER.next(),
                windows: Vec::new(),
            });
        }
        groups.get_mut(&key).unwrap().windows.insert(0, id.clone());
    }

    order.into_iter().filter_map(|key| groups.remove(&key)).collect()
}

/// Apply the stage manager layout to a workspace.
pub fn apply<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    config: &StageManagerConfig,
    state: &mut StageManagerState<W>,
) {
    reorganize(workspace, state);
    apply_geometry(workspace, config, state);
}

pub fn strip_width_for_config(config: &StageManagerConfig, monitor_width: f64) -> f64 {
    let stage_width = monitor_width * config.proportion - STAGE_RIGHT_PADDING;
    monitor_width - stage_width - STAGE_RIGHT_PADDING
}

pub fn pointer_in_strip_area(
    point: Point<f64, Logical>,
    working_area: Rectangle<f64, Logical>,
    strip_width: f64,
) -> bool {
    let strip_right = working_area.loc.x + strip_width;
    point.x >= working_area.loc.x
        && point.x < strip_right
        && point.y >= working_area.loc.y
        && point.y <= working_area.loc.y + working_area.size.h
}

pub fn pointer_in_stage_area(
    point: Point<f64, Logical>,
    working_area: Rectangle<f64, Logical>,
    strip_width: f64,
) -> bool {
    let stage_left = working_area.loc.x + strip_width;
    point.x >= stage_left
        && point.x <= working_area.loc.x + working_area.size.w
        && point.y >= working_area.loc.y
        && point.y <= working_area.loc.y + working_area.size.h
}

pub fn strip_drag_end<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    config: &StageManagerConfig,
    state: &mut StageManagerState<W>,
    window: &W::Id,
    pointer: Point<f64, Logical>,
) -> bool {
    let working_area = workspace.working_area();
    let strip_w = strip_width_for_config(config, working_area.size.w);

    let changed = if pointer_in_stage_area(pointer, working_area, strip_w) {
        workspace.stage_manager_save_active_sizes();
        state.on_window_dragged_to_stage(workspace, window.clone(), config.max_cast_groups)
    } else if pointer_in_strip_area(pointer, working_area, strip_w) && state.is_stage_window(window)
    {
        workspace.stage_manager_save_active_sizes();
        state.on_window_dragged_to_cast(workspace, window.clone(), config.max_cast_groups)
    } else {
        false
    };

    if let Some(tile) = workspace.tiles_mut().find(|t| t.window().id() == window) {
        tile.interactive_move_offset = Point::from((0., 0.));
    }

    if changed {
        reorganize(workspace, state);
    }
    apply_geometry(workspace, config, state);
    changed
}

pub fn scroll_cast<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    config: &StageManagerConfig,
    state: &mut StageManagerState<W>,
    delta_y: f64,
) -> bool {
    if delta_y == 0. {
        return false;
    }

    let working_area = workspace.working_area();
    let slot_gap = workspace.options.layout.gaps;
    let content_height = cast_content_height(&state.cast_group_layouts, slot_gap);
    let max_scroll = (content_height - working_area.size.h).max(0.);

    let old = state.cast_scroll_offset;
    state.cast_scroll_offset = (state.cast_scroll_offset + delta_y).clamp(0., max_scroll);

    if (state.cast_scroll_offset - old).abs() < f64::EPSILON {
        return false;
    }

    reorganize(workspace, state);
    apply_geometry(workspace, config, state);
    true
}

pub fn pointer_motion<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    config: &StageManagerConfig,
    state: &mut StageManagerState<W>,
    point: Point<f64, Logical>,
) -> bool {
    let working_area = workspace.working_area();
    let strip_w = strip_width_for_config(config, working_area.size.w);

    let hovered = if pointer_in_strip_area(point, working_area, strip_w) {
        hit_test_cast_groups(point, &state.cast_group_layouts)
    } else {
        None
    };

    if !state.set_hovered_cast(hovered) {
        return false;
    }
    reorganize(workspace, state);
    apply_geometry(workspace, config, state);
    true
}

/// Promote a focused cast slot to main after the dwell delay.
pub fn tick_auto_use_as_main<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    config: &StageManagerConfig,
    state: &mut StageManagerState<W>,
    suppress: bool,
) -> bool {
    if !config.auto_use_as_main || suppress {
        let had_state = state.interaction_target.is_some() || state.interaction_since.is_some();
        state.interaction_target = None;
        state.interaction_since = None;
        return had_state;
    }

    let focused_cast = workspace
        .active_window()
        .and_then(|w| state.cast_index_for(w.id()));

    let target = focused_cast;
    let now = workspace.clock.now_unadjusted();
    let mut changed = false;

    if target != state.interaction_target {
        state.interaction_target = target;
        state.interaction_since = target.map(|_| now);
        changed = true;
    }

    let Some(idx) = target else {
        return changed;
    };
    let Some(since) = state.interaction_since else {
        return changed;
    };

    let delay = Duration::from_millis(u64::from(config.auto_use_as_main_delay_ms));
    if now.saturating_sub(since) < delay {
        return changed;
    }

    let Some(id) = state
        .group_at_layout_index(idx)
        .and_then(|g| g.windows.first())
        .cloned()
    else {
        return changed;
    };

    if state.is_stage_window(&id) {
        state.interaction_target = None;
        state.interaction_since = None;
        return changed;
    }

    workspace.stage_manager_save_active_sizes();
    state.set_stage_single(workspace, id, config.max_cast_groups);
    state.interaction_target = None;
    state.interaction_since = None;
    true
}

pub fn disable<W: LayoutElement>(workspace: &mut Workspace<W>) {
    let floating_ids: Vec<W::Id> = workspace
        .floating
        .tiles()
        .map(|t| t.window().id().clone())
        .collect();

    for id in &floating_ids {
        if workspace.floating.has_window(id) {
            workspace.floating.clear_stage_manager_thumb(id);
        }
    }

    for id in floating_ids {
        if workspace.floating.has_window(&id) {
            let removed = workspace.floating.remove_tile(&id);
            workspace.scrolling.add_tile(
                None,
                removed.tile,
                false,
                removed.width,
                removed.is_full_width,
                None,
            );
        }
    }

    workspace.set_floating_inactive();
    workspace.scrolling.set_view_offset_for_stage_manager(0.);
}

fn cast_content_height(layouts: &[CastGroupLayout], slot_gap: f64) -> f64 {
    if layouts.is_empty() {
        return 0.;
    }
    let total: f64 = layouts
        .iter()
        .map(|l| l.rect.size.h + slot_gap)
        .sum();
    total - slot_gap
}

fn hit_test_cast_groups(
    point: Point<f64, Logical>,
    layouts: &[CastGroupLayout],
) -> Option<usize> {
    layouts.iter().enumerate().rev().find_map(|(idx, layout)| {
        let mut rect = layout.rect;
        rect.loc -= Point::from((CAST_HIT_PADDING, CAST_HIT_PADDING));
        rect.size += Size::from((CAST_HIT_PADDING * 2., CAST_HIT_PADDING * 2.));
        rect.contains(point).then_some(idx)
    })
}

fn reorganize<W: LayoutElement>(workspace: &mut Workspace<W>, state: &StageManagerState<W>) {
    for id in state.all_managed_windows() {
        move_to_floating(workspace, &id);
    }
}

fn move_to_floating<W: LayoutElement>(workspace: &mut Workspace<W>, id: &W::Id) {
    if !workspace.floating.has_window(id) && workspace.scrolling.contains_window(id) {
        let mut removed = workspace.scrolling.remove_tile(id, Transaction::new());
        removed.tile.stop_move_animations();
        workspace.floating.add_tile(removed.tile, false);
    }
}

fn apply_geometry<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    config: &StageManagerConfig,
    state: &mut StageManagerState<W>,
) {
    let working_area = workspace.working_area();
    let monitor_width = working_area.size.w;
    let monitor_height = working_area.size.h;

    let stage_width = monitor_width * config.proportion - STAGE_RIGHT_PADDING;
    let strip_w = strip_width_for_config(config, monitor_width);

    let thumb_width =
        (monitor_width * config.thumb_scale).round().max(1.) as i32;
    let thumb_height = (f64::from(thumb_width) * ASPECT_RATIO).round().max(1.) as i32;

    state.cast_group_layouts = apply_cast_strip(
        workspace,
        working_area,
        strip_w,
        thumb_width,
        thumb_height,
        state,
    );

    apply_stage_geometry(
        workspace,
        working_area,
        strip_w,
        stage_width,
        monitor_height,
        state,
    );

    if let Some(hovered) = state.hovered_cast {
        if let Some(id) = state
            .group_at_layout_index(hovered)
            .and_then(|g| g.windows.first())
        {
            workspace.floating.raise_to_top(id);
        }
    }

    if let Some(group) = &state.active_group {
        for id in &group.windows {
            workspace.floating.raise_to_top(id);
        }
    }

    workspace.scrolling.set_view_offset_for_stage_manager(0.);
}

fn apply_cast_strip<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    working_area: Rectangle<f64, Logical>,
    _strip_width: f64,
    thumb_width: i32,
    thumb_height: i32,
    state: &mut StageManagerState<W>,
) -> Vec<CastGroupLayout> {
    let mut layouts = Vec::new();
    let thumb_w = f64::from(thumb_width);
    let thumb_h = f64::from(thumb_height);
    let slot_gap = workspace.options.layout.gaps;

    let x = working_area.loc.x + CAST_STRIP_LEFT_PADDING;
    let mut y_cursor =
        working_area.loc.y + slot_gap - state.cast_scroll_offset;

    for group in &state.cast_groups {
        y_cursor = layout_cast_group_slot(
            workspace,
            thumb_width,
            thumb_height,
            thumb_w,
            thumb_h,
            x,
            y_cursor,
            slot_gap,
            group,
            &mut layouts,
        );
    }

    for group in &state.hidden_groups {
        y_cursor = layout_cast_group_slot(
            workspace,
            thumb_width,
            thumb_height,
            thumb_w,
            thumb_h,
            x,
            y_cursor,
            slot_gap,
            group,
            &mut layouts,
        );
    }

    let content_height = cast_content_height(&layouts, slot_gap);
    let max_scroll = (content_height - working_area.size.h).max(0.);
    state.cast_scroll_offset = state.cast_scroll_offset.clamp(0., max_scroll);

    layouts
}

/// Lay out one cast slot: front window on top, siblings stacked at the same position.
fn layout_cast_group_slot<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    thumb_width: i32,
    thumb_height: i32,
    thumb_w: f64,
    thumb_h: f64,
    x: f64,
    y_cursor: f64,
    slot_gap: f64,
    group: &StageGroup<W>,
    layouts: &mut Vec<CastGroupLayout>,
) -> f64 {
    let pos = Point::from((x, y_cursor));
    let rect = Rectangle::new(pos, Size::from((thumb_w, thumb_h)));

    if let Some(front) = group.windows.first() {
        move_to_floating(workspace, front);
        if !workspace.floating.has_window(front) {
            return y_cursor;
        }

        set_strip_thumb_geometry(workspace, front, thumb_width, thumb_height, pos);
        for id in group.windows.iter().skip(1) {
            move_to_floating(workspace, id);
            if workspace.floating.has_window(id) {
                workspace.floating.set_stage_manager_strip_inert(id);
            }
        }
        workspace.floating.raise_to_top(front);
    }

    layouts.push(CastGroupLayout { rect });
    y_cursor + thumb_h + slot_gap
}

fn set_strip_thumb_geometry<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    id: &W::Id,
    width: i32,
    height: i32,
    pos: Point<f64, Logical>,
) {
    workspace.floating.set_stage_manager_strip_geometry(
        id,
        Size::from((width, height)),
        pos,
        true,
    );
}

fn apply_stage_geometry<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    working_area: Rectangle<f64, Logical>,
    strip_width: f64,
    _stage_width: f64,
    monitor_height: f64,
    state: &StageManagerState<W>,
) {
    let Some(group) = &state.active_group else {
        return;
    };

    let windows = &group.windows;
    let n = windows.len();
    if n == 0 {
        return;
    }

    let gap = workspace.options.layout.gaps;
    let total_gap = gap * (n.saturating_sub(1)) as f64;
    let stage_left = working_area.loc.x + strip_width;
    let stage_right = working_area.loc.x + working_area.size.w - STAGE_RIGHT_PADDING;
    let stage_inner_width = (stage_right - stage_left).max(1.);
    let slot_width = (stage_inner_width - total_gap) / n as f64;

    for (i, id) in windows.iter().enumerate() {
        move_to_floating(workspace, id);
        if !workspace.floating.has_window(id) {
            continue;
        }

        let x = stage_left + i as f64 * (slot_width + gap);
        // Fill each slot to the computed right edge; saved width often leaves a large right gap.
        let window_width = if i + 1 == n {
            (stage_right - x).max(1.)
        } else {
            slot_width
        };

        let saved = workspace.floating.stage_manager_saved_size(id);
        let mut size = resolve_stage_window_size(saved, window_width, monitor_height);
        if saved.is_none() {
            // First time on main: fill to the right edge. Restored windows keep saved width.
            size.w = window_width.round().max(1.) as i32;
        }
        let y = working_area.loc.y + (monitor_height - f64::from(size.h)) / 2.;
        set_stage_window_geometry(workspace, id, size, Point::from((x, y)));
    }
}

fn resolve_stage_window_size(
    saved: Option<Size<i32, Logical>>,
    available_width: f64,
    available_height: f64,
) -> Size<i32, Logical> {
    let default_w = available_width.round().max(1.) as i32;
    let default_h = (f64::from(default_w) * ASPECT_RATIO).round().max(1.) as i32;

    let mut size = saved.unwrap_or_else(|| Size::from((default_w, default_h)));

    size.w = (size.w as f64).min(available_width).round().max(1.) as i32;
    size.h = (size.h as f64)
        .min(available_height)
        .round()
        .max(1.) as i32;

    size
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_stage_window_size_keeps_saved_width() {
        let saved = Size::from((1200, 800));
        let size = resolve_stage_window_size(Some(saved), 1800., 1080.);
        assert_eq!(size.w, 1200);
    }

    #[test]
    fn resolve_stage_window_size_defaults_to_available_width() {
        let size = resolve_stage_window_size(None, 1800., 1080.);
        assert_eq!(size.w, 1800);
    }
}

fn set_stage_window_geometry<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    id: &W::Id,
    size: Size<i32, Logical>,
    pos: Point<f64, Logical>,
) {
    workspace.floating.clear_stage_manager_thumb(id);
    workspace
        .floating
        .set_window_width(Some(id), SizeChange::SetFixed(size.w), true);
    workspace
        .floating
        .set_window_height(Some(id), SizeChange::SetFixed(size.h), true);
    workspace.floating.set_tile_position(id, pos);
}
