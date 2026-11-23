use std::collections::HashSet;

use rusqlite::Transaction;

use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::item_repo::{
    CreateFeatureInput, CreateWorkItemInput, UpdateItemFieldsInput, attach_work_item_to_feature,
    create_feature, create_work_item, detach_work_item_from_feature, get_item,
    query_features_for_work_item, reparent_feature, reorder_sibling_features,
    set_work_item_features, update_item_fields,
};
use crate::domain::{DomainError, DomainEvent, ItemId, ItemStatus, Priority, ProjectId};

#[derive(Debug, Clone)]
pub struct CreateFeature {
    pub project_id: ProjectId,
    pub parent_feature_id: Option<ItemId>,
    pub title: String,
    pub description: String,
    pub status: ItemStatus,
    pub priority: Priority,
    created_id: Option<ItemId>,
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
            created_id: None,
        }
    }
}

impl UndoableCommand for CreateFeature {
    fn label(&self) -> String {
        format!("Create Feature {}", self.title.trim())
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let item = create_feature(
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
        self.created_id = Some(item.id);

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: item.project_id,
            }],
            created_project_id: None,
            created_item_id: Some(item.id),
            selected_item_id: Some(item.id),
            toast: Some(format!("Created {} · {}", item.item_key, item.title)),
        })
    }

    fn undo(&mut self, _tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        Err(DomainError::Validation(
            "Undo is not yet enabled (Phase 12)".to_string(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct CreateWorkItem {
    pub project_id: ProjectId,
    pub title: String,
    pub description: String,
    pub status: ItemStatus,
    pub priority: Priority,
    pub feature_ids: Vec<ItemId>,
    created_id: Option<ItemId>,
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
            created_id: None,
        }
    }
}

impl UndoableCommand for CreateWorkItem {
    fn label(&self) -> String {
        format!("Create Work Item {}", self.title.trim())
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let item = create_work_item(
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
        self.created_id = Some(item.id);

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: item.project_id,
            }],
            created_project_id: None,
            created_item_id: Some(item.id),
            selected_item_id: Some(item.id),
            toast: Some(format!("Created {} · {}", item.item_key, item.title)),
        })
    }

    fn undo(&mut self, _tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        Err(DomainError::Validation(
            "Undo is not yet enabled (Phase 12)".to_string(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct UpdateItemFields {
    pub item_id: ItemId,
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<ItemStatus>,
    pub priority: Option<Priority>,
}

impl UndoableCommand for UpdateItemFields {
    fn label(&self) -> String {
        "Update Item Fields".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let item = update_item_fields(
            tx,
            UpdateItemFieldsInput {
                item_id: self.item_id,
                title: self.title.clone(),
                description: self.description.clone(),
                status: self.status,
                priority: self.priority,
            },
        )?;

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: item.project_id,
            }],
            created_project_id: None,
            created_item_id: None,
            selected_item_id: None,
            toast: None,
        })
    }

    fn undo(&mut self, _tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        Err(DomainError::Validation(
            "Undo is not yet enabled (Phase 12)".to_string(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct ReparentFeature {
    pub feature_id: ItemId,
    pub new_parent_id: Option<ItemId>,
}

impl ReparentFeature {
    pub fn new(feature_id: ItemId, new_parent_id: Option<ItemId>) -> Self {
        Self {
            feature_id,
            new_parent_id,
        }
    }
}

impl UndoableCommand for ReparentFeature {
    fn label(&self) -> String {
        "Move Feature".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let item = reparent_feature(tx, self.feature_id, self.new_parent_id)?;

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: item.project_id,
            }],
            created_project_id: None,
            created_item_id: None,
            selected_item_id: Some(item.id),
            toast: Some(format!("Updated parent for {}", item.item_key)),
        })
    }

    fn undo(&mut self, _tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        Err(DomainError::Validation(
            "Undo is not yet enabled (Phase 12)".to_string(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct ReorderSiblingFeatures {
    pub project_id: ProjectId,
    pub feature_id: ItemId,
    pub before_sibling_id: Option<ItemId>,
    pub after_sibling_id: Option<ItemId>,
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
        }
    }
}

impl UndoableCommand for ReorderSiblingFeatures {
    fn label(&self) -> String {
        "Reorder Sibling Features".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let item = reorder_sibling_features(
            tx,
            self.project_id,
            self.feature_id,
            self.before_sibling_id,
            self.after_sibling_id,
        )?;

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: item.project_id,
            }],
            created_project_id: None,
            created_item_id: None,
            selected_item_id: Some(item.id),
            toast: None,
        })
    }

    fn undo(&mut self, _tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        Err(DomainError::Validation(
            "Undo is not yet enabled (Phase 12)".to_string(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct SetWorkItemFeatures {
    pub work_item_id: ItemId,
    pub feature_ids: Vec<ItemId>,
}

impl SetWorkItemFeatures {
    pub fn new(work_item_id: ItemId, feature_ids: Vec<ItemId>) -> Self {
        Self {
            work_item_id,
            feature_ids,
        }
    }
}

impl UndoableCommand for SetWorkItemFeatures {
    fn label(&self) -> String {
        if self.feature_ids.is_empty() {
            "Unallocate Work Item".to_string()
        } else {
            "Update Feature Attachments".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let item = set_work_item_features(tx, self.work_item_id, &self.feature_ids)?;
        let unique_count = self.feature_ids.iter().copied().collect::<HashSet<_>>().len();
        let toast = if unique_count == 0 {
            format!("Moved {} to Unallocated", item.item_key)
        } else {
            format!("Assigned {} to {unique_count} feature(s)", item.item_key)
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: item.project_id,
            }],
            created_project_id: None,
            created_item_id: None,
            selected_item_id: None,
            toast: Some(toast),
        })
    }

    fn undo(&mut self, _tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        Err(DomainError::Validation(
            "Undo is not yet enabled (Phase 12)".to_string(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct AttachWorkItemToFeature {
    pub work_item_id: ItemId,
    pub feature_id: ItemId,
}

impl AttachWorkItemToFeature {
    pub fn new(work_item_id: ItemId, feature_id: ItemId) -> Self {
        Self {
            work_item_id,
            feature_id,
        }
    }
}

impl UndoableCommand for AttachWorkItemToFeature {
    fn label(&self) -> String {
        "Attach Work Item to Feature".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let link = attach_work_item_to_feature(tx, self.work_item_id, self.feature_id)?;
        let wi = get_item(tx, self.work_item_id)?;
        let feat = get_item(tx, self.feature_id)?;

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: link.project_id,
            }],
            created_project_id: None,
            created_item_id: None,
            selected_item_id: None,
            toast: Some(format!("Attached {} to {}", wi.item_key, feat.item_key)),
        })
    }

    fn undo(&mut self, _tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        Err(DomainError::Validation(
            "Undo is not yet enabled (Phase 12)".to_string(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct DetachWorkItemFromFeature {
    pub work_item_id: ItemId,
    pub feature_id: ItemId,
}

impl DetachWorkItemFromFeature {
    pub fn new(work_item_id: ItemId, feature_id: ItemId) -> Self {
        Self {
            work_item_id,
            feature_id,
        }
    }
}

impl UndoableCommand for DetachWorkItemFromFeature {
    fn label(&self) -> String {
        "Detach Work Item from Feature".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let _removed = detach_work_item_from_feature(tx, self.work_item_id, self.feature_id)?;
        let wi = get_item(tx, self.work_item_id)?;
        let feat_key = get_item(tx, self.feature_id)
            .map(|f| f.item_key)
            .unwrap_or_else(|_| "Feature".to_string());
        let remaining = query_features_for_work_item(tx, self.work_item_id)?.len();
        let toast = if remaining == 0 {
            format!("Moved {} to Unallocated", wi.item_key)
        } else {
            format!("Detached {} from {}", wi.item_key, feat_key)
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::ItemsChanged {
                project_id: wi.project_id,
            }],
            created_project_id: None,
            created_item_id: None,
            selected_item_id: None,
            toast: Some(toast),
        })
    }

    fn undo(&mut self, _tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        Err(DomainError::Validation(
            "Undo is not yet enabled (Phase 12)".to_string(),
        ))
    }
}
