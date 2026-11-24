use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::{Transaction, params};

use crate::commands::snapshot_helpers::{
    FwiSortDiff, ItemSortDiff, ItemTagRow, apply_fwi_sort_diffs, apply_item_sort_diffs,
    delete_fwi_row, diff_fwi_sorts, diff_item_sorts, insert_exact_item_row, insert_fwi_row,
    insert_item_tag_row, load_fwi_rows_for_feature, load_fwi_rows_for_project,
    load_fwi_rows_for_work_item, load_item_sort_map, load_item_tags_for_item, set_item_updated_at,
};
use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::blocker_repo::{
    query_active_blocker_keys_for_item, validate_attach_work_item_blockers,
    validate_reparent_blockers,
};
use crate::db::item_repo::{
    CreateFeatureInput, CreateWorkItemInput, UpdateItemFieldsInput, attach_work_item_to_feature,
    create_feature, create_work_item, detach_work_item_from_feature, feature_depth,
    feature_subtree_height, get_item, is_descendant_of, move_work_item_between_features,
    query_features_for_work_item, reorder_feature_work_items, reorder_sibling_features,
    reorder_unallocated_work_items, reparent_feature, set_work_item_features, update_item_fields,
};
use crate::db::project_repo::get_project;
use crate::db::tag_repo::set_item_tags;
use crate::db::{map_sqlite_err, ts_to_sql, uuid_to_sql};
use crate::domain::{
    AssigneeId, DomainError, DomainEvent, FeatureWorkItem, Item, ItemId, ItemKind, ItemStatus,
    MAX_FEATURE_DEPTH, Priority, ProjectId, TagId,
};

#[derive(Debug, Clone)]
struct CreateFeatureSnapshot {
    created_item: Item,
    inserted_tags: Vec<ItemTagRow>,
    prior_next_item_seq: i64,
    prior_project_updated_at: DateTime<Utc>,
    forward_project_updated_at: DateTime<Utc>,
    sibling_sort_diffs: Vec<ItemSortDiff>,
}

#[derive(Debug, Clone)]
pub struct CreateFeature {
    pub project_id: ProjectId,
    pub parent_feature_id: Option<ItemId>,
    pub title: String,
    pub description: String,
    pub status: ItemStatus,
    pub priority: Priority,
    pub assignee_id: Option<AssigneeId>,
    pub tag_ids: Vec<TagId>,
    created_id: Option<ItemId>,
    snapshot: Option<CreateFeatureSnapshot>,
}

impl CreateFeature {
    pub fn new(
        project_id: ProjectId,
        parent_feature_id: Option<ItemId>,
        title: impl Into<String>,
        description: impl Into<String>,
        status: ItemStatus,
        priority: Priority,
    ) -> Self {
        Self {
            project_id,
            parent_feature_id,
            title: title.into(),
            description: description.into(),
            status,
            priority,
            assignee_id: None,
            tag_ids: Vec::new(),
            created_id: None,
            snapshot: None,
        }
    }

    pub fn with_assignee(mut self, assignee_id: Option<AssigneeId>) -> Self {
        self.assignee_id = assignee_id;
        self
    }

    pub fn with_tags(mut self, tag_ids: Vec<TagId>) -> Self {
        self.tag_ids = tag_ids;
        self
    }
}

impl UndoableCommand for CreateFeature {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Create Feature {}", snap.created_item.item_key)
        } else {
            format!("Create Feature {}", self.title.trim())
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let item = if let Some(snap) = &self.snapshot {
            if let Some(pid) = snap.created_item.parent_feature_id {
                let parent = get_item(tx, pid)?;
                if parent.project_id != snap.created_item.project_id
                    || !parent.is_active()
                    || parent.kind != ItemKind::Feature
                {
                    return Err(DomainError::Validation(
                        "Cannot redo feature creation because parent feature is unavailable"
                            .to_string(),
                    ));
                }
                if feature_depth(tx, pid)? + 1 >= MAX_FEATURE_DEPTH {
                    return Err(DomainError::MaxFeatureDepthExceeded {
                        max_depth: MAX_FEATURE_DEPTH,
                    });
                }
            }
            apply_item_sort_diffs(tx, &snap.sibling_sort_diffs, false)?;
            insert_exact_item_row(tx, &snap.created_item)?;
            for row in &snap.inserted_tags {
                insert_item_tag_row(tx, row)?;
            }
            let current_proj = get_project(tx, snap.created_item.project_id)?;
            if current_proj.next_item_seq <= snap.created_item.item_number {
                tx.execute(
                    "UPDATE projects SET next_item_seq = ?1, updated_at = ?2 WHERE id = ?3;",
                    params![
                        snap.created_item.item_number + 1,
                        ts_to_sql(snap.forward_project_updated_at),
                        uuid_to_sql(snap.created_item.project_id.0),
                    ],
                )
                .map_err(map_sqlite_err)?;
            }
            snap.created_item.clone()
        } else {
            let prior_proj = get_project(tx, self.project_id)?;
            let before_sorts = load_item_sort_map(tx, self.project_id)?;

            let mut item = create_feature(
                tx,
                CreateFeatureInput {
                    project_id: self.project_id,
                    parent_feature_id: self.parent_feature_id,
                    title: self.title.clone(),
                    description: self.description.clone(),
                    status: self.status,
                    priority: self.priority,
                },
            )?;
            if self.assignee_id.is_some() {
                item = update_item_fields(
                    tx,
                    UpdateItemFieldsInput {
                        item_id: item.id,
                        assignee_id: Some(self.assignee_id),
                        ..Default::default()
                    },
                )?;
            }
            if !self.tag_ids.is_empty() {
                item = set_item_tags(tx, item.id, &self.tag_ids)?;
            }

            let after_proj = get_project(tx, self.project_id)?;
            let after_sorts = load_item_sort_map(tx, self.project_id)?;
            let sibling_sort_diffs = diff_item_sorts(&before_sorts, &after_sorts, &[item.id]);
            let inserted_tags = load_item_tags_for_item(tx, item.id)?;

            self.created_id = Some(item.id);
            self.snapshot = Some(CreateFeatureSnapshot {
                created_item: item.clone(),
                inserted_tags,
                prior_next_item_seq: prior_proj.next_item_seq,
                prior_project_updated_at: prior_proj.updated_at,
                forward_project_updated_at: after_proj.updated_at,
                sibling_sort_diffs,
            });
            item
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: item.project_id,
            }],
            created_item_id: Some(item.id),
            selected_item_id: Some(item.id),
            toast: Some(format!("Created {} · {}", item.item_key, item.title)),
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("CreateFeature has not been executed".to_string())
        })?;
        tx.execute(
            "DELETE FROM items WHERE id = ?1;",
            params![uuid_to_sql(snap.created_item.id.0)],
        )
        .map_err(map_sqlite_err)?;

        let current_proj = get_project(tx, snap.created_item.project_id)?;
        if current_proj.next_item_seq == snap.created_item.item_number + 1 {
            tx.execute(
                "UPDATE projects SET next_item_seq = ?1, updated_at = ?2 WHERE id = ?3;",
                params![
                    snap.prior_next_item_seq,
                    ts_to_sql(snap.prior_project_updated_at),
                    uuid_to_sql(snap.created_item.project_id.0),
                ],
            )
            .map_err(map_sqlite_err)?;
        }

        apply_item_sort_diffs(tx, &snap.sibling_sort_diffs, true)?;

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: snap.created_item.project_id,
            }],
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct CreateWorkItemSnapshot {
    created_item: Item,
    inserted_fwi: Vec<FeatureWorkItem>,
    inserted_tags: Vec<ItemTagRow>,
    prior_next_item_seq: i64,
    prior_project_updated_at: DateTime<Utc>,
    forward_project_updated_at: DateTime<Utc>,
    item_sort_diffs: Vec<ItemSortDiff>,
    fwi_sort_diffs: Vec<FwiSortDiff>,
}

