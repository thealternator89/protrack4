use rusqlite::Transaction;

use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::assignee_repo::{UpsertAssigneeInput, set_assignee_archived, upsert_assignee};
use crate::db::project_repo::{CreateProjectInput, UpdateProjectInput, create_project, update_project};
use crate::db::tag_repo::{UpsertTagInput, delete_tag, upsert_tag};
use crate::domain::{
    AssigneeId, DomainError, DomainEvent, ProjectId, ProjectStatus, TagId,
};

#[derive(Debug, Clone)]
pub struct CreateProject {
    pub name: String,
    pub key_prefix: String,
    pub description: String,
    pub color: String,
    pub status: ProjectStatus,
    created_id: Option<ProjectId>,
}

impl CreateProject {
    pub fn new(
        name: impl Into<String>,
        key_prefix: impl Into<String>,
        description: impl Into<String>,
        color: impl Into<String>,
        status: ProjectStatus,
    ) -> Self {
        Self {
            name: name.into(),
            key_prefix: key_prefix.into(),
            description: description.into(),
            color: color.into(),
            status,
            created_id: None,
        }
    }
}

impl UndoableCommand for CreateProject {
    fn label(&self) -> String {
        format!("Create Project {}", self.name.trim())
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let project = create_project(
            tx,
            CreateProjectInput {
                name: self.name.clone(),
                key_prefix: self.key_prefix.clone(),
                description: self.description.clone(),
                color: self.color.clone(),
                status: self.status,
            },
        )?;
        self.created_id = Some(project.id);

        Ok(CommandEffect {
            events: vec![DomainEvent::ProjectsChanged],
            created_project_id: Some(project.id),
            toast: Some(format!(
                "Created project {} ({})",
                project.name, project.key_prefix
            )),
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
pub struct UpdateProject {
    pub project_id: ProjectId,
    pub name: String,
    pub key_prefix: String,
    pub description: String,
    pub color: String,
    pub status: ProjectStatus,
}

impl UndoableCommand for UpdateProject {
    fn label(&self) -> String {
        format!("Update Project {}", self.name.trim())
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project, prefix_changed) = update_project(
            tx,
            UpdateProjectInput {
                project_id: self.project_id,
                name: self.name.clone(),
                key_prefix: self.key_prefix.clone(),
                description: self.description.clone(),
                color: self.color.clone(),
                status: self.status,
            },
        )?;

        let mut events = vec![DomainEvent::ProjectsChanged];
        if prefix_changed {
            events.push(DomainEvent::ItemsChanged {
                project_id: project.id,
            });
        }

        let toast = if prefix_changed {
            format!(
                "Updated {} and rewrote item keys to {}-N",
                project.name, project.key_prefix
            )
        } else {
            format!("Updated project {}", project.name)
        };

        Ok(CommandEffect {
            events,
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
pub struct UpsertAssignee {
    pub id: Option<AssigneeId>,
    pub name: String,
    pub handle: String,
    pub color: String,
    pub role: Option<String>,
    pub notes: String,
    created_id: Option<AssigneeId>,
}

impl UpsertAssignee {
    pub fn create(
        name: impl Into<String>,
        handle: impl Into<String>,
        color: impl Into<String>,
        role: Option<String>,
        notes: impl Into<String>,
    ) -> Self {
        Self {
            id: None,
            name: name.into(),
            handle: handle.into(),
            color: color.into(),
            role,
            notes: notes.into(),
            created_id: None,
        }
    }

    pub fn update(
        id: AssigneeId,
        name: impl Into<String>,
        handle: impl Into<String>,
        color: impl Into<String>,
        role: Option<String>,
        notes: impl Into<String>,
    ) -> Self {
        Self {
            id: Some(id),
            name: name.into(),
            handle: handle.into(),
            color: color.into(),
            role,
            notes: notes.into(),
            created_id: None,
        }
    }
}

impl UndoableCommand for UpsertAssignee {
    fn label(&self) -> String {
        if self.id.is_some() {
            format!("Update Assignee {}", self.name.trim())
        } else {
            format!("Create Assignee {}", self.name.trim())
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (assignee, created) = upsert_assignee(
            tx,
            UpsertAssigneeInput {
                id: self.id,
                name: self.name.clone(),
                handle: self.handle.clone(),
                color: self.color.clone(),
                role: self.role.clone(),
                notes: self.notes.clone(),
            },
        )?;
        if created {
            self.created_id = Some(assignee.id);
        }

        let toast = if created {
            format!("Created assignee {} ({})", assignee.name, assignee.handle)
        } else {
            format!("Updated assignee {} ({})", assignee.name, assignee.handle)
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::AssigneesChanged],
            created_assignee_id: created.then_some(assignee.id),
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
pub struct ArchiveAssignee {
    pub assignee_id: AssigneeId,
    pub archived: bool,
}

impl ArchiveAssignee {
    pub fn new(assignee_id: AssigneeId, archived: bool) -> Self {
        Self {
            assignee_id,
            archived,
        }
    }
}

impl UndoableCommand for ArchiveAssignee {
    fn label(&self) -> String {
        if self.archived {
            "Archive Assignee".to_string()
        } else {
            "Restore Assignee".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let assignee = set_assignee_archived(tx, self.assignee_id, self.archived)?;
        let toast = if self.archived {
            format!("Archived assignee {}", assignee.name)
        } else {
            format!("Restored assignee {}", assignee.name)
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::AssigneesChanged],
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
pub struct UpsertTag {
    pub id: Option<TagId>,
    pub project_id: ProjectId,
    pub name: String,
    pub color: String,
    created_id: Option<TagId>,
}

impl UpsertTag {
    pub fn create(
        project_id: ProjectId,
        name: impl Into<String>,
        color: impl Into<String>,
    ) -> Self {
        Self {
            id: None,
            project_id,
            name: name.into(),
            color: color.into(),
            created_id: None,
        }
    }

    pub fn update(
        id: TagId,
        project_id: ProjectId,
        name: impl Into<String>,
        color: impl Into<String>,
    ) -> Self {
        Self {
            id: Some(id),
            project_id,
            name: name.into(),
            color: color.into(),
            created_id: None,
        }
    }
}

impl UndoableCommand for UpsertTag {
    fn label(&self) -> String {
        if self.id.is_some() {
            format!("Update Tag {}", self.name.trim())
        } else {
            format!("Create Tag {}", self.name.trim())
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (tag, created) = upsert_tag(
            tx,
            UpsertTagInput {
                id: self.id,
                project_id: self.project_id,
                name: self.name.clone(),
                color: self.color.clone(),
            },
        )?;
        if created {
            self.created_id = Some(tag.id);
        }

        let toast = if created {
            format!("Created tag '{}'", tag.name)
        } else {
            format!("Updated tag '{}'", tag.name)
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::TagsChanged {
                project_id: tag.project_id,
            }],
            created_tag_id: created.then_some(tag.id),
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
pub struct DeleteTag {
    pub tag_id: TagId,
}

impl DeleteTag {
    pub fn new(tag_id: TagId) -> Self {
        Self { tag_id }
    }
}

impl UndoableCommand for DeleteTag {
    fn label(&self) -> String {
        "Delete Tag".to_string()
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let tag = delete_tag(tx, self.tag_id)?;
        Ok(CommandEffect {
            events: vec![
                DomainEvent::TagsChanged {
                    project_id: tag.project_id,
                },
                DomainEvent::ItemsChanged {
                    project_id: tag.project_id,
                },
            ],
            toast: Some(format!("Deleted tag '{}'", tag.name)),
            ..Default::default()
        })
    }

    fn undo(&mut self, _tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        Err(DomainError::Validation(
            "Undo is not yet enabled (Phase 12)".to_string(),
        ))
    }
}
