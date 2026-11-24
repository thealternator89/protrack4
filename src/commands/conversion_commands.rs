use rusqlite::Transaction;

use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::item_repo::{convert_feature_to_work_item, convert_work_item_to_feature};
use crate::domain::{DomainError, DomainEvent, ItemId};

#[derive(Debug, Clone)]
pub struct ConvertWorkItemToFeature {
    pub work_item_id: ItemId,
}

impl ConvertWorkItemToFeature {
    pub fn new(work_item_id: ItemId) -> Self {
        Self { work_item_id }
    }
}

impl UndoableCommand for ConvertWorkItemToFeature {
    fn label(&self) -> String {
        "Convert Work Item to Feature".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let item = convert_work_item_to_feature(tx, self.work_item_id)?;
        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: item.project_id,
            }],
            selected_item_id: Some(item.id),
            toast: Some(format!("Converted {} to Feature", item.item_key)),
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
pub struct ConvertFeatureToWorkItem {
    pub feature_id: ItemId,
}

impl ConvertFeatureToWorkItem {
    pub fn new(feature_id: ItemId) -> Self {
        Self { feature_id }
    }
}

impl UndoableCommand for ConvertFeatureToWorkItem {
    fn label(&self) -> String {
        "Convert Feature to Work Item".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (item, parent_id) = convert_feature_to_work_item(tx, self.feature_id)?;
        let toast = match parent_id {
            Some(_) => format!("Converted {} to Work Item", item.item_key),
            None => format!("Converted {} to Unallocated Work Item", item.item_key),
        };
        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: item.project_id,
            }],
            selected_item_id: Some(item.id),
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
