use chrono::{DateTime, Utc};
use rusqlite::{Transaction, params};

use crate::commands::snapshot_helpers::{
    FwiSortDiff, ItemSortDiff, apply_fwi_sort_diffs, apply_item_sort_diffs, diff_fwi_sorts,
    diff_item_sorts, insert_blocker_row, insert_fwi_row, load_blockers_for_blocker_feature,
    load_fwi_rows_for_feature, load_fwi_rows_for_project, load_fwi_rows_for_work_item,
    load_item_sort_map,
};
use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::item_repo::{
    check_feature_to_work_item_eligibility, check_work_item_to_feature_eligibility,
    convert_feature_to_work_item, convert_work_item_to_feature, get_item,
};
use crate::db::{map_sqlite_err, parse_uuid_col, ts_to_sql, uuid_to_sql};
use crate::domain::{
    ConversionEligibility, DomainError, DomainEvent, FeatureWorkItem, ItemBlocker, ItemId,
    ProjectId,
};

#[derive(Debug, Clone)]
struct ConvertWiToFeatureSnapshot {
    project_id: ProjectId,
    item_key: String,
    prior_sort_order: String,
    prior_updated_at: DateTime<Utc>,
    deleted_fwi_rows: Vec<FeatureWorkItem>,
    forward_parent_id: Option<ItemId>,
    forward_sort_order: String,
    forward_updated_at: DateTime<Utc>,
    sibling_sort_diffs: Vec<ItemSortDiff>,
}

#[derive(Debug, Clone)]
pub struct ConvertWorkItemToFeature {
    pub work_item_id: ItemId,
    snapshot: Option<ConvertWiToFeatureSnapshot>,
}

impl ConvertWorkItemToFeature {
    pub fn new(work_item_id: ItemId) -> Self {
        Self {
            work_item_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for ConvertWorkItemToFeature {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Convert {} to Feature", snap.item_key)
        } else {
            "Convert Work Item to Feature".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project_id, item_id, item_key) = if let Some(snap) = &self.snapshot {
            if let ConversionEligibility::Blocked(reasons) =
                check_work_item_to_feature_eligibility(tx, self.work_item_id)?
            {
                return Err(DomainError::ConversionBlocked(format!(
                    "{} cannot currently be converted to Feature ({:?})",
                    snap.item_key, reasons
                )));
            }
            let wi_sql = uuid_to_sql(self.work_item_id.0);
            tx.execute(
                "DELETE FROM feature_work_items WHERE work_item_id = ?1;",
                params![&wi_sql],
            )
            .map_err(map_sqlite_err)?;
            apply_item_sort_diffs(tx, &snap.sibling_sort_diffs, false)?;
            tx.execute(
                "UPDATE items
                 SET kind = 'feature',
                     parent_feature_id = ?1,
                     sort_order = ?2,
                     updated_at = ?3
                 WHERE id = ?4;",
                params![
                    snap.forward_parent_id.map(|id| uuid_to_sql(id.0)),
                    &snap.forward_sort_order,
                    ts_to_sql(snap.forward_updated_at),
                    &wi_sql,
                ],
            )
            .map_err(map_sqlite_err)?;
            (snap.project_id, self.work_item_id, snap.item_key.clone())
        } else {
            let prior = get_item(tx, self.work_item_id)?;
            let deleted_fwi_rows = load_fwi_rows_for_work_item(tx, self.work_item_id)?;
            let before_sorts = load_item_sort_map(tx, prior.project_id)?;

            let item = convert_work_item_to_feature(tx, self.work_item_id)?;

            let after_sorts = load_item_sort_map(tx, prior.project_id)?;
            let sibling_sort_diffs =
                diff_item_sorts(&before_sorts, &after_sorts, &[self.work_item_id]);

            self.snapshot = Some(ConvertWiToFeatureSnapshot {
                project_id: item.project_id,
                item_key: item.item_key.clone(),
                prior_sort_order: prior.sort_order,
                prior_updated_at: prior.updated_at,
                deleted_fwi_rows,
                forward_parent_id: item.parent_feature_id,
                forward_sort_order: item.sort_order.clone(),
                forward_updated_at: item.updated_at,
                sibling_sort_diffs,
            });
            (item.project_id, item.id, item.item_key)
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged { project_id }],
            selected_item_id: Some(item_id),
            toast: Some(format!("Converted {item_key} to Feature")),
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("ConvertWorkItemToFeature has not been executed".to_string())
        })?;
        // 1. Flip kind back to work_item first so trg_fwi_kinds_ins passes.
        tx.execute(
            "UPDATE items
             SET kind = 'work_item',
                 parent_feature_id = NULL,
                 sort_order = ?1,
                 updated_at = ?2
             WHERE id = ?3;",
            params![
                &snap.prior_sort_order,
                ts_to_sql(snap.prior_updated_at),
                uuid_to_sql(self.work_item_id.0),
            ],
        )
        .map_err(map_sqlite_err)?;