#[derive(Debug, Clone)]
pub struct CreateWorkItem {
    pub project_id: ProjectId,
    pub title: String,
    pub description: String,
    pub status: ItemStatus,
    pub priority: Priority,
    pub feature_ids: Vec<ItemId>,
    pub assignee_id: Option<AssigneeId>,
    pub tag_ids: Vec<TagId>,
    created_id: Option<ItemId>,
    snapshot: Option<CreateWorkItemSnapshot>,
}

impl CreateWorkItem {
    pub fn new(
        project_id: ProjectId,
        title: impl Into<String>,
        description: impl Into<String>,
        status: ItemStatus,
        priority: Priority,
        feature_ids: Vec<ItemId>,
    ) -> Self {
        Self {
            project_id,
            title: title.into(),
            description: description.into(),
            status,
            priority,
            feature_ids,
            assignee_id: None,
            tag_ids: Vec::new(),
            created_id: None,
            snapshot: None,
        }
    }

    pub fn with_assignee(mut self, assignee_id: Option<AssigneeId>) -> Self {
        self.assignee_id = assignee_id;
        self
    }

    pub fn with_tags(mut self, tag_ids: Vec<TagId>) -> Self {
        self.tag_ids = tag_ids;
        self
    }
}

impl UndoableCommand for CreateWorkItem {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Create Work Item {}", snap.created_item.item_key)
        } else {
            format!("Create Work Item {}", self.title.trim())
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let item = if let Some(snap) = &self.snapshot {
            for link in &snap.inserted_fwi {
                let feat = get_item(tx, link.feature_id)?;
                if feat.project_id != snap.created_item.project_id
                    || !feat.is_active()
                    || feat.kind != ItemKind::Feature
                {
                    return Err(DomainError::Validation(
                        "Cannot redo work item creation because attached feature is unavailable"
                            .to_string(),
                    ));
                }
            }
            apply_item_sort_diffs(tx, &snap.item_sort_diffs, false)?;
            apply_fwi_sort_diffs(tx, &snap.fwi_sort_diffs, false)?;
            insert_exact_item_row(tx, &snap.created_item)?;
            for link in &snap.inserted_fwi {
                insert_fwi_row(tx, link)?;
            }
            for row in &snap.inserted_tags {
                insert_item_tag_row(tx, row)?;
            }
            let current_proj = get_project(tx, snap.created_item.project_id)?;
            if current_proj.next_item_seq <= snap.created_item.item_number {
                tx.execute(
                    "UPDATE projects SET next_item_seq = ?1, updated_at = ?2 WHERE id = ?3;",
                    params![
                        snap.created_item.item_number + 1,
                        ts_to_sql(snap.forward_project_updated_at),
                        uuid_to_sql(snap.created_item.project_id.0),
                    ],
                )
                .map_err(map_sqlite_err)?;
            }
            snap.created_item.clone()
        } else {
            let prior_proj = get_project(tx, self.project_id)?;
            let before_sorts = load_item_sort_map(tx, self.project_id)?;
            let before_fwi = load_fwi_rows_for_project(tx, self.project_id)?;

            let mut item = create_work_item(
                tx,
                CreateWorkItemInput {
                    project_id: self.project_id,
                    title: self.title.clone(),
                    description: self.description.clone(),
                    status: self.status,
                    priority: self.priority,
                    feature_ids: self.feature_ids.clone(),
                },
            )?;
            if self.assignee_id.is_some() {
                item = update_item_fields(
                    tx,
                    UpdateItemFieldsInput {
                        item_id: item.id,
                        assignee_id: Some(self.assignee_id),
                        ..Default::default()
                    },
                )?;
            }
            if !self.tag_ids.is_empty() {
                item = set_item_tags(tx, item.id, &self.tag_ids)?;
            }

            let after_proj = get_project(tx, self.project_id)?;
            let after_sorts = load_item_sort_map(tx, self.project_id)?;
            let after_fwi = load_fwi_rows_for_project(tx, self.project_id)?;

            let item_sort_diffs = diff_item_sorts(&before_sorts, &after_sorts, &[item.id]);
            let inserted_fwi = load_fwi_rows_for_work_item(tx, item.id)?;
            let fwi_sort_diffs = diff_fwi_sorts(&before_fwi, &after_fwi);
            let inserted_tags = load_item_tags_for_item(tx, item.id)?;

            self.created_id = Some(item.id);
            self.snapshot = Some(CreateWorkItemSnapshot {
                created_item: item.clone(),
                inserted_fwi,
                inserted_tags,
                prior_next_item_seq: prior_proj.next_item_seq,
                prior_project_updated_at: prior_proj.updated_at,
                forward_project_updated_at: after_proj.updated_at,
                item_sort_diffs,
                fwi_sort_diffs,
            });
            item
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: item.project_id,
            }],
            created_item_id: Some(item.id),
            selected_item_id: Some(item.id),
            toast: Some(format!("Created {} · {}", item.item_key, item.title)),
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("CreateWorkItem has not been executed".to_string())
        })?;
        tx.execute(
            "DELETE FROM items WHERE id = ?1;",
            params![uuid_to_sql(snap.created_item.id.0)],
        )
        .map_err(map_sqlite_err)?;

        let current_proj = get_project(tx, snap.created_item.project_id)?;
        if current_proj.next_item_seq == snap.created_item.item_number + 1 {
            tx.execute(
                "UPDATE projects SET next_item_seq = ?1, updated_at = ?2 WHERE id = ?3;",
                params![
                    snap.prior_next_item_seq,
                    ts_to_sql(snap.prior_project_updated_at),
                    uuid_to_sql(snap.created_item.project_id.0),
                ],
            )
            .map_err(map_sqlite_err)?;
        }

        apply_fwi_sort_diffs(tx, &snap.fwi_sort_diffs, true)?;
        apply_item_sort_diffs(tx, &snap.item_sort_diffs, true)?;

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: snap.created_item.project_id,
            }],
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct UpdateItemFieldsSnapshot {
    project_id: ProjectId,
    item_key: String,
    prior_title: String,
    prior_description: String,
    prior_status: ItemStatus,
    prior_priority: Priority,
    prior_assignee_id: Option<AssigneeId>,
    prior_updated_at: DateTime<Utc>,
    forward_title: String,
    forward_description: String,
    forward_status: ItemStatus,
    forward_priority: Priority,
    forward_assignee_id: Option<AssigneeId>,
    forward_updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct UpdateItemFields {
    pub item_id: ItemId,
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<ItemStatus>,
    pub priority: Option<Priority>,
    pub assignee_id: Option<Option<AssigneeId>>,
    snapshot: Option<UpdateItemFieldsSnapshot>,
}

impl UpdateItemFields {
    pub fn new(item_id: ItemId) -> Self {
        Self {
            item_id,
            title: None,
            description: None,
            status: None,
            priority: None,
            assignee_id: None,
            snapshot: None,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_status(mut self, status: ItemStatus) -> Self {
        self.status = Some(status);
        self
    }

    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = Some(priority);
        self
    }

    pub fn with_assignee_id(mut self, assignee_id: Option<AssigneeId>) -> Self {
        self.assignee_id = Some(assignee_id);
        self
    }
}

fn compute_in_progress_warning_toast(
    tx: &Transaction<'_>,
    project_id: ProjectId,
    item_id: ItemId,
    item_key: &str,
    previous_status: ItemStatus,
    new_status: ItemStatus,
) -> Result<Option<String>, DomainError> {
    if new_status == ItemStatus::InProgress && previous_status != ItemStatus::InProgress {
        let (block_state, blocker_keys) =
            query_active_blocker_keys_for_item(tx, project_id, item_id)?;
        if block_state.is_blocked() && !blocker_keys.is_empty() {
            let preview = if blocker_keys.len() > 3 {
                format!(
                    "{} (+{} more)",
                    blocker_keys[..3].join(", "),
                    blocker_keys.len() - 3
                )
            } else {
                blocker_keys.join(", ")
            };
            return Ok(Some(format!(
                "Warning: {item_key} is currently blocked by {preview}"
            )));
        }
    }
    Ok(None)
}

impl UndoableCommand for UpdateItemFields {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Edit {}", snap.item_key)
        } else {
            "Update Item Fields".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project_id, warning_toast) = if let Some(snap) = &self.snapshot {
            let existing = get_item(tx, self.item_id)?;
            if !existing.is_active() {
                return Err(DomainError::Validation(
                    "Cannot edit an item in Trash".to_string(),
                ));
            }
            tx.execute(
                "UPDATE items
                 SET title = ?1,
                     description = ?2,
                     status = ?3,
                     priority = ?4,
                     assignee_id = ?5,
                     updated_at = ?6
                 WHERE id = ?7;",
                params![
                    &snap.forward_title,
                    &snap.forward_description,
                    snap.forward_status.as_str(),
                    snap.forward_priority.as_str(),
                    snap.forward_assignee_id.map(|id| uuid_to_sql(id.0)),
                    ts_to_sql(snap.forward_updated_at),
                    uuid_to_sql(self.item_id.0),
                ],
            )
            .map_err(map_sqlite_err)?;

            let warning_toast = compute_in_progress_warning_toast(
                tx,
                snap.project_id,
                self.item_id,
                &snap.item_key,
                existing.status,
                snap.forward_status,
            )?;
            (snap.project_id, warning_toast)
        } else {
            let prior = get_item(tx, self.item_id)?;
            let item = update_item_fields(
                tx,
                UpdateItemFieldsInput {
                    item_id: self.item_id,
                    title: self.title.clone(),
                    description: self.description.clone(),
                    status: self.status,
                    priority: self.priority,
                    assignee_id: self.assignee_id,
                },
            )?;
            let warning_toast = compute_in_progress_warning_toast(
                tx,
                item.project_id,
                item.id,
                &item.item_key,
                prior.status,
                item.status,
            )?;
            self.snapshot = Some(UpdateItemFieldsSnapshot {
                project_id: item.project_id,
                item_key: item.item_key.clone(),
                prior_title: prior.title,
                prior_description: prior.description,
                prior_status: prior.status,
                prior_priority: prior.priority,
                prior_assignee_id: prior.assignee_id,
                prior_updated_at: prior.updated_at,
                forward_title: item.title,
                forward_description: item.description,
                forward_status: item.status,
                forward_priority: item.priority,
                forward_assignee_id: item.assignee_id,
                forward_updated_at: item.updated_at,
            });
            (item.project_id, warning_toast)
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged { project_id }],
            warning_toast,
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("UpdateItemFields has not been executed".to_string())
        })?;
        tx.execute(
            "UPDATE items
             SET title = ?1,
                 description = ?2,
                 status = ?3,
                 priority = ?4,
                 assignee_id = ?5,
                 updated_at = ?6
             WHERE id = ?7;",
            params![
                &snap.prior_title,
                &snap.prior_description,
                snap.prior_status.as_str(),
                snap.prior_priority.as_str(),
                snap.prior_assignee_id.map(|id| uuid_to_sql(id.0)),
                ts_to_sql(snap.prior_updated_at),
                uuid_to_sql(self.item_id.0),
            ],
        )
        .map_err(map_sqlite_err)?;

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: snap.project_id,
            }],
            selected_item_id: Some(self.item_id),
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct SetItemTagsSnapshot {
    project_id: ProjectId,
    item_key: String,
    prior_updated_at: DateTime<Utc>,
    forward_updated_at: DateTime<Utc>,
    prior_rows: Vec<ItemTagRow>,
    forward_rows: Vec<ItemTagRow>,
}

