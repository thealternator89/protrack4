use rusqlite::Transaction;

use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::item_repo::{
    empty_project_trash, purge_trash_group, restore_trash_group, trash_item_subtree,
};
use crate::domain::{DomainError, DomainEvent, ItemId, ProjectId};

#[derive(Debug, Clone)]
pub struct TrashItemSubtree {
    pub item_id: ItemId,
}

impl TrashItemSubtree {
    pub fn new(item_id: ItemId) -> Self {
        Self { item_id }
    }
}

impl UndoableCommand for TrashItemSubtree {
    fn label(&self) -> String {
        "Move to Trash".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
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

        Ok(CommandEffect {
            events: vec![
                DomainEvent::ItemsChanged {
                    project_id: res.project_id,
                },
                DomainEvent::TrashChanged {
                    project_id: res.project_id,
                },
            ],
            toast: Some(toast),
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
pub struct RestoreTrashGroup {
    pub trash_root_id: ItemId,
}

impl RestoreTrashGroup {
    pub fn new(trash_root_id: ItemId) -> Self {
        Self { trash_root_id }
    }
}

impl UndoableCommand for RestoreTrashGroup {
    fn label(&self) -> String {
        "Restore from Trash".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let res = restore_trash_group(tx, self.trash_root_id)?;
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

        Ok(CommandEffect {
            events: vec![
                DomainEvent::TrashChanged {
                    project_id: res.project_id,
                },
                DomainEvent::ItemsChanged {
                    project_id: res.project_id,
                },
            ],
            selected_item_id: Some(res.root.id),
            toast: Some(toast),
            warning_toast,
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
}
