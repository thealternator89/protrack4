use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::{Transaction, params};

use crate::commands::snapshot_helpers::{
    ItemSortDiff, apply_item_sort_diffs, diff_item_sorts, insert_blocker_row,
    load_blockers_for_project, load_item_sort_map,
};
use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::item_repo::{
    empty_project_trash, get_item, purge_trash_group, restore_trash_group, row_to_item,
    trash_item_subtree,
};
use crate::db::{map_sqlite_err, ts_to_sql, uuid_to_sql};
use crate::domain::{DomainError, DomainEvent, Item, ItemBlocker, ItemId, ProjectId};

#[derive(Debug, Clone)]
struct TrashMemberPrior {
    id: ItemId,
    prior_updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct TrashSubtreeSnapshot {
    project_id: ProjectId,
    root_key: String,
    members: Vec<TrashMemberPrior>,
    forward_deleted_at: DateTime<Utc>,
    toast: String,
}

#[derive(Debug, Clone)]
pub struct TrashItemSubtree {
    pub item_id: ItemId,
    snapshot: Option<TrashSubtreeSnapshot>,
}

impl TrashItemSubtree {
    pub fn new(item_id: ItemId) -> Self {
        Self {
            item_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for TrashItemSubtree {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Move {} to Trash", snap.root_key)
        } else {
            "Move to Trash".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project_id, toast) = if let Some(snap) = &self.snapshot {
            let now_sql = ts_to_sql(snap.forward_deleted_at);
            let root_sql = uuid_to_sql(self.item_id.0);
            for m in &snap.members {
                tx.execute(
                    "UPDATE items
                     SET deleted_at = ?1,
                         trash_root_id = ?2,
                         updated_at = ?1
                     WHERE id = ?3;",
                    params![&now_sql, &root_sql, uuid_to_sql(m.id.0)],
                )
                .map_err(map_sqlite_err)?;
            }
            (snap.project_id, snap.toast.clone())
        } else {
            let root_prior = get_item(tx, self.item_id)?;
            let before_sorts = load_item_sort_map(tx, root_prior.project_id)?;

            let res = trash_item_subtree(tx, self.item_id)?;
            let extra_features = res.trashed_feature_ids.len().saturating_sub(1);
            let extra_wis = if res.trashed_feature_ids.is_empty() {
                0
            } else {
                res.trashed_work_item_ids.len()
            };

            let mut toast = match (extra_features, extra_wis) {
                (0, 0) => format!("Moved {} to Trash", res.root.item_key),
                (f, 0) => format!(
                    "Moved {} (+{f} sub-feature{}) to Trash",
                    res.root.item_key,
                    if f == 1 { "" } else { "s" }
                ),
                (0, w) => format!(
                    "Moved {} (+{w} work item{}) to Trash",
                    res.root.item_key,
                    if w == 1 { "" } else { "s" }
                ),
                (f, w) => format!(
                    "Moved {} (+{f} sub-feature{}, +{w} work item{}) to Trash",
                    res.root.item_key,
                    if f == 1 { "" } else { "s" },
                    if w == 1 { "" } else { "s" }
                ),
            };
            if res.spared_shared_work_item_count > 0 {
                let n = res.spared_shared_work_item_count;
                toast.push_str(&format!(
                    " ({n} shared work item{} kept active)",
                    if n == 1 { "" } else { "s" }
                ));
            }

            let mut members = Vec::new();
            let mut seen = HashSet::new();
            for id in res
                .trashed_feature_ids
                .iter()
                .chain(res.trashed_work_item_ids.iter())
                .copied()
            {
                if seen.insert(id) {
                    let prior_updated_at = before_sorts
                        .get(&id)
                        .map(|(_, ts)| *ts)
                        .unwrap_or(root_prior.updated_at);
                    members.push(TrashMemberPrior {
                        id,
                        prior_updated_at,
                    });
                }
            }

            let forward_deleted_at = res.root.deleted_at.unwrap_or(res.root.updated_at);
            self.snapshot = Some(TrashSubtreeSnapshot {
                project_id: res.project_id,
                root_key: res.root.item_key,
                members,
                forward_deleted_at,
                toast: toast.clone(),
            });
            (res.project_id, toast)
        };

        Ok(CommandEffect {
            events: vec![
                DomainEvent::ItemsChanged { project_id },
                DomainEvent::TrashChanged { project_id },
            ],
            toast: Some(toast),
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("TrashItemSubtree has not been executed".to_string())
        })?;
        for m in &snap.members {
            tx.execute(
                "UPDATE items
                 SET deleted_at = NULL,
                     trash_root_id = NULL,
                     updated_at = ?1
                 WHERE id = ?2;",
                params![ts_to_sql(m.prior_updated_at), uuid_to_sql(m.id.0)],
            )
            .map_err(map_sqlite_err)?;
        }

        Ok(CommandEffect {
            events: vec![
                DomainEvent::ItemsChanged {
                    project_id: snap.project_id,
                },
                DomainEvent::TrashChanged {
                    project_id: snap.project_id,
                },
            ],
            selected_item_id: Some(self.item_id),
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct RestoreMemberState {
    prior: Item,
    forward_parent_id: Option<ItemId>,
    forward_sort_order: String,
    forward_updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct RestoreTrashSnapshot {
    project_id: ProjectId,
    root_key: String,
    members: Vec<RestoreMemberState>,
    pruned_blockers: Vec<ItemBlocker>,
    sibling_sort_diffs: Vec<ItemSortDiff>,
    toast: String,
    warning_toast: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RestoreTrashGroup {
    pub trash_root_id: ItemId,
    snapshot: Option<RestoreTrashSnapshot>,
}

impl RestoreTrashGroup {
    pub fn new(trash_root_id: ItemId) -> Self {
        Self {
            trash_root_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for RestoreTrashGroup {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Restore {} from Trash", snap.root_key)
        } else {
            "Restore from Trash".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project_id, toast, warning_toast) = if let Some(snap) = &self.snapshot {
            for m in &snap.members {
                tx.execute(
                    "UPDATE items
                     SET deleted_at = NULL,
                         trash_root_id = NULL,
                         parent_feature_id = ?1,
                         sort_order = ?2,
                         updated_at = ?3
                     WHERE id = ?4;",
                    params![
                        m.forward_parent_id.map(|id| uuid_to_sql(id.0)),
                        &m.forward_sort_order,
                        ts_to_sql(m.forward_updated_at),
                        uuid_to_sql(m.prior.id.0),
                    ],
                )
                .map_err(map_sqlite_err)?;
            }
            apply_item_sort_diffs(tx, &snap.sibling_sort_diffs, false)?;
            for b in &snap.pruned_blockers {
                tx.execute(
                    "DELETE FROM item_blockers
                     WHERE blocked_item_id = ?1 AND blocker_feature_id = ?2;",
                    params![
                        uuid_to_sql(b.blocked_item_id.0),
                        uuid_to_sql(b.blocker_feature_id.0)
                    ],
                )
                .map_err(map_sqlite_err)?;
            }
            (snap.project_id, snap.toast.clone(), snap.warning_toast.clone())
        } else {
            let root_prior = get_item(tx, self.trash_root_id)?;
            let project_sql = uuid_to_sql(root_prior.project_id.0);
            let root_sql = uuid_to_sql(self.trash_root_id.0);

            let prior_members: Vec<Item> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT id, project_id, item_number, item_key, kind, parent_feature_id,
                                title, description, status, priority, assignee_id, sort_order,
                                deleted_at, trash_root_id, created_at, updated_at, link_url, link_label
                         FROM items
                         WHERE project_id = ?1
                           AND trash_root_id = ?2
                           AND deleted_at IS NOT NULL
                         ORDER BY item_number ASC;",
                    )
                    .map_err(map_sqlite_err)?;
                let rows = stmt
                    .query_map(params![&project_sql, &root_sql], row_to_item)
                    .map_err(map_sqlite_err)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)?
            };
            let member_ids: Vec<ItemId> = prior_members.iter().map(|m| m.id).collect();
            let before_sorts = load_item_sort_map(tx, root_prior.project_id)?;
            let before_blockers = load_blockers_for_project(tx, root_prior.project_id)?;

            let res = restore_trash_group(tx, self.trash_root_id)?;

            let after_sorts = load_item_sort_map(tx, res.project_id)?;
            let after_blockers = load_blockers_for_project(tx, res.project_id)?;

            let sibling_sort_diffs = diff_item_sorts(&before_sorts, &after_sorts, &member_ids);
            let after_blocker_set: HashSet<(ItemId, ItemId)> = after_blockers
                .iter()
                .map(|b| (b.blocked_item_id, b.blocker_feature_id))
                .collect();
            let pruned_blockers: Vec<ItemBlocker> = before_blockers
                .into_iter()
                .filter(|b| !after_blocker_set.contains(&(b.blocked_item_id, b.blocker_feature_id)))
                .collect();

            let mut member_states = Vec::with_capacity(prior_members.len());
            for prior in prior_members {
                let after = get_item(tx, prior.id)?;
                member_states.push(RestoreMemberState {
                    prior,
                    forward_parent_id: after.parent_feature_id,
                    forward_sort_order: after.sort_order,
                    forward_updated_at: after.updated_at,
                });
            }

            let toast = if res.restored_count > 1 {
                format!(
                    "Restored {} ({} items)",
                    res.root.item_key, res.restored_count
                )
            } else {
                format!("Restored {}", res.root.item_key)
            };

            let mut warnings = Vec::new();
            if res.promoted_to_root {
                warnings.push(format!(
                    "{} was restored at project root because its previous parent is unavailable",
                    res.root.item_key
                ));
            }
            if res.pruned_blocker_count > 0 {
                let n = res.pruned_blocker_count;
                warnings.push(format!(
                    "Pruned {n} conflicting blocker link{} to preserve DAG invariants",
                    if n == 1 { "" } else { "s" }
                ));
            }
            let warning_toast = if warnings.is_empty() {
                None
            } else {
                Some(warnings.join(" · "))
            };

            self.snapshot = Some(RestoreTrashSnapshot {
                project_id: res.project_id,
                root_key: res.root.item_key,
                members: member_states,
                pruned_blockers,
                sibling_sort_diffs,
                toast: toast.clone(),
                warning_toast: warning_toast.clone(),
            });

            (res.project_id, toast, warning_toast)
        };

        Ok(CommandEffect {
            events: vec![
                DomainEvent::TrashChanged { project_id },
                DomainEvent::ItemsChanged { project_id },
            ],
            selected_item_id: Some(self.trash_root_id),
            toast: Some(toast),
            warning_toast,
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("RestoreTrashGroup has not been executed".to_string())
        })?;
        for m in &snap.members {
            let p = &m.prior;
            tx.execute(
                "UPDATE items
                 SET deleted_at = ?1,
                     trash_root_id = ?2,
                     parent_feature_id = ?3,
                     sort_order = ?4,
                     updated_at = ?5
                 WHERE id = ?6;",
                params![
                    p.deleted_at.map(ts_to_sql),
                    p.trash_root_id.map(|id| uuid_to_sql(id.0)),
                    p.parent_feature_id.map(|id| uuid_to_sql(id.0)),
                    &p.sort_order,
                    ts_to_sql(p.updated_at),
                    uuid_to_sql(p.id.0),
                ],
            )
            .map_err(map_sqlite_err)?;
        }
        apply_item_sort_diffs(tx, &snap.sibling_sort_diffs, true)?;
        for b in &snap.pruned_blockers {
            insert_blocker_row(tx, b)?;
        }

        Ok(CommandEffect {
            events: vec![
                DomainEvent::TrashChanged {
                    project_id: snap.project_id,
                },
                DomainEvent::ItemsChanged {
                    project_id: snap.project_id,
                },
            ],
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
pub struct PurgeTrashGroup {
    pub trash_root_id: ItemId,
}

impl PurgeTrashGroup {
    pub fn new(trash_root_id: ItemId) -> Self {
        Self { trash_root_id }
    }
}

impl UndoableCommand for PurgeTrashGroup {
    fn label(&self) -> String {
        "Delete Forever".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let res = purge_trash_group(tx, self.trash_root_id)?;
        let toast = if res.purged_item_count > 1 {
            format!(
                "Permanently deleted {} ({} items)",
                res.root_key, res.purged_item_count
            )
        } else {
            format!("Permanently deleted {}", res.root_key)
        };
        Ok(CommandEffect {
            events: vec![
                DomainEvent::TrashChanged {
                    project_id: res.project_id,
                },
                DomainEvent::ItemsChanged {
                    project_id: res.project_id,
                },
            ],
            toast: Some(toast),
            ..Default::default()
        })
    }

    fn undo(&mut self, _tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        Err(DomainError::Validation(
            "Permanent deletion cannot be undone".to_string(),
        ))
    }

    fn clears_undo_history(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct EmptyTrash {
    pub project_id: ProjectId,
}

impl EmptyTrash {
    pub fn new(project_id: ProjectId) -> Self {
        Self { project_id }
    }
}

impl UndoableCommand for EmptyTrash {
    fn label(&self) -> String {
        "Empty Trash".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let res = empty_project_trash(tx, self.project_id)?;
        let toast = if res.purged_item_count == 0 {
            "Trash is already empty".to_string()
        } else {
            format!(
                "Emptied Trash ({} item{} permanently deleted)",
                res.purged_item_count,
                if res.purged_item_count == 1 { "" } else { "s" }
            )
        };
        Ok(CommandEffect {
            events: vec![
                DomainEvent::TrashChanged {
                    project_id: res.project_id,
                },
                DomainEvent::ItemsChanged {
                    project_id: res.project_id,
                },
            ],
            toast: Some(toast),
            ..Default::default()
        })
    }

    fn undo(&mut self, _tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        Err(DomainError::Validation(
            "Empty Trash cannot be undone".to_string(),
        ))
    }

    fn clears_undo_history(&self) -> bool {
        true
    }
}