#[derive(Debug, Clone)]
pub struct SetItemTags {
    pub item_id: ItemId,
    pub tag_ids: Vec<TagId>,
    snapshot: Option<SetItemTagsSnapshot>,
}

impl SetItemTags {
    pub fn new(item_id: ItemId, tag_ids: Vec<TagId>) -> Self {
        Self {
            item_id,
            tag_ids,
            snapshot: None,
        }
    }
}

impl UndoableCommand for SetItemTags {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Update Tags on {}", snap.item_key)
        } else {
            "Update Item Tags".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let project_id = if let Some(snap) = &self.snapshot {
            let item = get_item(tx, self.item_id)?;
            if !item.is_active() {
                return Err(DomainError::Validation(
                    "Cannot modify tags of an item in Trash".to_string(),
                ));
            }
            tx.execute(
                "DELETE FROM item_tags WHERE item_id = ?1;",
                params![uuid_to_sql(self.item_id.0)],
            )
            .map_err(map_sqlite_err)?;
            for row in &snap.forward_rows {
                insert_item_tag_row(tx, row)?;
            }
            set_item_updated_at(tx, self.item_id, snap.forward_updated_at)?;
            snap.project_id
        } else {
            let prior = get_item(tx, self.item_id)?;
            let prior_rows = load_item_tags_for_item(tx, self.item_id)?;
            let item = set_item_tags(tx, self.item_id, &self.tag_ids)?;
            let forward_rows = load_item_tags_for_item(tx, self.item_id)?;
            self.snapshot = Some(SetItemTagsSnapshot {
                project_id: item.project_id,
                item_key: item.item_key,
                prior_updated_at: prior.updated_at,
                forward_updated_at: item.updated_at,
                prior_rows,
                forward_rows,
            });
            item.project_id
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged { project_id }],
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("SetItemTags has not been executed".to_string())
        })?;
        tx.execute(
            "DELETE FROM item_tags WHERE item_id = ?1;",
            params![uuid_to_sql(self.item_id.0)],
        )
        .map_err(map_sqlite_err)?;
        for row in &snap.prior_rows {
            insert_item_tag_row(tx, row)?;
        }
        set_item_updated_at(tx, self.item_id, snap.prior_updated_at)?;

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: snap.project_id,
            }],
            selected_item_id: Some(self.item_id),
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct ReparentFeatureSnapshot {
    project_id: ProjectId,
    item_key: String,
    prior_parent_id: Option<ItemId>,
    prior_sort_order: String,
    prior_updated_at: DateTime<Utc>,
    forward_parent_id: Option<ItemId>,
    forward_sort_order: String,
    forward_updated_at: DateTime<Utc>,
    sibling_sort_diffs: Vec<ItemSortDiff>,
}