        // 2. Restore all prior feature_work_items rows (active + stale trashed links).
        for row in &snap.deleted_fwi_rows {
            insert_fwi_row(tx, row)?;
        }

        // 3. Restore any rebalanced sibling features.
        apply_item_sort_diffs(tx, &snap.sibling_sort_diffs, true)?;

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
struct ConvertFeatureToWiSnapshot {
    project_id: ProjectId,
    item_key: String,
    prior_parent_id: Option<ItemId>,
    prior_sort_order: String,
    prior_updated_at: DateTime<Utc>,
    stale_trashed_children: Vec<ItemId>,
    stale_fwi_rows: Vec<FeatureWorkItem>,
    stale_blocker_rows: Vec<ItemBlocker>,
    created_parent_link: Option<FeatureWorkItem>,
    forward_sort_order: String,
    forward_updated_at: DateTime<Utc>,
    wi_sort_diffs: Vec<ItemSortDiff>,
    fwi_sort_diffs: Vec<FwiSortDiff>,
}

#[derive(Debug, Clone)]
pub struct ConvertFeatureToWorkItem {
    pub feature_id: ItemId,
    snapshot: Option<ConvertFeatureToWiSnapshot>,
}

impl ConvertFeatureToWorkItem {
    pub fn new(feature_id: ItemId) -> Self {
        Self {
            feature_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for ConvertFeatureToWorkItem {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Convert {} to Work Item", snap.item_key)
        } else {
            "Convert Feature to Work Item".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project_id, item_id, item_key, has_parent) = if let Some(snap) = &self.snapshot {
            if let ConversionEligibility::Blocked(reasons) =
                check_feature_to_work_item_eligibility(tx, self.feature_id)?
            {
                return Err(DomainError::ConversionBlocked(format!(
                    "{} cannot currently be converted to Work Item ({:?})",
                    snap.item_key, reasons
                )));
            }
            let feat_sql = uuid_to_sql(self.feature_id.0);
            tx.execute(
                "UPDATE items
                 SET parent_feature_id = NULL
                 WHERE parent_feature_id = ?1 AND deleted_at IS NOT NULL;",
                params![&feat_sql],
            )
            .map_err(map_sqlite_err)?;
            tx.execute(
                "DELETE FROM feature_work_items WHERE feature_id = ?1;",
                params![&feat_sql],
            )
            .map_err(map_sqlite_err)?;
            tx.execute(
                "DELETE FROM item_blockers WHERE blocker_feature_id = ?1;",
                params![&feat_sql],
            )
            .map_err(map_sqlite_err)?;

            apply_item_sort_diffs(tx, &snap.wi_sort_diffs, false)?;
            tx.execute(
                "UPDATE items
                 SET kind = 'work_item',
                     parent_feature_id = NULL,
                     sort_order = ?1,
                     updated_at = ?2
                 WHERE id = ?3;",
                params![
                    &snap.forward_sort_order,
                    ts_to_sql(snap.forward_updated_at),
                    &feat_sql,
                ],
            )
            .map_err(map_sqlite_err)?;
            apply_fwi_sort_diffs(tx, &snap.fwi_sort_diffs, false)?;
            if let Some(link) = &snap.created_parent_link {
                insert_fwi_row(tx, link)?;
            }
            (
                snap.project_id,
                self.feature_id,
                snap.item_key.clone(),
                snap.created_parent_link.is_some(),
            )
        } else {
            let prior = get_item(tx, self.feature_id)?;
            let feat_sql = uuid_to_sql(self.feature_id.0);

            let stale_trashed_children: Vec<ItemId> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT id FROM items
                         WHERE parent_feature_id = ?1 AND deleted_at IS NOT NULL
                         ORDER BY item_number ASC;",
                    )
                    .map_err(map_sqlite_err)?;
                let rows = stmt
                    .query_map(params![&feat_sql], |row| {
                        Ok(ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?))
                    })
                    .map_err(map_sqlite_err)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)?
            };
            let stale_fwi_rows = load_fwi_rows_for_feature(tx, self.feature_id)?;
            let stale_blocker_rows = load_blockers_for_blocker_feature(tx, self.feature_id)?;
            let before_sorts = load_item_sort_map(tx, prior.project_id)?;
            let before_fwi = load_fwi_rows_for_project(tx, prior.project_id)?;

