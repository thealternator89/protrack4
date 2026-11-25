use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Window};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::{
    Assignee, AssigneeId, FeatureWorkItem, Item, ItemId, ItemKind, ItemStatus, Priority, Project,
    ProjectId, ProjectSnapshot, ProjectStatus, Tag, TagId, WorkspaceMeta, WorkspaceOverview,
};
use crate::workspace::AppPreferences;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CenterViewTab {
    #[default]
    Outline,
    Table,
    Unallocated,
    Trash,
}

impl CenterViewTab {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Outline => "Outline",
            Self::Table => "Table",
            Self::Unallocated => "Unallocated",
            Self::Trash => "Trash",
        }
    }

    pub const fn toolbar_index(&self) -> Option<usize> {
        match self {
            Self::Outline => Some(0),
            Self::Table => Some(1),
            Self::Unallocated => Some(2),
            Self::Trash => None,
        }
    }

    pub const fn from_toolbar_index(ix: usize) -> Self {
        match ix {
            0 => Self::Outline,
            1 => Self::Table,
            2 => Self::Unallocated,
            _ => Self::Outline,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    pub const fn cycle(&self) -> Self {
        match self {
            Self::System => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::System,
        }
    }

    pub fn apply(&self, window: Option<&mut Window>, cx: &mut App) {
        match self {
            Self::System => Theme::sync_system_appearance(window, cx),
            Self::Light => Theme::change(ThemeMode::Light, window, cx),
            Self::Dark => Theme::change(ThemeMode::Dark, window, cx),
        }
        Theme::global_mut(cx).notification.placement = gpui_kit::Anchor::BottomRight;
    }
}

pub struct ProjectStore {
    workspace_path: Option<PathBuf>,
    workspace_meta: Option<WorkspaceMeta>,
    projects: Vec<Project>,
    assignees: Vec<Assignee>,
    snapshot: Option<ProjectSnapshot>,
    active_tab: CenterViewTab,
    selected_item_id: Option<ItemId>,
    focused_feature_id: Option<ItemId>,
    expanded_features: HashSet<ItemId>,
    sidebar_open: bool,
    inspector_open: bool,
    theme_preference: ThemePreference,
}

impl ProjectStore {
    pub fn new_with_seed() -> Self {
        let snapshot = sample_snapshot();
        let expanded_features: HashSet<ItemId> = snapshot
            .items
            .iter()
            .filter(|item| item.is_active() && item.kind == ItemKind::Feature)
            .map(|item| item.id)
            .collect();
        let selected_item_id = snapshot
            .child_features(None)
            .first()
            .map(|item| item.id)
            .or_else(|| snapshot.active_items().first().map(|item| item.id));
        let focused_feature_id = snapshot
            .child_features(None)
            .first()
            .map(|item| item.id);

        Self {
            workspace_path: None,
            workspace_meta: Some(WorkspaceMeta {
                schema_version: crate::db::APP_MAX_SCHEMA_VERSION,
                app_min_version: env!("CARGO_PKG_VERSION").to_string(),
                workspace_name: "sample.protrack".to_string(),
                created_at: snapshot.project.created_at,
                updated_at: snapshot.project.updated_at,
            }),
            projects: snapshot.projects.clone(),
            assignees: snapshot.assignees.clone(),
            snapshot: Some(snapshot),
            active_tab: CenterViewTab::Outline,
            selected_item_id,
            focused_feature_id,
            expanded_features,
            sidebar_open: true,
            inspector_open: true,
            theme_preference: ThemePreference::System,
        }
    }

    pub fn new_from_preferences(prefs: &AppPreferences) -> Self {
        Self {
            workspace_path: None,
            workspace_meta: None,
            projects: Vec::new(),
            assignees: Vec::new(),
            snapshot: None,
            active_tab: CenterViewTab::Outline,
            selected_item_id: None,
            focused_feature_id: None,
            expanded_features: HashSet::new(),
            sidebar_open: !prefs.sidebar_collapsed,
            inspector_open: prefs.inspector_open,
            theme_preference: prefs.theme,
        }
    }

    pub fn has_workspace(&self) -> bool {
        self.workspace_meta.is_some()
    }

    pub fn workspace_path(&self) -> Option<&Path> {
        self.workspace_path.as_deref()
    }

    pub fn workspace_meta(&self) -> Option<&WorkspaceMeta> {
        self.workspace_meta.as_ref()
    }

    pub fn workspace_name(&self) -> String {
        if let Some(path) = &self.workspace_path
            && let Some(file_name) = path.file_name().and_then(|s| s.to_str())
        {
            return file_name.to_string();
        }
        if let Some(meta) = &self.workspace_meta {
            return meta.workspace_name.clone();
        }
        "No Workspace".to_string()
    }

    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    pub fn assignees(&self) -> &[Assignee] {
        &self.assignees
    }

    pub fn snapshot(&self) -> Option<&ProjectSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn active_tab(&self) -> CenterViewTab {
        self.active_tab
    }

    pub fn selected_item_id(&self) -> Option<ItemId> {
        self.selected_item_id
    }

    pub fn selected_item(&self) -> Option<&Item> {
        let snapshot = self.snapshot.as_ref()?;
        self.selected_item_id
            .and_then(|id| snapshot.item(id))
            .filter(|item| item.is_active())
    }

    pub fn focused_feature_id(&self) -> Option<ItemId> {
        self.focused_feature_id
    }

    pub fn set_focused_feature(&mut self, id: Option<ItemId>) {
        self.focused_feature_id = id;
    }

    pub fn expanded_features(&self) -> &HashSet<ItemId> {
        &self.expanded_features
    }

    pub fn is_feature_expanded(&self, id: ItemId) -> bool {
        self.expanded_features.contains(&id)
    }

    pub fn sidebar_open(&self) -> bool {
        self.sidebar_open
    }

    pub fn inspector_open(&self) -> bool {
        self.inspector_open
    }

    pub fn theme_preference(&self) -> ThemePreference {
        self.theme_preference
    }

    pub fn select_item(&mut self, id: Option<ItemId>) {
        self.selected_item_id = id;
        if let Some(item_id) = id
            && let Some(snapshot) = &self.snapshot
            && let Some(item) = snapshot.item(item_id)
            && item.is_active()
            && item.kind == ItemKind::Feature
        {
            self.focused_feature_id = Some(item_id);
        }
    }

    pub fn set_active_tab(&mut self, tab: CenterViewTab) {
        self.active_tab = tab;
    }

    pub fn toggle_feature_expanded(&mut self, id: ItemId) {
        if !self.expanded_features.remove(&id) {
            self.expanded_features.insert(id);
        }
    }

    pub fn expand_feature(&mut self, id: ItemId) {
        self.expanded_features.insert(id);
    }

    pub fn expand_ancestors_of(&mut self, item_id: ItemId) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let Some(item) = snapshot.item(item_id) else {
            return;
        };
        let mut queue: Vec<ItemId> = if item.kind == ItemKind::WorkItem {
            snapshot
                .features_for_work_item(item_id)
                .into_iter()
                .map(|f| f.id)
                .collect()
        } else {
            item.parent_feature_id.into_iter().collect()
        };
        let mut visited = HashSet::new();
        while let Some(pid) = queue.pop() {
            if !visited.insert(pid) {
                continue;
            }
            self.expanded_features.insert(pid);
            if let Some(parent_id) = snapshot.item(pid).and_then(|parent| parent.parent_feature_id) {
                queue.push(parent_id);
            }
        }
    }

    pub fn expand_all_features(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        for item in &snapshot.items {
            if item.is_active() && item.kind == ItemKind::Feature {
                self.expanded_features.insert(item.id);
            }
        }
    }

    pub fn collapse_all_features(&mut self) {
        self.expanded_features.clear();
    }

    pub fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
    }

    pub fn set_sidebar_open(&mut self, open: bool) {
        self.sidebar_open = open;
    }

    pub fn toggle_inspector(&mut self) {
        self.inspector_open = !self.inspector_open;
    }

    pub fn set_inspector_open(&mut self, open: bool) {
        self.inspector_open = open;
    }

    pub fn cycle_theme_preference(&mut self) -> ThemePreference {
        self.theme_preference = self.theme_preference.cycle();
        self.theme_preference
    }

    pub fn set_theme_preference(&mut self, preference: ThemePreference) {
        self.theme_preference = preference;
    }

    pub fn replace_snapshot(&mut self, snapshot: ProjectSnapshot) {
        self.projects = snapshot.projects.clone();
        self.assignees = snapshot.assignees.clone();
        self.snapshot = Some(snapshot);
        self.reconcile_selection(false);
    }

    pub fn apply_workspace_overview(&mut self, path: PathBuf, overview: WorkspaceOverview) {
        let workspace_changed = self.workspace_path.as_ref() != Some(&path);
        let new_project_id = overview.active_snapshot.as_ref().map(|s| s.project.id);
        let old_project_id = self.snapshot.as_ref().map(|s| s.project.id);
        let project_changed = workspace_changed || new_project_id != old_project_id;

        self.workspace_path = Some(path);
        self.workspace_meta = Some(overview.meta);
        self.projects = overview.projects;
        self.assignees = overview.assignees;
        self.snapshot = overview.active_snapshot;

        if workspace_changed {
            self.active_tab = CenterViewTab::Outline;
        }
        self.reconcile_selection(project_changed);
    }

    pub fn clear_workspace(&mut self) {
        self.workspace_path = None;
        self.workspace_meta = None;
        self.projects.clear();
        self.assignees.clear();
        self.snapshot = None;
        self.selected_item_id = None;
        self.focused_feature_id = None;
        self.expanded_features.clear();
    }

    fn reconcile_selection(&mut self, reset_tree_and_selection: bool) {
        let Some(snapshot) = &self.snapshot else {
            self.selected_item_id = None;
            self.focused_feature_id = None;
            self.expanded_features.clear();
            return;
        };

        let active_feature_ids: HashSet<ItemId> = snapshot
            .items
            .iter()
            .filter(|item| item.is_active() && item.kind == ItemKind::Feature)
            .map(|item| item.id)
            .collect();

        if reset_tree_and_selection {
            self.expanded_features = active_feature_ids;
            self.selected_item_id = snapshot
                .child_features(None)
                .first()
                .map(|item| item.id)
                .or_else(|| snapshot.active_items().first().map(|item| item.id));
            self.focused_feature_id = snapshot.child_features(None).first().map(|item| item.id);
            return;
        }

        self.expanded_features
            .retain(|id| active_feature_ids.contains(id));

        if !self
            .focused_feature_id
            .is_some_and(|id| active_feature_ids.contains(&id))
        {
            self.focused_feature_id = snapshot.child_features(None).first().map(|item| item.id);
        }

        let selection_still_active = self
            .selected_item_id
            .and_then(|id| snapshot.item(id))
            .is_some_and(|item| item.is_active());

        if !selection_still_active {
            self.selected_item_id = snapshot
                .child_features(None)
                .first()
                .map(|item| item.id)
                .or_else(|| snapshot.active_items().first().map(|item| item.id));
        }
    }
}