#[derive(Debug, Clone)]
pub struct ReparentFeature {
    pub feature_id: ItemId,
    pub new_parent_id: Option<ItemId>,
    snapshot: Option<ReparentFeatureSnapshot>,
}

impl ReparentFeature {
    pub fn new(feature_id: ItemId, new_parent_id: Option<ItemId>) -> Self {
        Self {
            feature_id,
            new_parent_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for ReparentFeature {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Move {}", snap.item_key)
        } else {
            "Move Feature".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project_id, item_key) = if let Some(snap) = &self.snapshot {
            let feature = get_item(tx, self.feature_id)?;
            if !feature.is_active() || feature.kind != ItemKind::Feature {
                return Err(DomainError::Validation(
                    "Cannot move Feature".to_string(),
                ));
            }
            if let Some(pid) = snap.forward_parent_id {
                if pid == self.feature_id {
                    return Err(DomainError::Validation(
                        "A Feature cannot be its own parent".to_string(),
                    ));
                }
                let parent = get_item(tx, pid)?;
                if parent.project_id != feature.project_id {
                    return Err(DomainError::CrossProjectReference);
                }
                if !parent.is_active() || parent.kind != ItemKind::Feature {
                    return Err(DomainError::Validation(
                        "Target parent must be an active Feature".to_string(),
                    ));
                }
                if is_descendant_of(tx, pid, self.feature_id)? {
                    return Err(DomainError::DependencyCycle(
                        "Cannot move a Feature under one of its own descendants".to_string(),
                    ));
                }
                if feature_depth(tx, pid)? + 1 + feature_subtree_height(tx, self.feature_id)?
                    >= MAX_FEATURE_DEPTH
                {
                    return Err(DomainError::MaxFeatureDepthExceeded {
                        max_depth: MAX_FEATURE_DEPTH,
                    });
                }
                validate_reparent_blockers(tx, feature.project_id, self.feature_id, pid)?;
            }
            apply_item_sort_diffs(tx, &snap.sibling_sort_diffs, false)?;
            tx.execute(
                "UPDATE items
                 SET parent_feature_id = ?1,
                     sort_order = ?2,
                     updated_at = ?3
                 WHERE id = ?4;",
                params![
                    snap.forward_parent_id.map(|id| uuid_to_sql(id.0)),
                    &snap.forward_sort_order,
                    ts_to_sql(snap.forward_updated_at),
                    uuid_to_sql(self.feature_id.0),
                ],
            )
            .map_err(map_sqlite_err)?;
            (snap.project_id, snap.item_key.clone())
        } else {
            let prior = get_item(tx, self.feature_id)?;
            let before_sorts = load_item_sort_map(tx, prior.project_id)?;
            let item = reparent_feature(tx, self.feature_id, self.new_parent_id)?;
            let after_sorts = load_item_sort_map(tx, item.project_id)?;
            let sibling_sort_diffs =
                diff_item_sorts(&before_sorts, &after_sorts, &[self.feature_id]);

            self.snapshot = Some(ReparentFeatureSnapshot {
                project_id: item.project_id,
                item_key: item.item_key.clone(),
                prior_parent_id: prior.parent_feature_id,
                prior_sort_order: prior.sort_order,
                prior_updated_at: prior.updated_at,
                forward_parent_id: item.parent_feature_id,
                forward_sort_order: item.sort_order.clone(),
                forward_updated_at: item.updated_at,
                sibling_sort_diffs,
            });
            (item.project_id, item.item_key)
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged { project_id }],
            selected_item_id: Some(self.feature_id),
            toast: Some(format!("Updated parent for {item_key}")),
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("ReparentFeature has not been executed".to_string())
        })?;
        tx.execute(
            "UPDATE items
             SET parent_feature_id = ?1,
                 sort_order = ?2,
                 updated_at = ?3
             WHERE id = ?4;",
            params![
                snap.prior_parent_id.map(|id| uuid_to_sql(id.0)),
                &snap.prior_sort_order,
                ts_to_sql(snap.prior_updated_at),
                uuid_to_sql(self.feature_id.0),
            ],
        )
        .map_err(map_sqlite_err)?;
        apply_item_sort_diffs(tx, &snap.sibling_sort_diffs, true)?;

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: snap.project_id,
            }],
            selected_item_id: Some(self.feature_id),
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct ReorderItemsSnapshot {
    item_key: String,
    diffs: Vec<ItemSortDiff>,
}

