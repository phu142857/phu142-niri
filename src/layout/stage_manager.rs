//! Stage Manager layout mode (macOS-style).
//!
//! Windows are organized into groups: one **active** group on the stage and up to N **cast**
//! groups shown as single live thumbnails in a strip (macOS-style). The strip can be placed on
//! any screen edge via `stack-position`. Additional groups are kept in **hidden** overflow.

use std::collections::HashMap;
use std::time::Duration;

use niri_config::{StackPosition, StageManagerConfig};
use niri_ipc::SizeChange;
use smithay::utils::{Logical, Point, Rectangle, Size};

use super::workspace::Workspace;
use super::LayoutElement;
use crate::utils::id::IdCounter;
use crate::utils::transaction::Transaction;

static GROUP_ID_COUNTER: IdCounter = IdCounter::new();

/// Padding between layout areas and the screen edges.
const STAGE_EDGE_PADDING: f64 = 2.;

/// Inset of cast thumbnails within the stack area.
const STACK_INSET: f64 = STAGE_EDGE_PADDING;

/// Hit-test padding around cast thumbnails.
const CAST_HIT_PADDING: f64 = 8.;

/// Mouse wheel scroll speed multiplier for the cast stack.
const STACK_SCROLL_MOUSE_FACTOR: f64 = 3.0;

/// Maximum windows shown on stage after explicit drag-merge.
const MAX_PARALLEL_STAGE: usize = 2;

