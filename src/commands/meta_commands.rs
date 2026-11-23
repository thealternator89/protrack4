use rusqlite::Transaction;

use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::project_repo::{CreateProjectInput, UpdateProjectInput, create_project, update_project};
use crate::domain::{DomainError, DomainEvent, ProjectId, ProjectStatus};

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
            created_item_id: None,
            selected_item_id: None,
            toast: Some(format!(
                "Created project {} ({})",
                project.name, project.key_prefix
            )),
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