#[derive(Debug, Clone)]
pub struct ReorderSiblingFeatures {
    pub project_id: ProjectId,
    pub feature_id: ItemId,
    pub before_sibling_id: Option<ItemId>,
    pub after_sibling_id: Option<ItemId>,
    snapshot: Option<ReorderItemsSnapshot>,
}

impl ReorderSiblingFeatures {
    pub fn new(
        project_id: ProjectId,
        feature_id: ItemId,
        before_sibling_id: Option<ItemId>,
        after_sibling_id: Option<ItemId>,
    ) -> Self {
        Self {
            project_id,
            feature_id,
            before_sibling_id,
            after_sibling_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for ReorderSiblingFeatures {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Reorder {}", snap.item_key)
        } else {
            "Reorder Sibling Features".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        if let Some(snap) = &self.snapshot {
            apply_item_sort_diffs(tx, &snap.diffs, false)?;
        } else {
            let before_sorts = load_item_sort_map(tx, self.project_id)?;
            let item = reorder_sibling_features(
                tx,
                self.project_id,
                self.feature_id,
                self.before_sibling_id,
                self.after_sibling_id,
            )?;
            let after_sorts = load_item_sort_map(tx, self.project_id)?;
            let diffs = diff_item_sorts(&before_sorts, &after_sorts, &[]);
            self.snapshot = Some(ReorderItemsSnapshot {
                item_key: item.item_key,
                diffs,
            });
        }

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: self.project_id,
            }],
            selected_item_id: Some(self.feature_id),
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("ReorderSiblingFeatures has not been executed".to_string())
        })?;
        apply_item_sort_diffs(tx, &snap.diffs, true)?;
        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: self.project_id,
            }],
            selected_item_id: Some(self.feature_id),
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct SetWorkItemFeaturesSnapshot {
    project_id: ProjectId,
    item_key: String,
    prior_updated_at: DateTime<Utc>,
    forward_updated_at: DateTime<Utc>,
    deleted_links: Vec<FeatureWorkItem>,
    inserted_links: Vec<FeatureWorkItem>,
    fwi_sort_diffs: Vec<FwiSortDiff>,
    unique_count: usize,
}

#[derive(Debug, Clone)]
pub struct SetWorkItemFeatures {
    pub work_item_id: ItemId,
    pub feature_ids: Vec<ItemId>,
    snapshot: Option<SetWorkItemFeaturesSnapshot>,
}

impl SetWorkItemFeatures {
    pub fn new(work_item_id: ItemId, feature_ids: Vec<ItemId>) -> Self {
        Self {
            work_item_id,
            feature_ids,
            snapshot: None,
        }
    }
}