/// Directional focus request from keyboard navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusDirection {
    Up,
    Down,
    Left,
    Right,
}

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
    /// Cast windows that skip immediate promotion on the first explicit activation (click).
    /// Cleared on passive focus or when the auto-use-as-main dwell timer starts.
    new_cast_windows: Vec<W::Id>,
    /// Stage window temporarily removed from the layout during an interactive move.
    interactive_move_stage: Option<W::Id>,
    /// Dialog on the stage → its parent parked in the cast strip until the dialog closes.
    stage_dialog_parents: Vec<(W::Id, W::Id)>,
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
            interactive_move_stage: None,
            stage_dialog_parents: Vec::new(),
        }
    }

    pub fn begin_interactive_move_stage(&mut self, id: W::Id) {
        self.interactive_move_stage = Some(id);
        self.clear_auto_use_as_main_timer();
        self.clear_pointer_hover();
    }

    pub fn end_interactive_move_stage(&mut self) {
        self.interactive_move_stage = None;
        self.clear_auto_use_as_main_timer();
    }

    pub fn is_interactive_move_of(&self, id: &W::Id) -> bool {
        self.interactive_move_stage.as_ref() == Some(id)
    }

    fn should_defer_promote(&self, id: &W::Id) -> bool {
        self.new_cast_windows.iter().any(|w| w == id)
    }

    /// Drop the one-shot click deferral for a cast window (does not promote).
    pub fn consume_promote_defer(&mut self, id: &W::Id) {
        self.new_cast_windows.retain(|w| w != id);
    }

    /// Cast received focus without an explicit activation request (e.g. focus-follows-mouse).
    pub fn on_cast_focused_passive(&mut self, id: &W::Id) {
        if self.is_cast_window(id) {
            let target_id = self.cast_group_front_for(id).unwrap_or_else(|| id.clone());
            self.consume_promote_defer(&target_id);
        }
    }

    pub fn auto_use_as_main_timer_active(&self, config: &StageManagerConfig) -> bool {
        config.auto_use_as_main && self.interaction_target.is_some()
    }

    pub fn clear_auto_use_as_main_timer(&mut self) {
        self.interaction_target = None;
        self.interaction_since = None;
    }

    pub fn clear_pointer_hover(&mut self) {
        self.hovered_cast = None;
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

    /// Whether [id] is a dialog opening on a window currently on the stage.
    pub fn is_stage_child_of_active(
        &self,
        workspace: &Workspace<W>,
        id: &W::Id,
    ) -> bool {
        self.stage_parent_on_stage(workspace, id).is_some()
    }

    pub fn is_stage_dialog(&self, id: &W::Id) -> bool {
        self.stage_dialog_parents
            .iter()
            .any(|(dialog, _)| dialog == id)
    }

    fn stage_parent_on_stage(&self, workspace: &Workspace<W>, id: &W::Id) -> Option<W::Id> {
        let child = workspace.windows().find(|w| w.id() == id)?;
        let active = self.active_group.as_ref()?;
        for parent_id in &active.windows {
            if parent_id == id {
                continue;
            }
            let Some(parent) = workspace.windows().find(|w| w.id() == parent_id) else {
                continue;
            };
            if child.is_child_of(parent) {
                return Some(parent_id.clone());
            }
        }
        None
    }

    fn stage_parent_of(&self, workspace: &Workspace<W>, id: &W::Id) -> Option<W::Id> {
        let child = workspace.windows().find(|w| w.id() == id)?;
        for parent_id in self.all_managed_windows() {
            if &parent_id == id {
                continue;
            }
            let Some(parent) = workspace.windows().find(|w| w.id() == &parent_id) else {
                continue;
            };
            if child.is_child_of(parent) {
                return Some(parent_id);
            }
        }
        None
    }

    /// Topmost modal overlay child of [parent_id] on the stage, if any.
    pub fn stage_modal_overlay_for_parent(
        &self,
        workspace: &Workspace<W>,
        parent_id: &W::Id,
    ) -> Option<W::Id> {
        let active = self.active_group.as_ref()?;
        active
            .windows
            .iter()
            .rev()
            .find(|child_id| self.stage_parent_of(workspace, child_id).as_ref() == Some(parent_id))
            .cloned()
    }

    pub fn has_stage_modal_overlay(&self, _workspace: &Workspace<W>) -> bool {
        !self.stage_dialog_parents.is_empty()
    }

    /// Whether [parent_id] is a stage primary window that should not receive hits at [pos].
    pub fn stage_parent_hit_blocked_by_modal(
        &self,
        workspace: &Workspace<W>,
        parent_id: &W::Id,
        tile_hits: &impl Fn(&W::Id) -> bool,
    ) -> bool {
        self.stage_modal_overlay_for_parent(workspace, parent_id)
            .is_some_and(|overlay_id| tile_hits(&overlay_id))
    }

    /// Redirect activation from a stage parent to its open modal overlay.
    pub fn resolve_activation_target(
        &self,
        workspace: &Workspace<W>,
        id: &W::Id,
    ) -> W::Id {
        self.stage_modal_overlay_for_parent(workspace, id)
            .unwrap_or_else(|| id.clone())
    }

    /// A child window gained an xdg parent after mapping.
    pub fn on_parent_changed(
        &mut self,
        workspace: &mut Workspace<W>,
        id: W::Id,
        max_cast: usize,
    ) -> bool {
        let Some(parent_id) = self.stage_parent_of(workspace, &id) else {
            return false;
        };
        if !self.is_stage_window(&parent_id) && !self.is_cast_window(&parent_id) {
            return false;
        }
        self.open_stage_dialog(workspace, id, parent_id, max_cast);
        true
    }

    /// Park the parent in the cast strip and show the dialog alone on the stage.
    fn open_stage_dialog(
        &mut self,
        workspace: &mut Workspace<W>,
        dialog_id: W::Id,
        parent_id: W::Id,
        max_cast: usize,
    ) {
        self.new_cast_windows.retain(|w| w != &dialog_id);
        Self::remove_from_group_list(&mut self.cast_groups, &dialog_id);
        Self::remove_from_group_list(&mut self.hidden_groups, &dialog_id);

        if self.is_stage_window(&parent_id) {
            workspace
                .floating
                .park_stage_position_for_cast(&parent_id, true);
            if let Some(active) = &mut self.active_group {
                active.remove(&parent_id);
                if active.windows.is_empty() {
                    self.active_group = None;
                }
            }
            let key = workspace.window_stack_group_key(&parent_id);
            self.insert_into_cast(workspace, parent_id.clone(), key);
        }

        self.stage_dialog_parents
            .retain(|(_, parent)| parent != &parent_id);
        self.stage_dialog_parents
            .push((dialog_id.clone(), parent_id));

        Self::remove_from_group_list(&mut self.cast_groups, &dialog_id);
        Self::remove_from_group_list(&mut self.hidden_groups, &dialog_id);
        self.active_group = Some(StageGroup::new(dialog_id));
        self.enforce_cast_limit(max_cast);
    }

    pub(super) fn raise_stage_z_order(
        &self,
        workspace: &mut Workspace<W>,
        active: Option<&W::Id>,
    ) {
        raise_stage_group_z_order(workspace, self, active);
    }

    pub fn on_window_added(
        &mut self,
        workspace: &mut Workspace<W>,
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

        if let Some(parent_id) = self.stage_parent_on_stage(workspace, &id) {
            self.open_stage_dialog(workspace, id, parent_id, max_cast);
            return;
        }

        let key = workspace.window_stack_group_key(&id);
        self.new_cast_windows.retain(|w| w != &id);
        self.new_cast_windows.push(id.clone());
        self.insert_into_cast(workspace, id, key);
        self.enforce_cast_limit(max_cast);
    }

    /// Returns the parent window to restore when a stage dialog closes.
    pub fn on_window_removed(
        &mut self,
        workspace: &mut Workspace<W>,
        id: &W::Id,
        max_cast: usize,
    ) -> Option<W::Id> {
        if self.interactive_move_stage.as_ref() == Some(id) {
            return None;
        }

        let parent_to_restore = self
            .stage_dialog_parents
            .iter()
            .find(|(dialog, _)| dialog == id)
            .map(|(_, parent)| parent.clone());
        self.stage_dialog_parents.retain(|(dialog, _)| dialog != id);

        self.new_cast_windows.retain(|w| w != id);
        if let Some(group) = &mut self.active_group {
            if group.remove(id) && group.windows.is_empty() {
                self.active_group = None;
            }
        }

        Self::remove_from_group_list(&mut self.cast_groups, id);
        Self::remove_from_group_list(&mut self.hidden_groups, id);

        if let Some(parent_id) = parent_to_restore {
            Self::remove_from_group_list(&mut self.cast_groups, &parent_id);
            Self::remove_from_group_list(&mut self.hidden_groups, &parent_id);
            workspace.floating.restore_stage_saved_position(&parent_id);
            self.active_group = Some(StageGroup::new(parent_id.clone()));
            self.enforce_cast_limit(max_cast);
            return Some(parent_id);
        }

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

        None
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

        let target_id = self.cast_group_front_for(id).unwrap_or_else(|| id.clone());

        if self.should_defer_promote(&target_id) {
            self.consume_promote_defer(&target_id);
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
        workspace: &mut Workspace<W>,
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
            workspace.floating.park_stage_position_for_cast(&win, false);
            let key = workspace.window_stack_group_key(&win);
            self.insert_into_cast(workspace, win, key);
        }

        if self.active_group.as_ref().is_some_and(|g| g.windows.len() == MAX_PARALLEL_STAGE) {
            for win in &self.active_group.as_ref().unwrap().windows {
                workspace.floating.clear_stage_manager_default_layout(win);
            }
            self.clear_auto_use_as_main_timer();
            self.clear_pointer_hover();
        }

        self.enforce_cast_limit(max_cast);
        true
    }

    /// Move a window onto the cast strip (e.g. after a cross-monitor interactive move).
    pub fn force_window_to_cast(
        &mut self,
        workspace: &mut Workspace<W>,
        id: W::Id,
        max_cast: usize,
    ) -> bool {
        self.stage_dialog_parents.retain(|(dialog, _)| dialog != &id);
        self.new_cast_windows.retain(|w| w != &id);

        if self.is_stage_window(&id) {
            let save_pos = self.active_group.as_ref().is_some_and(|g| {
                g.windows.len() == 1 && g.windows.contains(&id)
            });
            workspace
                .floating
                .park_stage_position_for_cast(&id, save_pos);
            if let Some(active) = &mut self.active_group {
                active.remove(&id);
                if active.windows.is_empty() {
                    self.active_group = None;
                }
            }
        }

        Self::remove_from_group_list(&mut self.cast_groups, &id);
        Self::remove_from_group_list(&mut self.hidden_groups, &id);

        let key = workspace.window_stack_group_key(&id);
        self.insert_into_cast(workspace, id, key);
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

        let was_parallel = self
            .active_group
            .as_ref()
            .is_some_and(|g| g.windows.len() > 1);

        workspace
            .floating
            .park_stage_position_for_cast(&id, !was_parallel);

        if let Some(active) = &mut self.active_group {
            active.remove(&id);
            if active.windows.is_empty() {
                self.active_group = None;
            }
        }

        if was_parallel {
            reset_managed_layout_defaults(workspace, self);
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

    pub fn stage_has_main(&self) -> bool {
        self.active_group
            .as_ref()
            .is_some_and(|g| !g.windows.is_empty())
    }

    pub fn is_cast_window(&self, id: &W::Id) -> bool {
        self.cast_groups.iter().any(|g| g.contains(id))
            || self.hidden_groups.iter().any(|g| g.contains(id))
    }

    /// Two or more primary (non-dialog) windows share the main stage.
    pub fn parallel_stage_active(&self, workspace: &Workspace<W>) -> bool {
        let Some(group) = &self.active_group else {
            return false;
        };
        group
            .windows
            .iter()
            .filter(|id| !is_stage_child_overlay(workspace, self, id))
            .count()
            >= MAX_PARALLEL_STAGE
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

        let was_parallel = self
            .active_group
            .as_ref()
            .is_some_and(|g| g.windows.len() > 1);

        Self::remove_from_group_list(&mut self.cast_groups, &id);
        Self::remove_from_group_list(&mut self.hidden_groups, &id);
        if was_parallel {
            reset_managed_layout_defaults(workspace, self);
        }
        self.demote_active_to_cast(workspace, !was_parallel);
        Self::remove_from_group_list(&mut self.cast_groups, &id);
        Self::remove_from_group_list(&mut self.hidden_groups, &id);

        if !was_parallel {
            workspace.floating.restore_stage_saved_position(&id);
        }
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

    fn demote_active_to_cast(&mut self, workspace: &mut Workspace<W>, save_pos: bool) {
        if let Some(group) = self.active_group.take() {
            for win in group.windows {
                workspace
                    .floating
                    .park_stage_position_for_cast(&win, save_pos);
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

/// Stack and stage regions derived from config.
#[derive(Debug, Clone, Copy)]
struct StageAreas {
    stack_area: Rectangle<f64, Logical>,
    stage_area: Rectangle<f64, Logical>,
}

fn compute_areas(
    working_area: Rectangle<f64, Logical>,
    config: &StageManagerConfig,
) -> StageAreas {
    let loc = working_area.loc;
    let w = working_area.size.w;
    let h = working_area.size.h;
    let pad = STAGE_EDGE_PADDING;

    match config.stack_position {
        StackPosition::Left => {
            let stage_w = (config.target_stage_width() - pad)
                .min(w - pad * 2.)
                .max(1.);
            let stack_w = w - stage_w - pad;
            StageAreas {
                stack_area: Rectangle::new(loc, Size::from((stack_w, h))),
                stage_area: Rectangle::new(
                    Point::from((loc.x + stack_w, loc.y)),
                    Size::from((w - stack_w - pad, h)),
                ),
            }
        }
        StackPosition::Right => {
            let stage_w = (config.target_stage_width() - pad)
                .min(w - pad * 2.)
                .max(1.);
            let stack_w = w - stage_w - pad;
            StageAreas {
                stack_area: Rectangle::new(
                    Point::from((loc.x + w - stack_w, loc.y)),
                    Size::from((stack_w, h)),
                ),
                stage_area: Rectangle::new(loc, Size::from((w - stack_w - pad, h))),
            }
        }
        StackPosition::Top => {
            let stage_h = (config.target_stage_height() - pad)
                .min(h - pad * 2.)
                .max(1.);
            let stack_h = h - stage_h - pad;
            StageAreas {
                stack_area: Rectangle::new(loc, Size::from((w, stack_h))),
                stage_area: Rectangle::new(
                    Point::from((loc.x, loc.y + stack_h)),
                    Size::from((w, h - stack_h - pad)),
                ),
            }
        }
        StackPosition::Bottom => {
            let stage_h = (config.target_stage_height() - pad)
                .min(h - pad * 2.)
                .max(1.);
            let stack_h = h - stage_h - pad;
            StageAreas {
                stack_area: Rectangle::new(
                    Point::from((loc.x, loc.y + h - stack_h)),
                    Size::from((w, stack_h)),
                ),
                stage_area: Rectangle::new(loc, Size::from((w, h - stack_h - pad))),
            }
        }
    }
}

fn cast_stack_window_ids<W: LayoutElement>(state: &StageManagerState<W>) -> Vec<W::Id> {
    state
        .cast_groups
        .iter()
        .chain(state.hidden_groups.iter())
        .filter_map(|group| group.windows.first().cloned())
        .collect()
}

fn focus_cast_stack_neighbor<W: LayoutElement>(
    state: &StageManagerState<W>,
    config: &StageManagerConfig,
    current: &W::Id,
    direction: FocusDirection,
) -> Option<W::Id> {
    let target_id = state.cast_group_front_for(current).unwrap_or_else(|| current.clone());
    let windows = cast_stack_window_ids(state);
    let idx = windows.iter().position(|id| id == &target_id)?;

    let delta = match (config.stack_position, direction) {
        (StackPosition::Left | StackPosition::Right, FocusDirection::Up) => -1,
        (StackPosition::Left | StackPosition::Right, FocusDirection::Down) => 1,
        (StackPosition::Top | StackPosition::Bottom, FocusDirection::Left) => -1,
        (StackPosition::Top | StackPosition::Bottom, FocusDirection::Right) => 1,
        _ => return None,
    };

    let new_idx = idx.checked_add_signed(delta)?;
    windows.get(new_idx).cloned()
}

/// Try stage-manager-specific focus moves that keep stack navigation off the main stage.
pub fn try_focus_neighbor<W: LayoutElement>(
    workspace: &Workspace<W>,
    state: &StageManagerState<W>,
    config: &StageManagerConfig,
    direction: FocusDirection,
) -> Option<W::Id> {
    let active = workspace.active_window()?.id();

    if state.is_cast_window(active) {
        return focus_cast_stack_neighbor(state, config, active, direction);
    }

    if state.is_stage_window(active) {
        let allowed = state.active_windows();
        let distance = match direction {
            FocusDirection::Up => {
                |focus: Point<f64, Logical>, other: Point<f64, Logical>| focus.y - other.y
            }
            FocusDirection::Down => {
                |focus: Point<f64, Logical>, other: Point<f64, Logical>| other.y - focus.y
            }
            FocusDirection::Left => {
                |focus: Point<f64, Logical>, other: Point<f64, Logical>| focus.x - other.x
            }
            FocusDirection::Right => {
                |focus: Point<f64, Logical>, other: Point<f64, Logical>| other.x - focus.x
            }
        };
        return workspace.floating.find_directional_among(active, &allowed, distance);
    }

    None
}

pub fn pointer_in_stack_area(
    point: Point<f64, Logical>,
    working_area: Rectangle<f64, Logical>,
    config: &StageManagerConfig,
) -> bool {
    compute_areas(working_area, config)
        .stack_area
        .contains(point)
}

pub fn pointer_in_stage_area(
    point: Point<f64, Logical>,
    working_area: Rectangle<f64, Logical>,
    config: &StageManagerConfig,
) -> bool {
    compute_areas(working_area, config)
        .stage_area
        .contains(point)
}

/// Place a window dropped from another monitor onto the stage or cast strip.
pub fn cross_monitor_move_finish<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    config: &StageManagerConfig,
    state: &mut StageManagerState<W>,
    window: &W::Id,
) -> bool {
    if state.active_group.as_ref().is_some_and(|g| {
        g.windows.len() == 1 && g.windows.first() == Some(window)
    }) {
        return false;
    }

    workspace.stage_manager_save_active_sizes();
    let changed = if state.stage_has_main() {
        state.force_window_to_cast(workspace, window.clone(), config.max_cast_groups)
    } else {
        state.set_stage_single(workspace, window.clone(), config.max_cast_groups);
        true
    };

    if changed {
        reorganize(workspace, state);
        apply_geometry(workspace, config, state);
    }
    changed
}

pub fn strip_drag_end<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    config: &StageManagerConfig,
    state: &mut StageManagerState<W>,
    window: &W::Id,
    pointer: Point<f64, Logical>,
) -> bool {
    let working_area = workspace.working_area();

    let changed = if pointer_in_stage_area(pointer, working_area, config) {
        workspace.stage_manager_save_active_sizes();
        state.on_window_dragged_to_stage(workspace, window.clone(), config.max_cast_groups)
    } else if pointer_in_stack_area(pointer, working_area, config) && state.is_stage_window(window)
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
    let areas = compute_areas(working_area, config);
    let slot_gap = workspace.options.layout.gaps;
    let content_extent =
        cast_content_extent(&state.cast_group_layouts, slot_gap, config.stack_position.is_vertical());
    let viewport_extent = if config.stack_position.is_vertical() {
        areas.stack_area.size.h
    } else {
        areas.stack_area.size.w
    };
    let max_scroll = (content_extent - viewport_extent).max(0.);

    let delta = delta_y * STACK_SCROLL_MOUSE_FACTOR;
    let old = state.cast_scroll_offset;
    state.cast_scroll_offset = (state.cast_scroll_offset + delta).clamp(0., max_scroll);

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
    if state.parallel_stage_active(workspace) {
        return false;
    }

    let working_area = workspace.working_area();

    let hovered = if pointer_in_stack_area(point, working_area, config) {
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

    if state.has_stage_modal_overlay(workspace) {
        let had_state = state.interaction_target.is_some() || state.interaction_since.is_some();
        state.interaction_target = None;
        state.interaction_since = None;
        return had_state;
    }

    if state.parallel_stage_active(workspace) {
        let had_state = state.interaction_target.is_some() || state.interaction_since.is_some();
        state.clear_auto_use_as_main_timer();
        return had_state;
    }

    let focused_cast = workspace
        .active_window()
        .and_then(|w| state.cast_index_for(w.id()));

    let target = focused_cast;
    let now = workspace.clock.now_unadjusted();
    let mut changed = false;

    if target != state.interaction_target {
        if let Some(idx) = target {
            if let Some(id) = state
                .group_at_layout_index(idx)
                .and_then(|g| g.windows.first().cloned())
            {
                state.consume_promote_defer(&id);
            }
        }
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
    state.consume_promote_defer(&id);
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

fn cast_content_extent(
    layouts: &[CastGroupLayout],
    slot_gap: f64,
    vertical: bool,
) -> f64 {
    if layouts.is_empty() {
        return 0.;
    }
    let total: f64 = layouts
        .iter()
        .map(|l| {
            if vertical {
                l.rect.size.h + slot_gap
            } else {
                l.rect.size.w + slot_gap
            }
        })
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

fn reset_managed_layout_defaults<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    state: &StageManagerState<W>,
) {
    for id in state.all_managed_windows() {
        workspace.floating.clear_stage_manager_default_layout(&id);
    }
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

/// Scroll the cast stack so the focused cast thumbnail stays visible.
fn scroll_stack_to_focused_cast<W: LayoutElement>(
    workspace: &Workspace<W>,
    config: &StageManagerConfig,
    state: &mut StageManagerState<W>,
    areas: StageAreas,
    thumb_width: i32,
    thumb_height: i32,
) {
    let Some(window) = workspace.active_window() else {
        return;
    };
    let Some(index) = state.cast_index_for(window.id()) else {
        return;
    };

    let slot_gap = workspace.options.layout.gaps;
    let vertical = config.stack_position.is_vertical();
    let thumb_extent = if vertical {
        f64::from(thumb_height)
    } else {
        f64::from(thumb_width)
    };

    let n = state.total_strip_groups();
    if n == 0 {
        return;
    }

    let content_extent = n as f64 * (thumb_extent + slot_gap) - slot_gap;
    let viewport_extent = if vertical {
        areas.stack_area.size.h
    } else {
        areas.stack_area.size.w
    };
    let max_scroll = (content_extent - viewport_extent).max(0.);

    let origin = if vertical {
        areas.stack_area.loc.y
    } else {
        areas.stack_area.loc.x
    };
    let viewport_end = origin + viewport_extent;

    let item_start =
        origin + slot_gap + index as f64 * (thumb_extent + slot_gap) - state.cast_scroll_offset;
    let item_end = item_start + thumb_extent;

    let mut scroll = state.cast_scroll_offset;
    if item_start < origin {
        scroll = (scroll - (origin - item_start)).max(0.);
    } else if item_end > viewport_end {
        scroll = (scroll + (item_end - viewport_end)).min(max_scroll);
    }

    state.cast_scroll_offset = scroll;
}

fn cast_thumb_size(
    working_area: Rectangle<f64, Logical>,
    areas: StageAreas,
    config: &StageManagerConfig,
) -> (i32, i32) {
    if config.stack_position.is_vertical() {
        let thumb_width = (areas.stack_area.size.w - STACK_INSET * 2.)
            .round()
            .max(1.) as i32;
        let thumb_height = (working_area.size.h * config.thumb_scale)
            .round()
            .max(1.) as i32;
        (thumb_width, thumb_height)
    } else {
        let thumb_width = (working_area.size.w * config.thumb_scale)
            .round()
            .max(1.) as i32;
        let thumb_height = (areas.stack_area.size.h - STACK_INSET * 2.)
            .round()
            .max(1.) as i32;
        (thumb_width, thumb_height)
    }
}

fn apply_geometry<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    config: &StageManagerConfig,
    state: &mut StageManagerState<W>,
) {
    let working_area = workspace.working_area();
    let areas = compute_areas(working_area, config);
    let (thumb_width, thumb_height) = cast_thumb_size(working_area, areas, config);

    scroll_stack_to_focused_cast(workspace, config, state, areas, thumb_width, thumb_height);

    state.cast_group_layouts = apply_cast_strip(
        workspace,
        config,
        areas,
        thumb_width,
        thumb_height,
        state,
    );

    apply_stage_geometry(workspace, config, areas.stage_area, state);

    if let Some(active) = workspace.active_window().map(|w| w.id().clone()) {
        if state.is_cast_window(&active) {
            workspace.floating.raise_to_top(&active);
        } else if state.is_stage_window(&active) {
            raise_stage_group_z_order(workspace, state, Some(&active));
        }
    } else if let Some(hovered) = state.hovered_cast {
        if let Some(id) = state
            .group_at_layout_index(hovered)
            .and_then(|g| g.windows.first())
        {
            workspace.floating.raise_to_top(id);
        }
    }

    workspace.scrolling.set_view_offset_for_stage_manager(0.);
}

fn apply_cast_strip<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    config: &StageManagerConfig,
    areas: StageAreas,
    thumb_width: i32,
    thumb_height: i32,
    state: &mut StageManagerState<W>,
) -> Vec<CastGroupLayout> {
    let mut layouts = Vec::new();
    let thumb_w = f64::from(thumb_width);
    let thumb_h = f64::from(thumb_height);
    let slot_gap = workspace.options.layout.gaps;
    let vertical = config.stack_position.is_vertical();

    let all_groups: Vec<&StageGroup<W>> = state
        .cast_groups
        .iter()
        .chain(state.hidden_groups.iter())
        .collect();

    if vertical {
        let thumb_x = match config.stack_position {
            StackPosition::Left => areas.stack_area.loc.x + STACK_INSET,
            StackPosition::Right => {
                areas.stack_area.loc.x + areas.stack_area.size.w - thumb_w - STACK_INSET
            }
            _ => unreachable!(),
        };
        let mut y_cursor =
            areas.stack_area.loc.y + slot_gap - state.cast_scroll_offset;

        for group in all_groups {
            y_cursor = layout_cast_group_slot(
                workspace,
                thumb_width,
                thumb_height,
                thumb_w,
                thumb_h,
                thumb_x,
                y_cursor,
                slot_gap,
                group,
                &mut layouts,
            );
        }
    } else {
        let thumb_y = match config.stack_position {
            StackPosition::Top => areas.stack_area.loc.y + STACK_INSET,
            StackPosition::Bottom => {
                areas.stack_area.loc.y + areas.stack_area.size.h - thumb_h - STACK_INSET
            }
            _ => unreachable!(),
        };
        let mut x_cursor =
            areas.stack_area.loc.x + slot_gap - state.cast_scroll_offset;

        for group in all_groups {
            x_cursor = layout_cast_group_slot_horizontal(
                workspace,
                thumb_width,
                thumb_height,
                thumb_w,
                thumb_h,
                x_cursor,
                thumb_y,
                slot_gap,
                group,
                &mut layouts,
            );
        }
    }

    let content_extent = cast_content_extent(&layouts, slot_gap, vertical);
    let viewport_extent = if vertical {
        areas.stack_area.size.h
    } else {
        areas.stack_area.size.w
    };
    let max_scroll = (content_extent - viewport_extent).max(0.);
    state.cast_scroll_offset = state.cast_scroll_offset.clamp(0., max_scroll);

    layouts
}

/// Lay out one cast slot in a vertical stack: front window on top, siblings stacked inert.
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

/// Lay out one cast slot in a horizontal stack.
fn layout_cast_group_slot_horizontal<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    thumb_width: i32,
    thumb_height: i32,
    thumb_w: f64,
    thumb_h: f64,
    x_cursor: f64,
    y: f64,
    slot_gap: f64,
    group: &StageGroup<W>,
    layouts: &mut Vec<CastGroupLayout>,
) -> f64 {
    let pos = Point::from((x_cursor, y));
    let rect = Rectangle::new(pos, Size::from((thumb_w, thumb_h)));

    if let Some(front) = group.windows.first() {
        move_to_floating(workspace, front);
        if !workspace.floating.has_window(front) {
            return x_cursor;
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
    x_cursor + thumb_w + slot_gap
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

fn is_stage_child_overlay<W: LayoutElement>(
    _workspace: &Workspace<W>,
    state: &StageManagerState<W>,
    id: &W::Id,
) -> bool {
    state.is_stage_dialog(id)
}

fn raise_stage_group_z_order<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    state: &StageManagerState<W>,
    active: Option<&W::Id>,
) {
    let Some(group) = &state.active_group else {
        return;
    };

    for id in &group.windows {
        if !is_stage_child_overlay(workspace, state, id) {
            workspace.floating.raise_to_top(id);
        }
    }
    for id in &group.windows {
        if is_stage_child_overlay(workspace, state, id) {
            workspace.floating.raise_to_top(id);
        }
    }

    if let Some(active) = active {
        let top = state
            .stage_modal_overlay_for_parent(workspace, active)
            .unwrap_or_else(|| active.clone());
        workspace.floating.raise_to_top(&top);
    }
}

fn apply_stage_geometry<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    config: &StageManagerConfig,
    stage_area: Rectangle<f64, Logical>,
    state: &StageManagerState<W>,
) {
    let Some(group) = &state.active_group else {
        return;
    };

    let primary_windows: Vec<_> = group
        .windows
        .iter()
        .filter(|id| !is_stage_child_overlay(workspace, state, id))
        .cloned()
        .collect();
    let n = primary_windows.len();
    if n == 0 {
        return;
    }

    let pad = STAGE_EDGE_PADDING;
    let gap = workspace.options.layout.gaps;
    let total_gap = gap * (n.saturating_sub(1)) as f64;
    let stage_left = stage_area.loc.x;
    let stage_right = stage_area.loc.x + stage_area.size.w;
    let stage_inner_width = stage_area.size.w.max(1.);
    let stage_inner_height = stage_area.size.h;
    let slot_width = (stage_inner_width - total_gap) / n as f64;
    let vertical_stack = config.stack_position.is_vertical();
    let cross_dim_default = if vertical_stack {
        (config.target_stage_height() - pad).max(1.)
    } else {
        (config.target_stage_width() - pad).max(1.)
    };

    for id in &group.windows {
        move_to_floating(workspace, id);
    }

    for (i, id) in primary_windows.iter().enumerate() {
        if !workspace.floating.has_window(id) {
            continue;
        }

        let window_width = if i + 1 == n {
            (stage_right - (stage_left + i as f64 * (slot_width + gap))).max(1.)
        } else {
            slot_width
        };

        if n > 1 {
            let size = Size::from((
                window_width.round().max(1.) as i32,
                cross_dim_default.round().max(1.) as i32,
            ));
            let x = stage_left + i as f64 * (slot_width + gap);
            let y = stage_area.loc.y + (stage_inner_height - f64::from(size.h)) / 2.;
            set_stage_window_geometry(workspace, id, size, Point::from((x, y)));
            continue;
        }

        let (avail_w, avail_h) = if vertical_stack {
            (window_width, cross_dim_default)
        } else {
            (cross_dim_default, stage_inner_height)
        };

        let saved = workspace.floating.stage_manager_saved_size(id);
        let mut size = resolve_stage_window_size(saved, avail_w, avail_h);
        if saved.is_none() {
            if vertical_stack {
                size.w = window_width.round().max(1.) as i32;
                size.h = cross_dim_default.round().max(1.) as i32;
            } else {
                size.w = cross_dim_default.round().max(1.) as i32;
                size.h = stage_inner_height.round().max(1.) as i32;
            }
        }

        if workspace.floating.has_user_position(id) {
            continue;
        }

        let x = if vertical_stack {
            stage_left + i as f64 * (slot_width + gap)
        } else {
            stage_left
                + i as f64 * (slot_width + gap)
                + (slot_width - f64::from(size.w)) / 2.
        };
        let y = stage_area.loc.y + (stage_inner_height - f64::from(size.h)) / 2.;
        set_stage_window_geometry(workspace, id, size, Point::from((x, y)));
    }
}

fn resolve_stage_window_size(
    saved: Option<Size<i32, Logical>>,
    available_width: f64,
    available_height: f64,
) -> Size<i32, Logical> {
    let default_w = available_width.round().max(1.) as i32;
    let default_h = available_height.round().max(1.) as i32;

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
    fn resolve_stage_window_size_defaults_to_available_size() {
        let size = resolve_stage_window_size(None, 1800., 900.);
        assert_eq!(size.w, 1800);
        assert_eq!(size.h, 900);
    }

    #[test]
    fn resolve_stage_window_size_clamps_saved_height() {
        let saved = Size::from((1200, 2000));
        let size = resolve_stage_window_size(Some(saved), 1800., 900.);
        assert_eq!(size.w, 1200);
        assert_eq!(size.h, 900);
    }
}

fn set_stage_window_geometry<W: LayoutElement>(
    workspace: &mut Workspace<W>,
    id: &W::Id,
    size: Size<i32, Logical>,
    pos: Point<f64, Logical>,
) {
    workspace.floating.clear_user_position(id);
    workspace.floating.clear_stage_manager_thumb(id);
    workspace
        .floating
        .set_window_width(Some(id), SizeChange::SetFixed(size.w), true);
    workspace
        .floating
        .set_window_height(Some(id), SizeChange::SetFixed(size.h), true);
    workspace.floating.set_tile_position(id, pos);
}
