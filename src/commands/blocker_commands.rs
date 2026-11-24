use rusqlite::Transaction;

use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::blocker_repo::{add_blocker, remove_blocker};
use crate::db::item_repo::get_item;
use crate::domain::{DomainError, DomainEvent, ItemId};

#[derive(Debug, Clone)]
pub struct AddBlocker {
    pub blocked_item_id: ItemId,
    pub blocker_feature_id: ItemId,
}

impl AddBlocker {
    pub fn new(blocked_item_id: ItemId, blocker_feature_id: ItemId) -> Self {
        Self {
            blocked_item_id,
            blocker_feature_id,
        }
    }
}

impl UndoableCommand for AddBlocker {
    fn label(&self) -> String {
        "Add Blocker".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (link, inserted) = add_blocker(tx, self.blocked_item_id, self.blocker_feature_id)?;
        let blocked = get_item(tx, self.blocked_item_id)?;
        let blocker = get_item(tx, self.blocker_feature_id)?;
        let toast = if inserted {
            Some(format!("Blocked {} on {}", blocked.item_key, blocker.item_key))
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

    fn undo(&mut self, _tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        Err(DomainError::Validation(
            "Undo is not yet enabled (Phase 12)".to_string(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct RemoveBlocker {
    pub blocked_item_id: ItemId,
    pub blocker_feature_id: ItemId,
}

impl RemoveBlocker {
    pub fn new(blocked_item_id: ItemId, blocker_feature_id: ItemId) -> Self {
        Self {
            blocked_item_id,
            blocker_feature_id,
        }
    }
}

impl UndoableCommand for RemoveBlocker {
    fn label(&self) -> String {
        "Remove Blocker".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project_id, removed) =
            remove_blocker(tx, self.blocked_item_id, self.blocker_feature_id)?;
        let blocked_key = get_item(tx, self.blocked_item_id)
            .map(|i| i.item_key)
            .unwrap_or_else(|_| "item".to_string());
        let blocker_key = get_item(tx, self.blocker_feature_id)
            .map(|i| i.item_key)
            .unwrap_or_else(|_| "blocker".to_string());
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

    fn undo(&mut self, _tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        Err(DomainError::Validation(
            "Undo is not yet enabled (Phase 12)".to_string(),
        ))
    }
}