impl UndoableCommand for SetWorkItemFeatures {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            if snap.unique_count == 0 {
                format!("Unallocate {}", snap.item_key)
            } else {
                format!("Assign Features for {}", snap.item_key)
            }
        } else if self.feature_ids.is_empty() {
            "Unallocate Work Item".to_string()
        } else {
            "Update Feature Attachments".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project_id, item_key, unique_count) = if let Some(snap) = &self.snapshot {
            let wi = get_item(tx, self.work_item_id)?;
            if !wi.is_active() || wi.kind != ItemKind::WorkItem {
                return Err(DomainError::Validation(
                    "Work Item is unavailable".to_string(),
                ));
            }
            for link in &snap.inserted_links {
                let feat = get_item(tx, link.feature_id)?;
                if !feat.is_active() || feat.kind != ItemKind::Feature {
                    return Err(DomainError::Validation(
                        "Target Feature is unavailable".to_string(),
                    ));
                }
                validate_attach_work_item_blockers(tx, self.work_item_id, link.feature_id)?;
            }
            for link in &snap.deleted_links {
                delete_fwi_row(tx, link.feature_id, link.work_item_id)?;
            }
            apply_fwi_sort_diffs(tx, &snap.fwi_sort_diffs, false)?;
            for link in &snap.inserted_links {
                insert_fwi_row(tx, link)?;
            }
            set_item_updated_at(tx, self.work_item_id, snap.forward_updated_at)?;
            (snap.project_id, snap.item_key.clone(), snap.unique_count)
        } else {
            let prior = get_item(tx, self.work_item_id)?;
            let before_project_fwi = load_fwi_rows_for_project(tx, prior.project_id)?;
            let before_wi_fwi = load_fwi_rows_for_work_item(tx, self.work_item_id)?;

            let item = set_work_item_features(tx, self.work_item_id, &self.feature_ids)?;

            let after_project_fwi = load_fwi_rows_for_project(tx, item.project_id)?;
            let after_wi_fwi = load_fwi_rows_for_work_item(tx, self.work_item_id)?;

            let before_fids: HashSet<ItemId> =
                before_wi_fwi.iter().map(|r| r.feature_id).collect();
            let after_fids: HashSet<ItemId> = after_wi_fwi.iter().map(|r| r.feature_id).collect();

            let deleted_links: Vec<FeatureWorkItem> = before_wi_fwi
                .into_iter()
                .filter(|r| !after_fids.contains(&r.feature_id))
                .collect();
            let inserted_links: Vec<FeatureWorkItem> = after_wi_fwi
                .into_iter()
                .filter(|r| !before_fids.contains(&r.feature_id))
                .collect();
            let fwi_sort_diffs = diff_fwi_sorts(&before_project_fwi, &after_project_fwi);
            let unique_count = self.feature_ids.iter().copied().collect::<HashSet<_>>().len();

            self.snapshot = Some(SetWorkItemFeaturesSnapshot {
                project_id: item.project_id,
                item_key: item.item_key.clone(),
                prior_updated_at: prior.updated_at,
                forward_updated_at: item.updated_at,
                deleted_links,
                inserted_links,
                fwi_sort_diffs,
                unique_count,
            });
            (item.project_id, item.item_key, unique_count)
        };

        let toast = if unique_count == 0 {
            format!("Moved {item_key} to Unallocated")
        } else {
            format!("Assigned {item_key} to {unique_count} feature(s)")
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged { project_id }],
            toast: Some(toast),
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("SetWorkItemFeatures has not been executed".to_string())
        })?;
        for link in &snap.inserted_links {
            delete_fwi_row(tx, link.feature_id, link.work_item_id)?;
        }
        apply_fwi_sort_diffs(tx, &snap.fwi_sort_diffs, true)?;
        for link in &snap.deleted_links {
            insert_fwi_row(tx, link)?;
        }
        set_item_updated_at(tx, self.work_item_id, snap.prior_updated_at)?;

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: snap.project_id,
            }],
            selected_item_id: Some(self.work_item_id),
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct AttachWorkItemSnapshot {
    project_id: ProjectId,
    wi_key: String,
    feat_key: String,
    inserted_link: Option<FeatureWorkItem>,
    prior_updated_at: DateTime<Utc>,
    forward_updated_at: DateTime<Utc>,
    fwi_sort_diffs: Vec<FwiSortDiff>,
}

#[derive(Debug, Clone)]
pub struct AttachWorkItemToFeature {
    pub work_item_id: ItemId,
    pub feature_id: ItemId,
    snapshot: Option<AttachWorkItemSnapshot>,
}

