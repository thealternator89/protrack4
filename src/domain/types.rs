use std::collections::{HashMap, HashSet};
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::{
    BlockerCandidateRejection, BlockerRef, BlockerWarning, ConversionBlockReason,
    ConversionEligibility, DomainError, EffectiveBlockState, FeatureProgress, ItemBlocker,
    format_conversion_block_message,
};

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

impl Assignee {
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashGroup {
    pub root: Item,
    pub deleted_at: DateTime<Utc>,
    pub descendant_feature_count: usize,
    pub descendant_work_item_count: usize,
    pub total_item_count: usize,
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
    #[serde(default)]
    pub progress_by_feature: HashMap<ItemId, FeatureProgress>,
    #[serde(default)]
    pub item_blockers: Vec<ItemBlocker>,
    #[serde(default)]
    pub direct_blockers_by_item: HashMap<ItemId, Vec<BlockerRef>>,
    #[serde(default)]
    pub inherited_blockers_by_item: HashMap<ItemId, Vec<BlockerRef>>,
    #[serde(default)]
    pub effective_block_state: HashMap<ItemId, EffectiveBlockState>,
    #[serde(default)]
    pub blocker_warnings: HashMap<ItemId, Vec<BlockerWarning>>,
    pub trash_count: usize,
    #[serde(default)]
    pub trash_groups: Vec<TrashGroup>,
}

impl ProjectSnapshot {
    pub fn item(&self, id: ItemId) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }

    pub fn assignee(&self, id: AssigneeId) -> Option<&Assignee> {
        self.assignees.iter().find(|assignee| assignee.id == id)
    }

    pub fn active_assignees(&self) -> Vec<&Assignee> {
        self.assignees
            .iter()
            .filter(|assignee| !assignee.is_archived())
            .collect()
    }

    pub fn selectable_assignees(&self, current_assignee_id: Option<AssigneeId>) -> Vec<&Assignee> {
        self.assignees
            .iter()
            .filter(|assignee| !assignee.is_archived() || Some(assignee.id) == current_assignee_id)
            .collect()
    }

