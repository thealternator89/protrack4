use chrono::{DateTime, Utc};
use rusqlite::{Transaction, params};

use crate::commands::snapshot_helpers::{
    insert_blocker_row, load_blocker_row, set_item_updated_at,
};
use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::blocker_repo::{add_blocker, remove_blocker, validate_add_blocker};
use crate::db::item_repo::get_item;
use crate::db::{map_sqlite_err, uuid_to_sql};
use crate::domain::{DomainError, DomainEvent, ItemBlocker, ItemId};

#[derive(Debug, Clone)]
struct AddBlockerSnapshot {
    inserted: bool,
    link: ItemBlocker,
    prior_updated_at: DateTime<Utc>,
    forward_updated_at: DateTime<Utc>,
    blocked_key: String,
    blocker_key: String,
}

#[derive(Debug, Clone)]
pub struct AddBlocker {
    pub blocked_item_id: ItemId,
    pub blocker_feature_id: ItemId,
    snapshot: Option<AddBlockerSnapshot>,
}

impl AddBlocker {
    pub fn new(blocked_item_id: ItemId, blocker_feature_id: ItemId) -> Self {
        Self {
            blocked_item_id,
            blocker_feature_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for AddBlocker {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Block {} on {}", snap.blocked_key, snap.blocker_key)
        } else {
            "Add Blocker".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (link, inserted, blocked_key, blocker_key) = if let Some(snap) = &self.snapshot {
            if snap.inserted {
                let _ = validate_add_blocker(tx, self.blocked_item_id, self.blocker_feature_id)?;
                insert_blocker_row(tx, &snap.link)?;
                set_item_updated_at(tx, self.blocked_item_id, snap.forward_updated_at)?;
            }
            (
                snap.link.clone(),
                snap.inserted,
                snap.blocked_key.clone(),
                snap.blocker_key.clone(),
            )
        } else {
            let prior_updated_at = get_item(tx, self.blocked_item_id)?.updated_at;
            let (link, inserted) =
                add_blocker(tx, self.blocked_item_id, self.blocker_feature_id)?;
            let blocked = get_item(tx, self.blocked_item_id)?;
            let blocker = get_item(tx, self.blocker_feature_id)?;
            self.snapshot = Some(AddBlockerSnapshot {
                inserted,
                link: link.clone(),
                prior_updated_at,
                forward_updated_at: blocked.updated_at,
                blocked_key: blocked.item_key.clone(),
                blocker_key: blocker.item_key.clone(),
            });
            (link, inserted, blocked.item_key, blocker.item_key)
        };

        let toast = if inserted {
            Some(format!("Blocked {blocked_key} on {blocker_key}"))
        } else {
            None
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: link.project_id,
            }],
            toast,
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("AddBlocker has not been executed".to_string())
        })?;
        if snap.inserted {
            tx.execute(
                "DELETE FROM item_blockers WHERE blocked_item_id = ?1 AND blocker_feature_id = ?2;",
                params![
                    uuid_to_sql(self.blocked_item_id.0),
                    uuid_to_sql(self.blocker_feature_id.0)
                ],
            )
            .map_err(map_sqlite_err)?;
            set_item_updated_at(tx, self.blocked_item_id, snap.prior_updated_at)?;
        }

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: snap.link.project_id,
            }],
            selected_item_id: Some(self.blocked_item_id),
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct RemoveBlockerSnapshot {
    project_id: crate::domain::ProjectId,
    removed_link: Option<ItemBlocker>,
    prior_updated_at: DateTime<Utc>,
    forward_updated_at: DateTime<Utc>,
    blocked_key: String,
    blocker_key: String,
}

#[derive(Debug, Clone)]
pub struct RemoveBlocker {
    pub blocked_item_id: ItemId,
    pub blocker_feature_id: ItemId,
    snapshot: Option<RemoveBlockerSnapshot>,
}

impl RemoveBlocker {
    pub fn new(blocked_item_id: ItemId, blocker_feature_id: ItemId) -> Self {
        Self {
            blocked_item_id,
            blocker_feature_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for RemoveBlocker {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!(
                "Remove Blocker {} from {}",
                snap.blocker_key, snap.blocked_key
            )
        } else {
            "Remove Blocker".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project_id, removed, blocked_key, blocker_key) = if let Some(snap) = &self.snapshot {
            if snap.removed_link.is_some() {
                tx.execute(
                    "DELETE FROM item_blockers WHERE blocked_item_id = ?1 AND blocker_feature_id = ?2;",
                    params![
                        uuid_to_sql(self.blocked_item_id.0),
                        uuid_to_sql(self.blocker_feature_id.0)
                    ],
                )
                .map_err(map_sqlite_err)?;
                set_item_updated_at(tx, self.blocked_item_id, snap.forward_updated_at)?;
            }
            (
                snap.project_id,
                snap.removed_link.is_some(),
                snap.blocked_key.clone(),
                snap.blocker_key.clone(),
            )
        } else {
            let prior_item = get_item(tx, self.blocked_item_id)?;
            let existing_link =
                load_blocker_row(tx, self.blocked_item_id, self.blocker_feature_id)?;
            let (project_id, removed) =
                remove_blocker(tx, self.blocked_item_id, self.blocker_feature_id)?;
            let after_item = get_item(tx, self.blocked_item_id)?;
            let blocked_key = after_item.item_key.clone();
            let blocker_key = get_item(tx, self.blocker_feature_id)
                .map(|i| i.item_key)
                .unwrap_or_else(|_| "blocker".to_string());
            self.snapshot = Some(RemoveBlockerSnapshot {
                project_id,
                removed_link: if removed { existing_link } else { None },
                prior_updated_at: prior_item.updated_at,
                forward_updated_at: after_item.updated_at,
                blocked_key: blocked_key.clone(),
                blocker_key: blocker_key.clone(),
            });
            (project_id, removed, blocked_key, blocker_key)
        };

        let toast = if removed {
            Some(format!("Removed blocker {blocker_key} from {blocked_key}"))
        } else {
            None
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged { project_id }],
            toast,
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("RemoveBlocker has not been executed".to_string())
        })?;
        if let Some(link) = &snap.removed_link {
            insert_blocker_row(tx, link)?;
            set_item_updated_at(tx, self.blocked_item_id, snap.prior_updated_at)?;
        }

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: snap.project_id,
            }],
            selected_item_id: Some(self.blocked_item_id),
            ..Default::default()
        })
    }
}