fn seed_time(hour: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 23, hour, min, 0)
        .single()
        .expect("valid seed timestamp")
}

pub fn sample_snapshot() -> ProjectSnapshot {
    let project_id = ProjectId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_0001));
    let mb_id = AssigneeId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_0101));
    let ai_id = AssigneeId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_0102));
    let core_tag_id = TagId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_0201));
    let ui_tag_id = TagId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_0202));

    let f1_id = ItemId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_1001));
    let f2_id = ItemId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_1002));
    let f3_id = ItemId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_1003));
    let f4_id = ItemId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_1004));

    let w5_id = ItemId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_2005));
    let w6_id = ItemId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_2006));
    let w7_id = ItemId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_2007));
    let w8_id = ItemId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_2008));
    let w9_id = ItemId::from_uuid(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_2009));

    let project = Project {
        id: project_id,
        name: "ProTrack v1".to_string(),
        key_prefix: "PRO".to_string(),
        next_item_seq: 10,
        description: "Personal hierarchical project tracker with Features-only blocking and portable SQLite workspaces.".to_string(),
        color: "#6366F1".to_string(),
        status: ProjectStatus::Active,
        sort_order: "a0".to_string(),
        created_at: seed_time(9, 0),
        updated_at: seed_time(14, 30),
    };

    let assignees = vec![
        Assignee {
            id: mb_id,
            name: "Mark Benson".to_string(),
            handle: "MB".to_string(),
            color: "#6366F1".to_string(),
            role: Some("Founder".to_string()),
            notes: "Product & Rust architecture".to_string(),
            archived_at: None,
            created_at: seed_time(9, 0),
            updated_at: seed_time(9, 0),
        },
        Assignee {
            id: ai_id,
            name: "Claude Agent".to_string(),
            handle: "AI".to_string(),
            color: "#10B981".to_string(),
            role: Some("Engineering".to_string()),
            notes: "Implementation & verification".to_string(),
            archived_at: None,
            created_at: seed_time(9, 5),
            updated_at: seed_time(9, 5),
        },
    ];

    let tags = vec![
        Tag {
            id: core_tag_id,
            project_id,
            name: "core".to_string(),
            color: "#8B5CF6".to_string(),
            created_at: seed_time(9, 0),
        },
        Tag {
            id: ui_tag_id,
            project_id,
            name: "ui".to_string(),
            color: "#0EA5E9".to_string(),
            created_at: seed_time(9, 0),
        },
    ];

    let items = vec![
        Item {
            id: f1_id,
            project_id,
            item_number: 1,
            item_key: "PRO-1".to_string(),
            kind: ItemKind::Feature,
            parent_feature_id: None,
            title: "Core Platform".to_string(),
            description: "## Core Platform\n\nFoundational architecture for **ProTrack v1**, spanning domain invariants, SQLite persistence, and the 3-pane `gpui-kit` desktop shell.\n\n- Strict project isolation (`INV-01`)\n- Recursive Feature nesting up to depth `32` (`INV-02`)\n- Deduplicated subtree progress roll-ups (`INV-11`)".to_string(),
            status: ItemStatus::InProgress,
            priority: Priority::High,
            assignee_id: Some(mb_id),
            link: None,
            sort_order: "a0".to_string(),
            deleted_at: None,
            trash_root_id: None,
            created_at: seed_time(9, 10),
            updated_at: seed_time(14, 15),
        },
        Item {
            id: f2_id,
            project_id,
            item_number: 2,
            item_key: "PRO-2".to_string(),
            kind: ItemKind::Feature,
            parent_feature_id: Some(f1_id),
            title: "Data & Persistence".to_string(),
            description: "### Data & Persistence\n\nOwns `.protrack` SQLite workspace files, advisory locking, and async `DbService` actor snapshots.".to_string(),
            status: ItemStatus::Todo,
            priority: Priority::High,
            assignee_id: Some(mb_id),
            link: None,
            sort_order: "a0".to_string(),
            deleted_at: None,
            trash_root_id: None,
            created_at: seed_time(9, 20),
            updated_at: seed_time(13, 45),
        },
        Item {
            id: f3_id,
            project_id,
            item_number: 3,
            item_key: "PRO-3".to_string(),
            kind: ItemKind::Feature,
            parent_feature_id: Some(f2_id),
            title: "Schema & Migrations".to_string(),
            description: "### Schema & Migrations\n\nManages `001_initial.sql`, forward-version guards, and pre-migration `.bak` safety backups.".to_string(),
            status: ItemStatus::InProgress,
            priority: Priority::Medium,
            assignee_id: Some(ai_id),
            link: None,
            sort_order: "a0".to_string(),
            deleted_at: None,
            trash_root_id: None,
            created_at: seed_time(9, 30),
            updated_at: seed_time(14, 5),
        },
        Item {
            id: f4_id,
            project_id,
            item_number: 4,
            item_key: "PRO-4".to_string(),
            kind: ItemKind::Feature,
            parent_feature_id: Some(f3_id),
            title: "SQLite Strict Triggers".to_string(),
            description: "### SQLite Strict Triggers\n\nEnforces `INV-02`, `INV-04`, and kind-conversion guards directly inside SQLite triggers.".to_string(),
            status: ItemStatus::Todo,
            priority: Priority::Medium,
            assignee_id: Some(ai_id),
            link: None,
            sort_order: "a0".to_string(),
            deleted_at: None,
            trash_root_id: None,
            created_at: seed_time(9, 40),
            updated_at: seed_time(12, 50),
        },
        Item {
            id: w5_id,
            project_id,
            item_number: 5,
            item_key: "PRO-5".to_string(),
            kind: ItemKind::WorkItem,
            parent_feature_id: None,
            title: "Define unified items table & composite FKs".to_string(),
            description: "## Unified Items Schema\n\nShared across **Data & Persistence** and **Schema & Migrations** so `PRO-N` keys survive lossless conversion.\n\n```sql\nFOREIGN KEY (parent_feature_id, project_id)\n    REFERENCES items(id, project_id) ON DELETE CASCADE\n```\n\n- Preserves `id` (UUIDv7) and `item_key`\n- Enables single external-content FTS5 index".to_string(),
            status: ItemStatus::Done,
            priority: Priority::High,
            assignee_id: Some(mb_id),
            link: None,
            sort_order: "a0".to_string(),
            deleted_at: None,
            trash_root_id: None,
            created_at: seed_time(10, 0),
            updated_at: seed_time(11, 30),
        },
        Item {
            id: w6_id,
            project_id,
            item_number: 6,
            item_key: "PRO-6".to_string(),
            kind: ItemKind::WorkItem,
            parent_feature_id: None,
            title: "Verify FTS5 external-content indexing".to_string(),
            description: "Confirm `items_fts` triggers sync inserts, updates, and soft-deletes cleanly.".to_string(),
            status: ItemStatus::InProgress,
            priority: Priority::Medium,
            assignee_id: Some(ai_id),
            link: None,
            sort_order: "a1".to_string(),
            deleted_at: None,
            trash_root_id: None,
            created_at: seed_time(10, 15),
            updated_at: seed_time(14, 0),
        },
        Item {
            id: w7_id,
            project_id,
            item_number: 7,
            item_key: "PRO-7".to_string(),
            kind: ItemKind::WorkItem,
            parent_feature_id: None,
            title: "Write invariant guard unit tests".to_string(),
            description: "Cover `INV-01`–`INV-15` edge cases with deterministic fixtures and negative trigger assertions.".to_string(),
            status: ItemStatus::Todo,
            priority: Priority::Low,
            assignee_id: Some(mb_id),
            link: None,
            sort_order: "a2".to_string(),
            deleted_at: None,
            trash_root_id: None,
            created_at: seed_time(10, 30),
            updated_at: seed_time(10, 30),
        },
        Item {
            id: w8_id,
            project_id,
            item_number: 8,
            item_key: "PRO-8".to_string(),
            kind: ItemKind::WorkItem,
            parent_feature_id: None,
            title: "Explore custom app icon & tray polish".to_string(),
            description: "Unallocated exploration item: evaluate crisp vector app icon proportions for Light & Dark macOS docks.".to_string(),
            status: ItemStatus::Backlog,
            priority: Priority::Low,
            assignee_id: None,
            link: None,
            sort_order: "a0".to_string(),
            deleted_at: None,
            trash_root_id: None,
            created_at: seed_time(11, 0),
            updated_at: seed_time(11, 0),
        },
        Item {
            id: w9_id,
            project_id,
            item_number: 9,
            item_key: "PRO-9".to_string(),
            kind: ItemKind::WorkItem,
            parent_feature_id: None,
            title: "Benchmark startup latency on large workspaces".to_string(),
            description: "Unallocated performance task: measure snapshot load time for **5,000+ items** to keep UI frames under budget.".to_string(),
            status: ItemStatus::Todo,
            priority: Priority::Urgent,
            assignee_id: Some(mb_id),
            link: None,
            sort_order: "a1".to_string(),
            deleted_at: None,
            trash_root_id: None,
            created_at: seed_time(11, 15),
            updated_at: seed_time(13, 20),
        },
    ];

    let feature_work_items = vec![
        FeatureWorkItem {
            project_id,
            feature_id: f2_id,
            work_item_id: w5_id,
            sort_order: "a0".to_string(),
            created_at: seed_time(10, 5),
        },
        FeatureWorkItem {
            project_id,
            feature_id: f3_id,
            work_item_id: w5_id,
            sort_order: "a0".to_string(),
            created_at: seed_time(10, 6),
        },
        FeatureWorkItem {
            project_id,
            feature_id: f3_id,
            work_item_id: w6_id,
            sort_order: "a1".to_string(),
            created_at: seed_time(10, 16),
        },
        FeatureWorkItem {
            project_id,
            feature_id: f4_id,
            work_item_id: w7_id,
            sort_order: "a0".to_string(),
            created_at: seed_time(10, 31),
        },
    ];

    let mut item_tags = HashMap::new();
    item_tags.insert(f1_id, vec![core_tag_id]);
    item_tags.insert(f2_id, vec![core_tag_id]);
    item_tags.insert(f3_id, vec![core_tag_id]);
    item_tags.insert(f4_id, vec![core_tag_id]);
    item_tags.insert(w5_id, vec![core_tag_id]);
    item_tags.insert(w6_id, vec![core_tag_id]);
    item_tags.insert(w7_id, vec![core_tag_id, ui_tag_id]);
    item_tags.insert(w8_id, vec![ui_tag_id]);
    item_tags.insert(w9_id, vec![core_tag_id]);

    let mut snapshot = ProjectSnapshot {
        project: project.clone(),
        projects: vec![project],
        items,
        assignees,
        tags,
        feature_work_items,
        item_tags,
        progress_by_feature: HashMap::new(),
        item_blockers: Vec::new(),
        direct_blockers_by_item: HashMap::new(),
        inherited_blockers_by_item: HashMap::new(),
        effective_block_state: HashMap::new(),
        blocker_warnings: HashMap::new(),
        trash_count: 0,
        trash_groups: Vec::new(),
    };
    snapshot.progress_by_feature = snapshot.compute_progress_map();
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_fixture_matches_phase_1_counts_and_depth() {
        let store = ProjectStore::new_with_seed();
        let snap = store.snapshot().unwrap();

        assert_eq!(snap.projects.len(), 1);
        assert_eq!(snap.assignees.len(), 2);
        assert_eq!(snap.tags.len(), 2);

        let features: Vec<_> = snap
            .active_items()
            .into_iter()
            .filter(|item| item.kind == ItemKind::Feature)
            .collect();
        assert_eq!(features.len(), 4);

        let max_depth = features
            .iter()
            .map(|f| snap.feature_depth(f.id))
            .max()
            .unwrap();
        assert_eq!(max_depth, 3);

        let unallocated = snap.unallocated_work_items();
        assert_eq!(unallocated.len(), 2);

        let allocated_wi_count = snap
            .active_items()
            .into_iter()
            .filter(|item| {
                item.kind == ItemKind::WorkItem && !snap.features_for_work_item(item.id).is_empty()
            })
            .count();
        assert_eq!(allocated_wi_count, 3);

        let pro_5 = snap
            .active_items()
            .into_iter()
            .find(|item| item.item_key == "PRO-5")
            .unwrap();
        assert_eq!(snap.features_for_work_item(pro_5.id).len(), 2);

        let pro_1 = snap
            .active_items()
            .into_iter()
            .find(|item| item.item_key == "PRO-1")
            .unwrap();
        assert_eq!(snap.feature_progress(pro_1.id), (1, 3));
        assert_eq!(store.selected_item_id(), Some(pro_1.id));
    }

    #[test]
    fn collapsing_parent_feature_hides_descendants_and_state_toggles_work() {
        let mut store = ProjectStore::new_with_seed();
        let rows = store
            .snapshot()
            .unwrap()
            .visible_feature_tree_rows(store.expanded_features());
        assert_eq!(rows.len(), 4);

        let pro_2_id = rows[1].item_id;
        store.toggle_feature_expanded(pro_2_id);
        let collapsed_rows = store
            .snapshot()
            .unwrap()
            .visible_feature_tree_rows(store.expanded_features());
        assert_eq!(collapsed_rows.len(), 2);

        store.toggle_sidebar();
        assert!(!store.sidebar_open());
        store.toggle_inspector();
        assert!(!store.inspector_open());

        assert_eq!(store.cycle_theme_preference(), ThemePreference::Light);
        assert_eq!(store.cycle_theme_preference(), ThemePreference::Dark);
        assert_eq!(store.cycle_theme_preference(), ThemePreference::System);

        store.set_active_tab(CenterViewTab::Table);
        assert_eq!(store.active_tab(), CenterViewTab::Table);
    }
}
