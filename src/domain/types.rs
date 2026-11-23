use std::collections::{HashMap, HashSet};
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::DomainError;

pub const MAX_FEATURE_DEPTH: usize = 32;
pub const VISUAL_MAX_INDENT_DEPTH: usize = 8;

macro_rules! define_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

define_id!(ProjectId);
define_id!(ItemId);
define_id!(AssigneeId);
define_id!(TagId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    Active,
    Paused,
    Completed,
    Archived,
}

impl ProjectStatus {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Paused => "Paused",
            Self::Completed => "Completed",
            Self::Archived => "Archived",
        }
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Archived => "archived",
        }
    }

    pub fn from_db_str(s: &str) -> Result<Self, DomainError> {
        match s {
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "completed" => Ok(Self::Completed),
            "archived" => Ok(Self::Archived),
            other => Err(DomainError::InvalidWorkspace(format!(
                "invalid project status '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Feature,
    WorkItem,
}

impl ItemKind {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Feature => "Feature",
            Self::WorkItem => "Work Item",
        }
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Feature => "feature",
            Self::WorkItem => "work_item",
        }
    }

    pub fn from_db_str(s: &str) -> Result<Self, DomainError> {
        match s {
            "feature" => Ok(Self::Feature),
            "work_item" => Ok(Self::WorkItem),
            other => Err(DomainError::InvalidWorkspace(format!(
                "invalid item kind '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Backlog,
    Todo,
    InProgress,
    Done,
    Cancelled,
}

impl ItemStatus {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Backlog => "Backlog",
            Self::Todo => "Todo",
            Self::InProgress => "In Progress",
            Self::Done => "Done",
            Self::Cancelled => "Cancelled",
        }
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_db_str(s: &str) -> Result<Self, DomainError> {
        match s {
            "backlog" => Ok(Self::Backlog),
            "todo" => Ok(Self::Todo),
            "in_progress" => Ok(Self::InProgress),
            "done" => Ok(Self::Done),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(DomainError::InvalidWorkspace(format!(
                "invalid item status '{other}'"
            ))),
        }
    }

    pub const fn is_done(&self) -> bool {
        matches!(self, Self::Done)
    }

    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    pub const fn is_closed(&self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    None,
    Low,
    Medium,
    High,
    Urgent,
}

impl Priority {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Urgent => "Urgent",
        }
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Urgent => "urgent",
        }
    }

    pub fn from_db_str(s: &str) -> Result<Self, DomainError> {
        match s {
            "none" => Ok(Self::None),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "urgent" => Ok(Self::Urgent),
            other => Err(DomainError::InvalidWorkspace(format!(
                "invalid item priority '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub key_prefix: String,
    pub next_item_seq: i64,
    pub description: String,
    pub color: String,
    pub status: ProjectStatus,
    pub sort_order: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub project_id: ProjectId,
    pub item_number: i64,
    pub item_key: String,
    pub kind: ItemKind,
    pub parent_feature_id: Option<ItemId>,
    pub title: String,
    pub description: String,
    pub status: ItemStatus,
    pub priority: Priority,
    pub assignee_id: Option<AssigneeId>,
    pub sort_order: String,
    pub deleted_at: Option<DateTime<Utc>>,
    pub trash_root_id: Option<ItemId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Item {
    pub fn is_active(&self) -> bool {
        self.deleted_at.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignee {
    pub id: AssigneeId,
    pub name: String,
    pub handle: String,
    pub color: String,
    pub role: Option<String>,
    pub notes: String,
    pub archived_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: TagId,
    pub project_id: ProjectId,
    pub name: String,
    pub color: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureWorkItem {
    pub project_id: ProjectId,
    pub feature_id: ItemId,
    pub work_item_id: ItemId,
    pub sort_order: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureTreeRow {
    pub item_id: ItemId,
    pub depth: usize,
    pub has_children: bool,
    pub is_expanded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceMeta {
    pub schema_version: u32,
    pub app_min_version: String,
    pub workspace_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainEvent {
    WorkspaceSwitched,
    ProjectsChanged,
    ItemsChanged { project_id: ProjectId },
    TrashChanged { project_id: ProjectId },
    AssigneesChanged,
    TagsChanged { project_id: ProjectId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceOverview {
    pub meta: WorkspaceMeta,
    pub projects: Vec<Project>,
    pub assignees: Vec<Assignee>,
    pub active_snapshot: Option<ProjectSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub project: Project,
    pub projects: Vec<Project>,
    pub items: Vec<Item>,
    pub assignees: Vec<Assignee>,
    pub tags: Vec<Tag>,
    pub feature_work_items: Vec<FeatureWorkItem>,
    pub item_tags: HashMap<ItemId, Vec<TagId>>,
    pub trash_count: usize,
}

impl ProjectSnapshot {
    pub fn item(&self, id: ItemId) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }

    pub fn assignee(&self, id: AssigneeId) -> Option<&Assignee> {
        self.assignees.iter().find(|assignee| assignee.id == id)
    }

    pub fn tags_for_item(&self, id: ItemId) -> Vec<&Tag> {
        let Some(tag_ids) = self.item_tags.get(&id) else {
            return Vec::new();
        };
        tag_ids
            .iter()
            .filter_map(|tag_id| {
                self.tags
                    .iter()
                    .find(|tag| tag.id == *tag_id && tag.project_id == self.project.id)
            })
            .collect()
    }

    pub fn active_items(&self) -> Vec<&Item> {
        let mut items: Vec<&Item> = self
            .items
            .iter()
            .filter(|item| item.project_id == self.project.id && item.is_active())
            .collect();
        items.sort_by_key(|item| item.item_number);
        items
    }

    pub fn child_features(&self, parent_id: Option<ItemId>) -> Vec<&Item> {
        let mut features: Vec<&Item> = self
            .items
            .iter()
            .filter(|item| {
                item.project_id == self.project.id
                    && item.is_active()
                    && item.kind == ItemKind::Feature
                    && item.parent_feature_id == parent_id
            })
            .collect();
        features.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then_with(|| a.item_number.cmp(&b.item_number))
        });
        features
    }

    pub fn work_items_for_feature(&self, feature_id: ItemId) -> Vec<&Item> {
        let Some(feature) = self.item(feature_id) else {
            return Vec::new();
        };
        if !feature.is_active() || feature.kind != ItemKind::Feature {
            return Vec::new();
        }

        let mut links: Vec<&FeatureWorkItem> = self
            .feature_work_items
            .iter()
            .filter(|link| link.project_id == self.project.id && link.feature_id == feature_id)
            .collect();
        links.sort_by(|a, b| a.sort_order.cmp(&b.sort_order));

        let mut seen = HashSet::new();
        let mut items = Vec::new();
        for link in links {
            if !seen.insert(link.work_item_id) {
                continue;
            }
            if let Some(item) = self.item(link.work_item_id)
                && item.project_id == self.project.id
                && item.is_active()
                && item.kind == ItemKind::WorkItem
            {
                items.push(item);
            }
        }
        items
    }

    pub fn features_for_work_item(&self, work_item_id: ItemId) -> Vec<&Item> {
        let mut feature_ids = HashSet::new();
        for link in &self.feature_work_items {
            if link.project_id == self.project.id && link.work_item_id == work_item_id {
                feature_ids.insert(link.feature_id);
            }
        }

        let mut features: Vec<&Item> = feature_ids
            .into_iter()
            .filter_map(|fid| self.item(fid))
            .filter(|feature| {
                feature.project_id == self.project.id
                    && feature.is_active()
                    && feature.kind == ItemKind::Feature
            })
            .collect();
        features.sort_by_key(|feature| feature.item_number);
        features
    }

    pub fn unallocated_work_items(&self) -> Vec<&Item> {
        let active_features: HashSet<ItemId> = self
            .items
            .iter()
            .filter(|item| {
                item.project_id == self.project.id
                    && item.is_active()
                    && item.kind == ItemKind::Feature
            })
            .map(|item| item.id)
            .collect();

        let allocated_wi_ids: HashSet<ItemId> = self
            .feature_work_items
            .iter()
            .filter(|link| {
                link.project_id == self.project.id && active_features.contains(&link.feature_id)
            })
            .map(|link| link.work_item_id)
            .collect();

        let mut items: Vec<&Item> = self
            .items
            .iter()
            .filter(|item| {
                item.project_id == self.project.id
                    && item.is_active()
                    && item.kind == ItemKind::WorkItem
                    && !allocated_wi_ids.contains(&item.id)
            })
            .collect();
        items.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then_with(|| a.item_number.cmp(&b.item_number))
        });
        items
    }

    pub fn feature_depth(&self, feature_id: ItemId) -> usize {
        let mut depth = 0usize;
        let mut visited = HashSet::new();
        let mut current = self.item(feature_id);

        while let Some(item) = current {
            if !visited.insert(item.id) || depth >= MAX_FEATURE_DEPTH {
                break;
            }
            let Some(parent_id) = item.parent_feature_id else {
                break;
            };
            let Some(parent) = self.item(parent_id) else {
                break;
            };
            if !parent.is_active() || parent.kind != ItemKind::Feature {
                break;
            }
            depth += 1;
            current = Some(parent);
        }

        depth
    }

    pub fn feature_path(&self, feature_id: ItemId) -> Vec<&Item> {
        let mut path = Vec::new();
        let mut visited = HashSet::new();
        let mut current = self.item(feature_id);

        while let Some(item) = current {
            if !visited.insert(item.id) || path.len() >= MAX_FEATURE_DEPTH {
                break;
            }
            if !item.is_active() || item.kind != ItemKind::Feature {
                break;
            }
            path.push(item);
            current = item.parent_feature_id.and_then(|pid| self.item(pid));
        }

        path.reverse();
        path
    }

    /// Computes deduplicated active progress `(done, total)` across a Feature's subtree (`INV-11`).
    pub fn feature_progress(&self, feature_id: ItemId) -> (usize, usize) {
        let Some(root) = self.item(feature_id) else {
            return (0, 0);
        };
        if !root.is_active() || root.kind != ItemKind::Feature {
            return (0, 0);
        }

        let mut subtree_features = HashSet::new();
        let mut descendant_features = Vec::new();
        let mut stack = vec![feature_id];

        while let Some(fid) = stack.pop() {
            if !subtree_features.insert(fid) || subtree_features.len() > MAX_FEATURE_DEPTH * 64 {
                continue;
            }
            if fid != feature_id
                && let Some(desc) = self.item(fid)
            {
                descendant_features.push(desc);
            }
            for child in self.child_features(Some(fid)) {
                stack.push(child.id);
            }
        }

        let mut work_item_ids = HashSet::new();
        for link in &self.feature_work_items {
            if link.project_id == self.project.id && subtree_features.contains(&link.feature_id) {
                work_item_ids.insert(link.work_item_id);
            }
        }

        let mut wi_done = 0usize;
        let mut wi_total = 0usize;
        for wi_id in work_item_ids {
            if let Some(wi) = self.item(wi_id)
                && wi.project_id == self.project.id
                && wi.is_active()
                && wi.kind == ItemKind::WorkItem
                && !wi.status.is_cancelled()
            {
                wi_total += 1;
                if wi.status.is_done() {
                    wi_done += 1;
                }
            }
        }

        if wi_total > 0 {
            return (wi_done, wi_total);
        }

        // Empty-subtree fallback (INV-11): count active non-cancelled descendant Features.
        let mut feat_done = 0usize;
        let mut feat_total = 0usize;
        for desc in descendant_features {
            if !desc.status.is_cancelled() {
                feat_total += 1;
                if desc.status.is_done() {
                    feat_done += 1;
                }
            }
        }
        (feat_done, feat_total)
    }

    pub fn visible_feature_tree_rows(&self, expanded: &HashSet<ItemId>) -> Vec<FeatureTreeRow> {
        let mut rows = Vec::new();
        let mut visited = HashSet::new();
        self.collect_tree_rows(None, 0, expanded, &mut visited, &mut rows);
        rows
    }

    /// Traverses all active Features in deterministic `(sort_order, item_number)` DFS order.
    pub fn active_features_dfs(&self) -> Vec<(&Item, usize)> {
        let mut out = Vec::new();
        let mut visited = HashSet::new();
        self.collect_features_dfs(None, 0, &mut visited, &mut out);
        out
    }

    fn collect_features_dfs<'a>(
        &'a self,
        parent_id: Option<ItemId>,
        depth: usize,
        visited: &mut HashSet<ItemId>,
        out: &mut Vec<(&'a Item, usize)>,
    ) {
        if depth >= MAX_FEATURE_DEPTH {
            return;
        }
        for feature in self.child_features(parent_id) {
            if !visited.insert(feature.id) {
                continue;
            }
            out.push((feature, depth));
            self.collect_features_dfs(Some(feature.id), depth + 1, visited, out);
        }
    }

    /// Returns all strict active descendant Feature IDs of `feature_id`.
    pub fn descendant_feature_ids(&self, feature_id: ItemId) -> HashSet<ItemId> {
        let mut descendants = HashSet::new();
        let mut stack = Vec::new();
        for child in self.child_features(Some(feature_id)) {
            stack.push(child.id);
        }
        while let Some(id) = stack.pop() {
            if id == feature_id || !descendants.insert(id) {
                continue;
            }
            for child in self.child_features(Some(id)) {
                stack.push(child.id);
            }
        }
        descendants
    }

    /// Returns the maximum depth offset of active descendant Features (`0` for a leaf Feature).
    pub fn feature_subtree_height(&self, feature_id: ItemId) -> usize {
        let Some(root) = self.item(feature_id) else {
            return 0;
        };
        if !root.is_active() || root.kind != ItemKind::Feature {
            return 0;
        }
        let mut max_offset = 0usize;
        let mut visited = HashSet::from([feature_id]);
        let mut stack = vec![(feature_id, 0usize)];
        while let Some((id, offset)) = stack.pop() {
            max_offset = max_offset.max(offset);
            if offset + 1 >= MAX_FEATURE_DEPTH {
                continue;
            }
            for child in self.child_features(Some(id)) {
                if visited.insert(child.id) {
                    stack.push((child.id, offset + 1));
                }
            }
        }
        max_offset
    }

    /// Returns all ordered active sibling Features sharing the same `parent_feature_id`.
    pub fn sibling_features(&self, feature_id: ItemId) -> Vec<&Item> {
        let Some(feature) = self.item(feature_id) else {
            return Vec::new();
        };
        if feature.project_id != self.project.id
            || !feature.is_active()
            || feature.kind != ItemKind::Feature
        {
            return Vec::new();
        }
        self.child_features(feature.parent_feature_id)
    }

    fn collect_tree_rows(
        &self,
        parent_id: Option<ItemId>,
        depth: usize,
        expanded: &HashSet<ItemId>,
        visited: &mut HashSet<ItemId>,
        out: &mut Vec<FeatureTreeRow>,
    ) {
        if depth >= MAX_FEATURE_DEPTH {
            return;
        }
        for feature in self.child_features(parent_id) {
            if !visited.insert(feature.id) {
                continue;
            }
            let children = self.child_features(Some(feature.id));
            let has_children = !children.is_empty();
            let is_expanded = expanded.contains(&feature.id);
            out.push(FeatureTreeRow {
                item_id: feature.id,
                depth,
                has_children,
                is_expanded,
            });
            if has_children && is_expanded {
                self.collect_tree_rows(Some(feature.id), depth + 1, expanded, visited, out);
            }
        }
    }
}

pub fn visual_indent_px(depth: usize) -> f32 {
    (depth.min(VISUAL_MAX_INDENT_DEPTH) as f32) * 14.0
}

pub fn clamped_depth_badge(depth: usize) -> Option<String> {
    if depth >= VISUAL_MAX_INDENT_DEPTH {
        Some(format!("·L{}", depth + 1))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 23, 12, 0, 0).unwrap()
    }

    #[test]
    fn enums_serialize_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&ProjectStatus::Active).unwrap(),
            "\"active\""
        );
        assert_eq!(
            serde_json::to_string(&ItemKind::WorkItem).unwrap(),
            "\"work_item\""
        );
        assert_eq!(
            serde_json::to_string(&ItemStatus::InProgress).unwrap(),
            "\"in_progress\""
        );
        assert_eq!(
            serde_json::to_string(&Priority::Urgent).unwrap(),
            "\"urgent\""
        );
    }

    #[test]
    fn visual_indent_clamps_after_depth_8() {
        assert_eq!(visual_indent_px(0), 0.0);
        assert_eq!(visual_indent_px(7), 98.0);
        assert_eq!(visual_indent_px(8), 112.0);
        assert_eq!(visual_indent_px(10), 112.0);
        assert_eq!(visual_indent_px(31), 112.0);

        assert_eq!(clamped_depth_badge(0), None);
        assert_eq!(clamped_depth_badge(7), None);
        assert_eq!(clamped_depth_badge(8), Some("·L9".to_string()));
        assert_eq!(clamped_depth_badge(10), Some("·L11".to_string()));
        assert_eq!(clamped_depth_badge(31), Some("·L32".to_string()));
    }

    #[test]
    fn progress_deduplicates_shared_work_items_and_falls_back_when_empty() {
        let pid = ProjectId::from_uuid(Uuid::from_u128(1));
        let f1 = ItemId::from_uuid(Uuid::from_u128(10));
        let f2a = ItemId::from_uuid(Uuid::from_u128(11));
        let f2b = ItemId::from_uuid(Uuid::from_u128(12));
        let wi_shared = ItemId::from_uuid(Uuid::from_u128(20));
        let wi_cancel = ItemId::from_uuid(Uuid::from_u128(21));

        let project = Project {
            id: pid,
            name: "Test".to_string(),
            key_prefix: "TST".to_string(),
            next_item_seq: 6,
            description: String::new(),
            color: "#6366F1".to_string(),
            status: ProjectStatus::Active,
            sort_order: "a0".to_string(),
            created_at: ts(),
            updated_at: ts(),
        };

        let make_item = |id, num, kind, parent, status| Item {
            id,
            project_id: pid,
            item_number: num,
            item_key: format!("TST-{num}"),
            kind,
            parent_feature_id: parent,
            title: format!("Item {num}"),
            description: String::new(),
            status,
            priority: Priority::None,
            assignee_id: None,
            sort_order: format!("a{num}"),
            deleted_at: None,
            trash_root_id: None,
            created_at: ts(),
            updated_at: ts(),
        };

        let snapshot = ProjectSnapshot {
            project: project.clone(),
            projects: vec![project],
            items: vec![
                make_item(f1, 1, ItemKind::Feature, None, ItemStatus::Todo),
                make_item(f2a, 2, ItemKind::Feature, Some(f1), ItemStatus::Done),
                make_item(f2b, 3, ItemKind::Feature, Some(f1), ItemStatus::Todo),
                make_item(wi_shared, 4, ItemKind::WorkItem, None, ItemStatus::Done),
                make_item(wi_cancel, 5, ItemKind::WorkItem, None, ItemStatus::Cancelled),
            ],
            assignees: Vec::new(),
            tags: Vec::new(),
            feature_work_items: vec![
                FeatureWorkItem {
                    project_id: pid,
                    feature_id: f2a,
                    work_item_id: wi_shared,
                    sort_order: "a0".to_string(),
                    created_at: ts(),
                },
                FeatureWorkItem {
                    project_id: pid,
                    feature_id: f2b,
                    work_item_id: wi_shared,
                    sort_order: "a0".to_string(),
                    created_at: ts(),
                },
                FeatureWorkItem {
                    project_id: pid,
                    feature_id: f2b,
                    work_item_id: wi_cancel,
                    sort_order: "a1".to_string(),
                    created_at: ts(),
                },
            ],
            item_tags: HashMap::new(),
            trash_count: 0,
        };

        // Shared WI counted once under F1; cancelled WI excluded.
        assert_eq!(snapshot.feature_progress(f1), (1, 1));
        assert_eq!(snapshot.feature_progress(f2a), (1, 1));
        assert_eq!(snapshot.feature_progress(f2b), (1, 1));

        // Removing WIs triggers child-feature fallback: F2a done, F2b todo -> (1, 2).
        let mut fallback_snapshot = snapshot;
        fallback_snapshot.feature_work_items.clear();
        assert_eq!(fallback_snapshot.feature_progress(f1), (1, 2));
        assert_eq!(fallback_snapshot.unallocated_work_items().len(), 2);
    }
}