            let (item, parent_id) = convert_feature_to_work_item(tx, self.feature_id)?;

            let after_sorts = load_item_sort_map(tx, prior.project_id)?;
            let wi_sort_diffs = diff_item_sorts(&before_sorts, &after_sorts, &[self.feature_id]);

            let after_fwi = load_fwi_rows_for_project(tx, prior.project_id)?;
            let created_parent_link = parent_id.and_then(|pid| {
                after_fwi
                    .iter()
                    .find(|r| r.feature_id == pid && r.work_item_id == self.feature_id)
                    .cloned()
            });
            let fwi_sort_diffs = diff_fwi_sorts(&before_fwi, &after_fwi);

            self.snapshot = Some(ConvertFeatureToWiSnapshot {
                project_id: item.project_id,
                item_key: item.item_key.clone(),
                prior_parent_id: prior.parent_feature_id,
                prior_sort_order: prior.sort_order,
                prior_updated_at: prior.updated_at,
                stale_trashed_children,
                stale_fwi_rows,
                stale_blocker_rows,
                created_parent_link,
                forward_sort_order: item.sort_order.clone(),
                forward_updated_at: item.updated_at,
                wi_sort_diffs,
                fwi_sort_diffs,
            });
            (item.project_id, item.id, item.item_key, parent_id.is_some())
        };

        let toast = if has_parent {
            format!("Converted {item_key} to Work Item")
        } else {
            format!("Converted {item_key} to Unallocated Work Item")
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged { project_id }],
            selected_item_id: Some(item_id),
            toast: Some(toast),
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("ConvertFeatureToWorkItem has not been executed".to_string())
        })?;
        let feat_sql = uuid_to_sql(self.feature_id.0);

        // 1. Delete created parent feature_work_items link BEFORE flipping kind (trg_items_kind_guard_upd).
        if let Some(link) = &snap.created_parent_link {
            tx.execute(
                "DELETE FROM feature_work_items WHERE feature_id = ?1 AND work_item_id = ?2;",
                params![uuid_to_sql(link.feature_id.0), &feat_sql],
            )
            .map_err(map_sqlite_err)?;
        }

        // 2. Flip kind back to feature and restore parent_feature_id, sort_order, updated_at.
        tx.execute(
            "UPDATE items
             SET kind = 'feature',
                 parent_feature_id = ?1,
                 sort_order = ?2,
                 updated_at = ?3
             WHERE id = ?4;",
            params![
                snap.prior_parent_id.map(|id| uuid_to_sql(id.0)),
                &snap.prior_sort_order,
                ts_to_sql(snap.prior_updated_at),
                &feat_sql,
            ],
        )
        .map_err(map_sqlite_err)?;

        // 3. Restore any pruned stale links from trashed rows (feature triggers pass now).
        for child_id in &snap.stale_trashed_children {
            tx.execute(
                "UPDATE items SET parent_feature_id = ?1 WHERE id = ?2;",
                params![&feat_sql, uuid_to_sql(child_id.0)],
            )
            .map_err(map_sqlite_err)?;
        }
        for fwi in &snap.stale_fwi_rows {
            insert_fwi_row(tx, fwi)?;
        }
        for blocker in &snap.stale_blocker_rows {
            insert_blocker_row(tx, blocker)?;
        }

        // 4. Restore any rebalanced sort orders.
        apply_fwi_sort_diffs(tx, &snap.fwi_sort_diffs, true)?;
        apply_item_sort_diffs(tx, &snap.wi_sort_diffs, true)?;

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: snap.project_id,
            }],
            selected_item_id: Some(self.feature_id),
            ..Default::default()
        })
    }
}