    pub fn tags_for_item(&self, id: ItemId) -> Vec<&Tag> {
        let Some(tag_ids) = self.item_tags.get(&id) else {
            return Vec::new();
        };
        let mut tags: Vec<&Tag> = tag_ids
            .iter()
            .filter_map(|tag_id| {
                self.tags
                    .iter()
                    .find(|tag| tag.id == *tag_id && tag.project_id == self.project.id)
            })
            .collect();
        tags.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
        });
        tags
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
        links.sort_by(|a, b| {
            a.sort_order.cmp(&b.sort_order).then_with(|| {
                let a_num = self.item(a.work_item_id).map_or(i64::MAX, |i| i.item_number);
                let b_num = self.item(b.work_item_id).map_or(i64::MAX, |i| i.item_number);
                a_num.cmp(&b_num)
            })
        });

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

    /// Computes deduplicated active progress metrics across a Feature's subtree (`INV-11`).
    pub fn compute_feature_progress(&self, feature_id: ItemId) -> FeatureProgress {
        let Some(root) = self.item(feature_id) else {
            return FeatureProgress::default();
        };
        if !root.is_active() || root.kind != ItemKind::Feature {
            return FeatureProgress::default();
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

        let mut work_items_done = 0usize;
        let mut work_items_total = 0usize;
        for wi_id in work_item_ids {
            if let Some(wi) = self.item(wi_id)
                && wi.project_id == self.project.id
                && wi.is_active()
                && wi.kind == ItemKind::WorkItem
                && !wi.status.is_cancelled()
            {
                work_items_total += 1;
                if wi.status.is_done() {
                    work_items_done += 1;
                }
            }
        }

        let mut child_features_done = 0usize;
        let mut child_features_total = 0usize;
        for desc in descendant_features {
            if !desc.status.is_cancelled() {
                child_features_total += 1;
                if desc.status.is_done() {
                    child_features_done += 1;
                }
            }
        }

        FeatureProgress {
            work_items_done,
            work_items_total,
            child_features_done,
            child_features_total,
        }
    }

    pub fn compute_progress_map(&self) -> HashMap<ItemId, FeatureProgress> {
        let mut map = HashMap::new();
        for item in &self.items {
            if item.project_id == self.project.id
                && item.is_active()
                && item.kind == ItemKind::Feature
            {
                map.insert(item.id, self.compute_feature_progress(item.id));
            }
        }
        map
    }

    pub fn feature_progress_detail(&self, feature_id: ItemId) -> FeatureProgress {
        self.progress_by_feature
            .get(&feature_id)
            .copied()
            .unwrap_or_else(|| self.compute_feature_progress(feature_id))
    }

    /// Computes deduplicated active progress `(done, total)` across a Feature's subtree (`INV-11`).
    pub fn feature_progress(&self, feature_id: ItemId) -> (usize, usize) {
        self.feature_progress_detail(feature_id).effective_counts()
    }

    pub fn visible_feature_tree_rows(&self, expanded: &HashSet<ItemId>) -> Vec<FeatureTreeRow> {
        let mut rows = Vec::new();
        let mut visited = HashSet::new();
        self.collect_tree_rows(None, 0, expanded, &mut visited, &mut rows);
        rows
    }

    pub fn visible_feature_tree_rows_scoped(
        &self,
        root_feature_id: Option<ItemId>,
        expanded: &HashSet<ItemId>,
    ) -> Vec<FeatureTreeRow> {
        let Some(root_id) = root_feature_id else {
            return self.visible_feature_tree_rows(expanded);
        };
        let Some(root) = self.item(root_id) else {
            return Vec::new();
        };
        if root.project_id != self.project.id
            || !root.is_active()
            || root.kind != ItemKind::Feature
        {
            return Vec::new();
        }
        let has_children = !self.child_features(Some(root_id)).is_empty();
        let is_expanded = expanded.contains(&root_id);
        let mut rows = vec![FeatureTreeRow {
            item_id: root_id,
            depth: 0,
            has_children,
            is_expanded,
        }];
        if has_children && is_expanded {
            let mut visited = HashSet::from([root_id]);
            self.collect_tree_rows(Some(root_id), 1, expanded, &mut visited, &mut rows);
        }
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

    pub fn effective_block_state(&self, item_id: ItemId) -> EffectiveBlockState {
        self.effective_block_state
            .get(&item_id)
            .copied()
            .unwrap_or(EffectiveBlockState::Unblocked)
    }

    pub fn direct_blockers(&self, item_id: ItemId) -> &[BlockerRef] {
        self.direct_blockers_by_item
            .get(&item_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn inherited_blockers(&self, item_id: ItemId) -> &[BlockerRef] {
        self.inherited_blockers_by_item
            .get(&item_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn blocker_warnings(&self, item_id: ItemId) -> &[BlockerWarning] {
        self.blocker_warnings
            .get(&item_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn primary_active_direct_blocker_key(&self, item_id: ItemId) -> Option<&str> {
        self.direct_blockers(item_id)
            .iter()
            .find(|b| b.is_active_block())
            .map(|b| b.blocker_key.as_str())
    }

    pub fn dependents_of_feature(&self, feature_id: ItemId) -> Vec<&Item> {
        let mut seen = HashSet::new();
        let mut items: Vec<&Item> = self
            .item_blockers
            .iter()
            .filter(|edge| {
                edge.project_id == self.project.id
                    && edge.blocker_feature_id == feature_id
                    && seen.insert(edge.blocked_item_id)
            })
            .filter_map(|edge| self.item(edge.blocked_item_id))
            .filter(|item| item.project_id == self.project.id && item.is_active())
            .collect();
        items.sort_by_key(|item| item.item_number);
        items
    }

    /// Evaluates strict Feature ↔ Work Item conversion eligibility (`INV-08`, `INV-09`).
    pub fn conversion_eligibility(&self, item_id: ItemId) -> Option<ConversionEligibility> {
        let item = self.item(item_id)?;
        if !item.is_active() || item.project_id != self.project.id {
            return None;
        }

        match item.kind {
            ItemKind::WorkItem => {
                let attached = self.features_for_work_item(item_id);
                match attached.len() {
                    0 => Some(ConversionEligibility::Allowed {
                        target_kind: ItemKind::Feature,
                        resulting_parent: None,
                        moves_to_unallocated: false,
                    }),
                    1 => {
                        let parent = attached[0];
                        if self.feature_depth(parent.id) + 1 >= MAX_FEATURE_DEPTH {
                            Some(ConversionEligibility::Blocked(vec![
                                ConversionBlockReason::ExceedsMaxFeatureDepth {
                                    max_depth: MAX_FEATURE_DEPTH,
                                },
                            ]))
                        } else {
                            Some(ConversionEligibility::Allowed {
                                target_kind: ItemKind::Feature,
                                resulting_parent: Some(parent.id),
                                moves_to_unallocated: false,
                            })
                        }
                    }
                    n => Some(ConversionEligibility::Blocked(vec![
                        ConversionBlockReason::AttachedToMultipleFeatures(n),
                    ])),
                }
            }
            ItemKind::Feature => {
                let mut reasons = Vec::new();
                let child_count = self.child_features(Some(item_id)).len();
                if child_count > 0 {
                    reasons.push(ConversionBlockReason::HasChildFeatures(child_count));
                }
                let wi_count = self.work_items_for_feature(item_id).len();
                if wi_count > 0 {
                    reasons.push(ConversionBlockReason::HasAttachedWorkItems(wi_count));
                }
                let dep_count = self.dependents_of_feature(item_id).len();
                if dep_count > 0 {
                    reasons.push(ConversionBlockReason::HasActiveDependents(dep_count));
                }

                if !reasons.is_empty() {
                    return Some(ConversionEligibility::Blocked(reasons));
                }

                let resulting_parent = item.parent_feature_id.filter(|pid| {
                    self.item(*pid)
                        .is_some_and(|p| p.is_active() && p.kind == ItemKind::Feature)
                });
                Some(ConversionEligibility::Allowed {
                    target_kind: ItemKind::WorkItem,
                    resulting_parent,
                    moves_to_unallocated: resulting_parent.is_none(),
                })
            }
        }
    }

    /// Formats a descriptive tooltip explaining either the resulting conversion target or exact block reasons.
    pub fn conversion_tooltip(&self, item_id: ItemId) -> String {
        let Some(item) = self.item(item_id) else {
            return "Item not found".to_string();
        };
        let Some(eligibility) = self.conversion_eligibility(item_id) else {
            return "Cannot convert item in Trash".to_string();
        };

        match eligibility {
            ConversionEligibility::Allowed {
                target_kind: ItemKind::Feature,
                resulting_parent: None,
                ..
            } => format!(
                "Convert {} to a root Feature (preserves key, tags, assignee, and blockers)",
                item.item_key
            ),
            ConversionEligibility::Allowed {
                target_kind: ItemKind::Feature,
                resulting_parent: Some(pid),
                ..
            } => {
                let parent_label = self
                    .item(pid)
                    .map(|p| format!("{} {}", p.item_key, p.title))
                    .unwrap_or_else(|| "parent Feature".to_string());
                format!(
                    "Convert {} to a child Feature under {} (preserves key, tags, assignee, and blockers)",
                    item.item_key, parent_label
                )
            }
            ConversionEligibility::Allowed {
                target_kind: ItemKind::WorkItem,
                resulting_parent: None,
                ..
            } => format!(
                "Convert {} to an Unallocated Work Item (preserves key, tags, assignee, and blockers)",
                item.item_key
            ),
            ConversionEligibility::Allowed {
                target_kind: ItemKind::WorkItem,
                resulting_parent: Some(pid),
                ..
            } => {
                let parent_label = self
                    .item(pid)
                    .map(|p| format!("{} {}", p.item_key, p.title))
                    .unwrap_or_else(|| "parent Feature".to_string());
                format!(
                    "Convert {} to a Work Item attached to {} (preserves key, tags, assignee, and blockers)",
                    item.item_key, parent_label
                )
            }
            ConversionEligibility::Blocked(reasons) => {
                let attached_names: Vec<String> = if item.kind == ItemKind::WorkItem {
                    self.features_for_work_item(item_id)
                        .into_iter()
                        .map(|f| f.title.clone())
                        .collect()
                } else {
                    Vec::new()
                };
                let dependent_keys: Vec<String> = if item.kind == ItemKind::Feature {
                    self.dependents_of_feature(item_id)
                        .into_iter()
                        .map(|d| d.item_key.clone())
                        .collect()
                } else {
                    Vec::new()
                };
                format_conversion_block_message(
                    item.kind,
                    &reasons,
                    &attached_names,
                    &dependent_keys,
                )
            }
        }
    }

    /// Evaluates `INV-05` eligibility of adding `candidate_feature_id` as a direct blocker of `blocked_item_id`.
    pub fn blocker_candidate_rejection(
        &self,
        blocked_item_id: ItemId,
        candidate_feature_id: ItemId,
    ) -> Option<BlockerCandidateRejection> {
        if blocked_item_id == candidate_feature_id {
            return Some(BlockerCandidateRejection::SelfReference);
        }

        if self
            .direct_blockers(blocked_item_id)
            .iter()
            .any(|b| b.blocker_id == candidate_feature_id)
        {
            return Some(BlockerCandidateRejection::AlreadyBlockedBy);
        }

        let blocked_item = self.item(blocked_item_id)?;
        let candidate = self.item(candidate_feature_id)?;
        if !blocked_item.is_active()
            || !candidate.is_active()
            || candidate.kind != ItemKind::Feature
            || blocked_item.project_id != self.project.id
            || candidate.project_id != self.project.id
        {
            return Some(BlockerCandidateRejection::WouldCreateCycle);
        }

        if blocked_item.kind == ItemKind::WorkItem {
            let attached = self.features_for_work_item(blocked_item_id);
            if attached.iter().any(|f| f.id == candidate_feature_id) {
                return Some(BlockerCandidateRejection::AttachedFeatureOfWorkItem);
            }
            for f in attached {
                if self.ancestor_feature_ids(f.id).contains(&candidate_feature_id) {
                    return Some(BlockerCandidateRejection::AncestorOfAttachedFeature);
                }
            }
            return None;
        }

        if self
            .ancestor_feature_ids(blocked_item_id)
            .contains(&candidate_feature_id)
        {
            return Some(BlockerCandidateRejection::AncestorOfItem);
        }

        if self
            .descendant_feature_ids(blocked_item_id)
            .contains(&candidate_feature_id)
        {
            return Some(BlockerCandidateRejection::DescendantOfItem);
        }

        if self.combined_graph_reachable(candidate_feature_id, blocked_item_id) {
            return Some(BlockerCandidateRejection::WouldCreateCycle);
        }

        None
    }

    /// Checks whether reparenting `feature_id` under `new_parent_id` violates `INV-05`.
    pub fn reparent_blocker_conflict(
        &self,
        feature_id: ItemId,
        new_parent_id: ItemId,
    ) -> Option<&'static str> {
        let mut new_ancestors = self.ancestor_feature_ids(new_parent_id);
        new_ancestors.insert(new_parent_id);

        let mut moving_subtree = self.descendant_feature_ids(feature_id);
        moving_subtree.insert(feature_id);

        if self.combined_graph_reachable(new_parent_id, feature_id) {
            return Some("Dependency cycle");
        }

        for edge in &self.item_blockers {
            if edge.project_id != self.project.id {
                continue;
            }
            if moving_subtree.contains(&edge.blocked_item_id)
                && new_ancestors.contains(&edge.blocker_feature_id)
            {
                return Some("Blocker conflict");
            }
            if new_ancestors.contains(&edge.blocked_item_id)
                && moving_subtree.contains(&edge.blocker_feature_id)
            {
                return Some("Blocker conflict");
            }
        }

        for link in &self.feature_work_items {
            if link.project_id == self.project.id && moving_subtree.contains(&link.feature_id) {
                for edge in &self.item_blockers {
                    if edge.project_id == self.project.id
                        && edge.blocked_item_id == link.work_item_id
                        && new_ancestors.contains(&edge.blocker_feature_id)
                    {
                        return Some("Work item blocker conflict");
                    }
                }
            }
        }

        None
    }

    /// Checks whether attaching `work_item_id` to `target_feature_id` violates `INV-05`.
    pub fn attach_work_item_blocker_conflict(
        &self,
        work_item_id: ItemId,
        target_feature_id: ItemId,
    ) -> Option<&'static str> {
        let mut target_and_ancestors = self.ancestor_feature_ids(target_feature_id);
        target_and_ancestors.insert(target_feature_id);

        for edge in &self.item_blockers {
            if edge.project_id == self.project.id
                && edge.blocked_item_id == work_item_id
                && target_and_ancestors.contains(&edge.blocker_feature_id)
            {
                return Some("Work Item is blocked by target Feature or an ancestor");
            }
        }
        None
    }

    fn ancestor_feature_ids(&self, feature_id: ItemId) -> HashSet<ItemId> {
        let mut ancestors = HashSet::new();
        let mut visited = HashSet::from([feature_id]);
        let mut cur = self.item(feature_id).and_then(|f| f.parent_feature_id);
        while let Some(pid) = cur {
            if !visited.insert(pid) || ancestors.len() >= MAX_FEATURE_DEPTH {
                break;
            }
            let Some(parent) = self.item(pid) else {
                break;
            };
            if !parent.is_active() || parent.kind != ItemKind::Feature {
                break;
            }
            ancestors.insert(pid);
            cur = parent.parent_feature_id;
        }
        ancestors
    }

    fn combined_graph_reachable(&self, start: ItemId, target: ItemId) -> bool {
        let mut visited = HashSet::new();
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            if node == target {
                return true;
            }
            if !visited.insert(node) {
                continue;
            }
            if let Some(item) = self.item(node)
                && item.is_active()
                && item.kind == ItemKind::Feature
                && let Some(parent_id) = item.parent_feature_id
            {
                stack.push(parent_id);
            }
            for edge in &self.item_blockers {
                if edge.project_id == self.project.id
                    && edge.blocked_item_id == node
                    && let Some(blocker) = self.item(edge.blocker_feature_id)
                    && blocker.is_active()
                    && blocker.kind == ItemKind::Feature
                {
                    stack.push(edge.blocker_feature_id);
                }
            }
        }
        false
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
            progress_by_feature: HashMap::new(),
            item_blockers: Vec::new(),
            direct_blockers_by_item: HashMap::new(),
            inherited_blockers_by_item: HashMap::new(),
            effective_block_state: HashMap::new(),
            blocker_warnings: HashMap::new(),
            trash_count: 0,
            trash_groups: Vec::new(),
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