impl AttachWorkItemToFeature {
    pub fn new(work_item_id: ItemId, feature_id: ItemId) -> Self {
        Self {
            work_item_id,
            feature_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for AttachWorkItemToFeature {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Attach {} to {}", snap.wi_key, snap.feat_key)
        } else {
            "Attach Work Item to Feature".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project_id, wi_key, feat_key) = if let Some(snap) = &self.snapshot {
            if let Some(link) = &snap.inserted_link {
                validate_attach_work_item_blockers(tx, self.work_item_id, self.feature_id)?;
                apply_fwi_sort_diffs(tx, &snap.fwi_sort_diffs, false)?;
                insert_fwi_row(tx, link)?;
                set_item_updated_at(tx, self.work_item_id, snap.forward_updated_at)?;
            }
            (
                snap.project_id,
                snap.wi_key.clone(),
                snap.feat_key.clone(),
            )
        } else {
            let prior_wi = get_item(tx, self.work_item_id)?;
            let before_fwi = load_fwi_rows_for_feature(tx, self.feature_id)?;
            let already_attached = before_fwi
                .iter()
                .any(|r| r.work_item_id == self.work_item_id);

            let link = attach_work_item_to_feature(tx, self.work_item_id, self.feature_id)?;
            let after_wi = get_item(tx, self.work_item_id)?;
            let feat = get_item(tx, self.feature_id)?;
            let after_fwi = load_fwi_rows_for_feature(tx, self.feature_id)?;
            let fwi_sort_diffs = diff_fwi_sorts(&before_fwi, &after_fwi);

            self.snapshot = Some(AttachWorkItemSnapshot {
                project_id: link.project_id,
                wi_key: after_wi.item_key.clone(),
                feat_key: feat.item_key.clone(),
                inserted_link: (!already_attached).then_some(link),
                prior_updated_at: prior_wi.updated_at,
                forward_updated_at: after_wi.updated_at,
                fwi_sort_diffs,
            });
            (after_wi.project_id, after_wi.item_key, feat.item_key)
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged { project_id }],
            toast: Some(format!("Attached {wi_key} to {feat_key}")),
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("AttachWorkItemToFeature has not been executed".to_string())
        })?;
        if let Some(link) = &snap.inserted_link {
            delete_fwi_row(tx, link.feature_id, link.work_item_id)?;
            apply_fwi_sort_diffs(tx, &snap.fwi_sort_diffs, true)?;
            set_item_updated_at(tx, self.work_item_id, snap.prior_updated_at)?;
        }

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: snap.project_id,
            }],
            selected_item_id: Some(self.work_item_id),
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct DetachWorkItemSnapshot {
    project_id: ProjectId,
    wi_key: String,
    feat_key: String,
    removed_link: Option<FeatureWorkItem>,
    prior_updated_at: DateTime<Utc>,
    forward_updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct DetachWorkItemFromFeature {
    pub work_item_id: ItemId,
    pub feature_id: ItemId,
    snapshot: Option<DetachWorkItemSnapshot>,
}

impl DetachWorkItemFromFeature {
    pub fn new(work_item_id: ItemId, feature_id: ItemId) -> Self {
        Self {
            work_item_id,
            feature_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for DetachWorkItemFromFeature {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Detach {} from {}", snap.wi_key, snap.feat_key)
        } else {
            "Detach Work Item from Feature".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project_id, wi_key, feat_key) = if let Some(snap) = &self.snapshot {
            if let Some(link) = &snap.removed_link {
                delete_fwi_row(tx, link.feature_id, link.work_item_id)?;
                set_item_updated_at(tx, self.work_item_id, snap.forward_updated_at)?;
            }
            (
                snap.project_id,
                snap.wi_key.clone(),
                snap.feat_key.clone(),
            )
        } else {
            let prior_wi = get_item(tx, self.work_item_id)?;
            let existing_link = load_fwi_rows_for_work_item(tx, self.work_item_id)?
                .into_iter()
                .find(|r| r.feature_id == self.feature_id);
            let removed =
                detach_work_item_from_feature(tx, self.work_item_id, self.feature_id)?;
            let after_wi = get_item(tx, self.work_item_id)?;
            let feat_key = get_item(tx, self.feature_id)
                .map(|f| f.item_key)
                .unwrap_or_else(|_| "Feature".to_string());

            self.snapshot = Some(DetachWorkItemSnapshot {
                project_id: after_wi.project_id,
                wi_key: after_wi.item_key.clone(),
                feat_key: feat_key.clone(),
                removed_link: if removed { existing_link } else { None },
                prior_updated_at: prior_wi.updated_at,
                forward_updated_at: after_wi.updated_at,
            });
            (after_wi.project_id, after_wi.item_key, feat_key)
        };

        let remaining = query_features_for_work_item(tx, self.work_item_id)?.len();
        let toast = if remaining == 0 {
            format!("Moved {wi_key} to Unallocated")
        } else {
            format!("Detached {wi_key} from {feat_key}")
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged { project_id }],
            toast: Some(toast),
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("DetachWorkItemFromFeature has not been executed".to_string())
        })?;
        if let Some(link) = &snap.removed_link {
            insert_fwi_row(tx, link)?;
            set_item_updated_at(tx, self.work_item_id, snap.prior_updated_at)?;
        }

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: snap.project_id,
            }],
            selected_item_id: Some(self.work_item_id),
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct ReorderFwiSnapshot {
    wi_key: String,
    prior_updated_at: DateTime<Utc>,
    forward_updated_at: DateTime<Utc>,
    diffs: Vec<FwiSortDiff>,
}

#[derive(Debug, Clone)]
pub struct ReorderFeatureWorkItems {
    pub project_id: ProjectId,
    pub feature_id: ItemId,
    pub work_item_id: ItemId,
    pub before_work_item_id: Option<ItemId>,
    pub after_work_item_id: Option<ItemId>,
    snapshot: Option<ReorderFwiSnapshot>,
}

impl ReorderFeatureWorkItems {
    pub fn new(
        project_id: ProjectId,
        feature_id: ItemId,
        work_item_id: ItemId,
        before_work_item_id: Option<ItemId>,
        after_work_item_id: Option<ItemId>,
    ) -> Self {
        Self {
            project_id,
            feature_id,
            work_item_id,
            before_work_item_id,
            after_work_item_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for ReorderFeatureWorkItems {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Reorder {}", snap.wi_key)
        } else {
            "Reorder Feature Work Items".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        if let Some(snap) = &self.snapshot {
            apply_fwi_sort_diffs(tx, &snap.diffs, false)?;
            set_item_updated_at(tx, self.work_item_id, snap.forward_updated_at)?;
        } else {
            let prior_wi = get_item(tx, self.work_item_id)?;
            let before_fwi = load_fwi_rows_for_feature(tx, self.feature_id)?;
            let _link = reorder_feature_work_items(
                tx,
                self.project_id,
                self.feature_id,
                self.work_item_id,
                self.before_work_item_id,
                self.after_work_item_id,
            )?;
            let after_wi = get_item(tx, self.work_item_id)?;
            let after_fwi = load_fwi_rows_for_feature(tx, self.feature_id)?;
            let diffs = diff_fwi_sorts(&before_fwi, &after_fwi);
            self.snapshot = Some(ReorderFwiSnapshot {
                wi_key: after_wi.item_key,
                prior_updated_at: prior_wi.updated_at,
                forward_updated_at: after_wi.updated_at,
                diffs,
            });
        }

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: self.project_id,
            }],
            selected_item_id: Some(self.work_item_id),
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("ReorderFeatureWorkItems has not been executed".to_string())
        })?;
        apply_fwi_sort_diffs(tx, &snap.diffs, true)?;
        set_item_updated_at(tx, self.work_item_id, snap.prior_updated_at)?;
        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: self.project_id,
            }],
            selected_item_id: Some(self.work_item_id),
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
pub struct ReorderUnallocatedWorkItems {
    pub project_id: ProjectId,
    pub work_item_id: ItemId,
    pub before_work_item_id: Option<ItemId>,
    pub after_work_item_id: Option<ItemId>,
    snapshot: Option<ReorderItemsSnapshot>,
}

impl ReorderUnallocatedWorkItems {
    pub fn new(
        project_id: ProjectId,
        work_item_id: ItemId,
        before_work_item_id: Option<ItemId>,
        after_work_item_id: Option<ItemId>,
    ) -> Self {
        Self {
            project_id,
            work_item_id,
            before_work_item_id,
            after_work_item_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for ReorderUnallocatedWorkItems {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Reorder {}", snap.item_key)
        } else {
            "Reorder Unallocated Work Items".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        if let Some(snap) = &self.snapshot {
            apply_item_sort_diffs(tx, &snap.diffs, false)?;
        } else {
            let before_sorts = load_item_sort_map(tx, self.project_id)?;
            let item = reorder_unallocated_work_items(
                tx,
                self.project_id,
                self.work_item_id,
                self.before_work_item_id,
                self.after_work_item_id,
            )?;
            let after_sorts = load_item_sort_map(tx, self.project_id)?;
            let diffs = diff_item_sorts(&before_sorts, &after_sorts, &[]);
            self.snapshot = Some(ReorderItemsSnapshot {
                item_key: item.item_key,
                diffs,
            });
        }

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: self.project_id,
            }],
            selected_item_id: Some(self.work_item_id),
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation(
                "ReorderUnallocatedWorkItems has not been executed".to_string(),
            )
        })?;
        apply_item_sort_diffs(tx, &snap.diffs, true)?;
        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: self.project_id,
            }],
            selected_item_id: Some(self.work_item_id),
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct MoveWorkItemSnapshot {
    project_id: ProjectId,
    wi_key: String,
    from_key: String,
    to_key: String,
    prior_updated_at: DateTime<Utc>,
    forward_updated_at: DateTime<Utc>,
    inserted_to_link: Option<FeatureWorkItem>,
    removed_from_link: Option<FeatureWorkItem>,
    fwi_sort_diffs: Vec<FwiSortDiff>,
}

#[derive(Debug, Clone)]
pub struct MoveWorkItemBetweenFeatures {
    pub work_item_id: ItemId,
    pub from_feature_id: ItemId,
    pub to_feature_id: ItemId,
    snapshot: Option<MoveWorkItemSnapshot>,
}

impl MoveWorkItemBetweenFeatures {
    pub fn new(work_item_id: ItemId, from_feature_id: ItemId, to_feature_id: ItemId) -> Self {
        Self {
            work_item_id,
            from_feature_id,
            to_feature_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for MoveWorkItemBetweenFeatures {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Move {} to {}", snap.wi_key, snap.to_key)
        } else {
            "Move Work Item Between Features".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project_id, wi_key, from_key, to_key) = if let Some(snap) = &self.snapshot {
            if self.from_feature_id != self.to_feature_id {
                if let Some(link) = &snap.inserted_to_link {
                    validate_attach_work_item_blockers(tx, self.work_item_id, self.to_feature_id)?;
                    apply_fwi_sort_diffs(tx, &snap.fwi_sort_diffs, false)?;
                    insert_fwi_row(tx, link)?;
                }
                if let Some(removed) = &snap.removed_from_link {
                    delete_fwi_row(tx, removed.feature_id, removed.work_item_id)?;
                }
                set_item_updated_at(tx, self.work_item_id, snap.forward_updated_at)?;
            }
            (
                snap.project_id,
                snap.wi_key.clone(),
                snap.from_key.clone(),
                snap.to_key.clone(),
            )
        } else {
            let prior_wi = get_item(tx, self.work_item_id)?;
            let before_to_fwi = load_fwi_rows_for_feature(tx, self.to_feature_id)?;
            let before_from_link = load_fwi_rows_for_work_item(tx, self.work_item_id)?
                .into_iter()
                .find(|r| r.feature_id == self.from_feature_id);
            let was_on_to = before_to_fwi
                .iter()
                .any(|r| r.work_item_id == self.work_item_id);

            let link = move_work_item_between_features(
                tx,
                self.work_item_id,
                self.from_feature_id,
                self.to_feature_id,
            )?;

            let after_wi = get_item(tx, self.work_item_id)?;
            let from_feat = get_item(tx, self.from_feature_id)?;
            let to_feat = get_item(tx, self.to_feature_id)?;
            let after_to_fwi = load_fwi_rows_for_feature(tx, self.to_feature_id)?;

            let inserted_to_link = if !was_on_to {
                after_to_fwi
                    .iter()
                    .find(|r| r.work_item_id == self.work_item_id)
                    .cloned()
            } else {
                None
            };
            let removed_from_link = if self.from_feature_id != self.to_feature_id {
                before_from_link
            } else {
                None
            };
            let fwi_sort_diffs = diff_fwi_sorts(&before_to_fwi, &after_to_fwi);

            self.snapshot = Some(MoveWorkItemSnapshot {
                project_id: link.project_id,
                wi_key: after_wi.item_key.clone(),
                from_key: from_feat.item_key.clone(),
                to_key: to_feat.item_key.clone(),
                prior_updated_at: prior_wi.updated_at,
                forward_updated_at: after_wi.updated_at,
                inserted_to_link,
                removed_from_link,
                fwi_sort_diffs,
            });
            (
                link.project_id,
                after_wi.item_key,
                from_feat.item_key,
                to_feat.item_key,
            )
        };

        let toast = if self.from_feature_id == self.to_feature_id {
            None
        } else {
            Some(format!("Moved {wi_key} from {from_key} to {to_key}"))
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged { project_id }],
            selected_item_id: Some(self.work_item_id),
            toast,
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("MoveWorkItemBetweenFeatures has not been executed".to_string())
        })?;
        if self.from_feature_id != self.to_feature_id {
            if let Some(link) = &snap.inserted_to_link {
                delete_fwi_row(tx, link.feature_id, link.work_item_id)?;
                apply_fwi_sort_diffs(tx, &snap.fwi_sort_diffs, true)?;
            }
            if let Some(removed) = &snap.removed_from_link {
                insert_fwi_row(tx, removed)?;
            }
            set_item_updated_at(tx, self.work_item_id, snap.prior_updated_at)?;
        }

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: snap.project_id,
            }],
            selected_item_id: Some(self.work_item_id),
            ..Default::default()
        })
    }
}
