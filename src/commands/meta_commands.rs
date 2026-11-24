use chrono::{DateTime, Utc};
use rusqlite::{Transaction, params};

use crate::commands::snapshot_helpers::{
    ItemTagRow, ProjectSortDiff, apply_project_sort_diffs, diff_project_sorts,
    insert_exact_assignee_row, insert_exact_project_row, insert_exact_tag_row, insert_item_tag_row,
    load_item_tags_for_tag, load_project_sort_map,
};
use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::assignee_repo::{
    UpsertAssigneeInput, get_assignee, set_assignee_archived, upsert_assignee,
};
use crate::db::map_sqlite_err;
use crate::db::project_repo::{
    CreateProjectInput, UpdateProjectInput, create_project, get_project, update_project,
};
use crate::db::tag_repo::{UpsertTagInput, delete_tag, get_tag, upsert_tag};
use crate::db::{parse_ts_col, parse_uuid_col, ts_to_sql, uuid_to_sql};
use crate::domain::{
    Assignee, AssigneeId, DomainError, DomainEvent, ItemId, Project, ProjectId, ProjectStatus, Tag,
    TagId,
};

#[derive(Debug, Clone)]
struct CreateProjectSnapshot {
    created_project: Project,
    sort_diffs: Vec<ProjectSortDiff>,
}

#[derive(Debug, Clone)]
pub struct CreateProject {
    pub name: String,
    pub key_prefix: String,
    pub description: String,
    pub color: String,
    pub status: ProjectStatus,
    created_id: Option<ProjectId>,
    snapshot: Option<CreateProjectSnapshot>,
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
            snapshot: None,
        }
    }
}

impl UndoableCommand for CreateProject {
    fn label(&self) -> String {
        format!("Create Project {}", self.name.trim())
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let project = if let Some(snap) = &self.snapshot {
            apply_project_sort_diffs(tx, &snap.sort_diffs, false)?;
            insert_exact_project_row(tx, &snap.created_project)?;
            snap.created_project.clone()
        } else {
            let before = load_project_sort_map(tx)?;
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
            let after = load_project_sort_map(tx)?;
            let sort_diffs = diff_project_sorts(&before, &after, Some(project.id));
            self.created_id = Some(project.id);
            self.snapshot = Some(CreateProjectSnapshot {
                created_project: project.clone(),
                sort_diffs,
            });
            project
        };

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

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("CreateProject has not been executed".to_string())
        })?;
        tx.execute(
            "DELETE FROM projects WHERE id = ?1;",
            params![uuid_to_sql(snap.created_project.id.0)],
        )
        .map_err(map_sqlite_err)?;
        apply_project_sort_diffs(tx, &snap.sort_diffs, true)?;

        Ok(CommandEffect {
            events: vec![DomainEvent::ProjectsChanged],
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct ItemKeySnapshot {
    id: ItemId,
    old_key: String,
    old_updated_at: DateTime<Utc>,
    new_key: String,
    new_updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct UpdateProjectSnapshot {
    prior: Project,
    forward: Project,
    prefix_changed: bool,
    item_key_snapshots: Vec<ItemKeySnapshot>,
}

#[derive(Debug, Clone)]
pub struct UpdateProject {
    pub project_id: ProjectId,
    pub name: String,
    pub key_prefix: String,
    pub description: String,
    pub color: String,
    pub status: ProjectStatus,
    snapshot: Option<UpdateProjectSnapshot>,
}

impl UpdateProject {
    pub fn new(
        project_id: ProjectId,
        name: impl Into<String>,
        key_prefix: impl Into<String>,
        description: impl Into<String>,
        color: impl Into<String>,
        status: ProjectStatus,
    ) -> Self {
        Self {
            project_id,
            name: name.into(),
            key_prefix: key_prefix.into(),
            description: description.into(),
            color: color.into(),
            status,
            snapshot: None,
        }
    }
}

impl UndoableCommand for UpdateProject {
    fn label(&self) -> String {
        format!("Update Project {}", self.name.trim())
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let (project, prefix_changed) = if let Some(snap) = &self.snapshot {
            let f = &snap.forward;
            tx.execute(
                "UPDATE projects
                 SET name = ?1, key_prefix = ?2, description = ?3, color = ?4, status = ?5, updated_at = ?6
                 WHERE id = ?7;",
                params![
                    &f.name,
                    &f.key_prefix,
                    &f.description,
                    &f.color,
                    f.status.as_str(),
                    ts_to_sql(f.updated_at),
                    uuid_to_sql(f.id.0),
                ],
            )
            .map_err(map_sqlite_err)?;
            if snap.prefix_changed {
                for item in &snap.item_key_snapshots {
                    tx.execute(
                        "UPDATE items SET item_key = ?1, updated_at = ?2 WHERE id = ?3;",
                        params![
                            &item.new_key,
                            ts_to_sql(item.new_updated_at),
                            uuid_to_sql(item.id.0)
                        ],
                    )
                    .map_err(map_sqlite_err)?;
                }
            }
            (f.clone(), snap.prefix_changed)
        } else {
            let prior = get_project(tx, self.project_id)?;
            let mut old_items_map = std::collections::HashMap::new();
            {
                let mut stmt = tx
                    .prepare(
                        "SELECT id, item_key, updated_at FROM items WHERE project_id = ?1;",
                    )
                    .map_err(map_sqlite_err)?;
                let rows = stmt
                    .query_map(params![uuid_to_sql(self.project_id.0)], |row| {
                        let id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
                        let key: String = row.get(1)?;
                        let updated_at = parse_ts_col(2, row.get(2)?)?;
                        Ok((id, (key, updated_at)))
                    })
                    .map_err(map_sqlite_err)?;
                for pair in rows {
                    let (id, val) = pair.map_err(map_sqlite_err)?;
                    old_items_map.insert(id, val);
                }
            }

            let (forward, prefix_changed) = update_project(
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

            let mut item_key_snapshots = Vec::new();
            if prefix_changed {
                let mut stmt = tx
                    .prepare(
                        "SELECT id, item_key, updated_at FROM items WHERE project_id = ?1;",
                    )
                    .map_err(map_sqlite_err)?;
                let rows = stmt
                    .query_map(params![uuid_to_sql(self.project_id.0)], |row| {
                        let id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
                        let new_key: String = row.get(1)?;
                        let new_updated_at = parse_ts_col(2, row.get(2)?)?;
                        Ok((id, new_key, new_updated_at))
                    })
                    .map_err(map_sqlite_err)?;
                for row in rows {
                    let (id, new_key, new_updated_at) = row.map_err(map_sqlite_err)?;
                    if let Some((old_key, old_updated_at)) = old_items_map.remove(&id) {
                        item_key_snapshots.push(ItemKeySnapshot {
                            id,
                            old_key,
                            old_updated_at,
                            new_key,
                            new_updated_at,
                        });
                    }
                }
            }

            self.snapshot = Some(UpdateProjectSnapshot {
                prior,
                forward: forward.clone(),
                prefix_changed,
                item_key_snapshots,
            });
            (forward, prefix_changed)
        };

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

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("UpdateProject has not been executed".to_string())
        })?;
        let p = &snap.prior;
        tx.execute(
            "UPDATE projects
             SET name = ?1, key_prefix = ?2, description = ?3, color = ?4, status = ?5, updated_at = ?6
             WHERE id = ?7;",
            params![
                &p.name,
                &p.key_prefix,
                &p.description,
                &p.color,
                p.status.as_str(),
                ts_to_sql(p.updated_at),
                uuid_to_sql(p.id.0),
            ],
        )
        .map_err(map_sqlite_err)?;

        let mut events = vec![DomainEvent::ProjectsChanged];
        if snap.prefix_changed {
            for item in &snap.item_key_snapshots {
                tx.execute(
                    "UPDATE items SET item_key = ?1, updated_at = ?2 WHERE id = ?3;",
                    params![
                        &item.old_key,
                        ts_to_sql(item.old_updated_at),
                        uuid_to_sql(item.id.0)
                    ],
                )
                .map_err(map_sqlite_err)?;
            }
            events.push(DomainEvent::ItemsChanged {
                project_id: p.id,
            });
        }

        Ok(CommandEffect {
            events,
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
enum UpsertAssigneeSnapshot {
    Created(Assignee),
    Updated {
        prior: Assignee,
        forward: Assignee,
    },
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
    snapshot: Option<UpsertAssigneeSnapshot>,
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
            snapshot: None,
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
            snapshot: None,
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
        let (assignee, created) = match &self.snapshot {
            Some(UpsertAssigneeSnapshot::Created(a)) => {
                insert_exact_assignee_row(tx, a)?;
                (a.clone(), true)
            }
            Some(UpsertAssigneeSnapshot::Updated { forward, .. }) => {
                tx.execute(
                    "UPDATE assignees
                     SET name = ?1, handle = ?2, color = ?3, role = ?4, notes = ?5, updated_at = ?6
                     WHERE id = ?7;",
                    params![
                        &forward.name,
                        &forward.handle,
                        &forward.color,
                        &forward.role,
                        &forward.notes,
                        ts_to_sql(forward.updated_at),
                        uuid_to_sql(forward.id.0),
                    ],
                )
                .map_err(map_sqlite_err)?;
                (forward.clone(), false)
            }
            None => {
                let prior = match self.id {
                    Some(aid) => Some(get_assignee(tx, aid)?),
                    None => None,
                };
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
                    self.snapshot = Some(UpsertAssigneeSnapshot::Created(assignee.clone()));
                } else if let Some(prior) = prior {
                    self.snapshot = Some(UpsertAssigneeSnapshot::Updated {
                        prior,
                        forward: assignee.clone(),
                    });
                }
                (assignee, created)
            }
        };

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

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("UpsertAssignee has not been executed".to_string())
        })?;
        match snap {
            UpsertAssigneeSnapshot::Created(a) => {
                tx.execute(
                    "DELETE FROM assignees WHERE id = ?1;",
                    params![uuid_to_sql(a.id.0)],
                )
                .map_err(map_sqlite_err)?;
            }
            UpsertAssigneeSnapshot::Updated { prior, .. } => {
                tx.execute(
                    "UPDATE assignees
                     SET name = ?1, handle = ?2, color = ?3, role = ?4, notes = ?5, archived_at = ?6, updated_at = ?7
                     WHERE id = ?8;",
                    params![
                        &prior.name,
                        &prior.handle,
                        &prior.color,
                        &prior.role,
                        &prior.notes,
                        prior.archived_at.map(ts_to_sql),
                        ts_to_sql(prior.updated_at),
                        uuid_to_sql(prior.id.0),
                    ],
                )
                .map_err(map_sqlite_err)?;
            }
        }

        Ok(CommandEffect {
            events: vec![DomainEvent::AssigneesChanged],
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct ArchiveAssigneeSnapshot {
    name: String,
    prior_archived_at: Option<DateTime<Utc>>,
    prior_updated_at: DateTime<Utc>,
    forward_archived_at: Option<DateTime<Utc>>,
    forward_updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ArchiveAssignee {
    pub assignee_id: AssigneeId,
    pub archived: bool,
    snapshot: Option<ArchiveAssigneeSnapshot>,
}

impl ArchiveAssignee {
    pub fn new(assignee_id: AssigneeId, archived: bool) -> Self {
        Self {
            assignee_id,
            archived,
            snapshot: None,
        }
    }
}

impl UndoableCommand for ArchiveAssignee {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            if self.archived {
                format!("Archive Assignee {}", snap.name)
            } else {
                format!("Restore Assignee {}", snap.name)
            }
        } else if self.archived {
            "Archive Assignee".to_string()
        } else {
            "Restore Assignee".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let name = if let Some(snap) = &self.snapshot {
            tx.execute(
                "UPDATE assignees SET archived_at = ?1, updated_at = ?2 WHERE id = ?3;",
                params![
                    snap.forward_archived_at.map(ts_to_sql),
                    ts_to_sql(snap.forward_updated_at),
                    uuid_to_sql(self.assignee_id.0),
                ],
            )
            .map_err(map_sqlite_err)?;
            snap.name.clone()
        } else {
            let prior = get_assignee(tx, self.assignee_id)?;
            let assignee = set_assignee_archived(tx, self.assignee_id, self.archived)?;
            self.snapshot = Some(ArchiveAssigneeSnapshot {
                name: assignee.name.clone(),
                prior_archived_at: prior.archived_at,
                prior_updated_at: prior.updated_at,
                forward_archived_at: assignee.archived_at,
                forward_updated_at: assignee.updated_at,
            });
            assignee.name
        };

        let toast = if self.archived {
            format!("Archived assignee {name}")
        } else {
            format!("Restored assignee {name}")
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::AssigneesChanged],
            toast: Some(toast),
            ..Default::default()
        })
    }

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("ArchiveAssignee has not been executed".to_string())
        })?;
        tx.execute(
            "UPDATE assignees SET archived_at = ?1, updated_at = ?2 WHERE id = ?3;",
            params![
                snap.prior_archived_at.map(ts_to_sql),
                ts_to_sql(snap.prior_updated_at),
                uuid_to_sql(self.assignee_id.0),
            ],
        )
        .map_err(map_sqlite_err)?;

        Ok(CommandEffect {
            events: vec![DomainEvent::AssigneesChanged],
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
enum UpsertTagSnapshot {
    Created(Tag),
    Updated { prior: Tag, forward: Tag },
}

#[derive(Debug, Clone)]
pub struct UpsertTag {
    pub id: Option<TagId>,
    pub project_id: ProjectId,
    pub name: String,
    pub color: String,
    created_id: Option<TagId>,
    snapshot: Option<UpsertTagSnapshot>,
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
            snapshot: None,
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
            snapshot: None,
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
        let (tag, created) = match &self.snapshot {
            Some(UpsertTagSnapshot::Created(t)) => {
                insert_exact_tag_row(tx, t)?;
                (t.clone(), true)
            }
            Some(UpsertTagSnapshot::Updated { forward, .. }) => {
                tx.execute(
                    "UPDATE tags SET name = ?1, color = ?2 WHERE id = ?3;",
                    params![&forward.name, &forward.color, uuid_to_sql(forward.id.0)],
                )
                .map_err(map_sqlite_err)?;
                (forward.clone(), false)
            }
            None => {
                let prior = match self.id {
                    Some(tid) => Some(get_tag(tx, tid)?),
                    None => None,
                };
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
                    self.snapshot = Some(UpsertTagSnapshot::Created(tag.clone()));
                } else if let Some(prior) = prior {
                    self.snapshot = Some(UpsertTagSnapshot::Updated {
                        prior,
                        forward: tag.clone(),
                    });
                }
                (tag, created)
            }
        };

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

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("UpsertTag has not been executed".to_string())
        })?;
        let project_id = match snap {
            UpsertTagSnapshot::Created(t) => {
                tx.execute("DELETE FROM tags WHERE id = ?1;", params![uuid_to_sql(t.id.0)])
                    .map_err(map_sqlite_err)?;
                t.project_id
            }
            UpsertTagSnapshot::Updated { prior, .. } => {
                tx.execute(
                    "UPDATE tags SET name = ?1, color = ?2 WHERE id = ?3;",
                    params![&prior.name, &prior.color, uuid_to_sql(prior.id.0)],
                )
                .map_err(map_sqlite_err)?;
                prior.project_id
            }
        };

        Ok(CommandEffect {
            events: vec![DomainEvent::TagsChanged { project_id }],
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
struct DeleteTagSnapshot {
    deleted_tag: Tag,
    item_tag_rows: Vec<ItemTagRow>,
}

#[derive(Debug, Clone)]
pub struct DeleteTag {
    pub tag_id: TagId,
    snapshot: Option<DeleteTagSnapshot>,
}

impl DeleteTag {
    pub fn new(tag_id: TagId) -> Self {
        Self {
            tag_id,
            snapshot: None,
        }
    }
}

impl UndoableCommand for DeleteTag {
    fn label(&self) -> String {
        if let Some(snap) = &self.snapshot {
            format!("Delete Tag '{}'", snap.deleted_tag.name)
        } else {
            "Delete Tag".to_string()
        }
    }

    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let tag = if let Some(snap) = &self.snapshot {
            delete_tag(tx, self.tag_id)?;
            snap.deleted_tag.clone()
        } else {
            let item_tag_rows = load_item_tags_for_tag(tx, self.tag_id)?;
            let tag = delete_tag(tx, self.tag_id)?;
            self.snapshot = Some(DeleteTagSnapshot {
                deleted_tag: tag.clone(),
                item_tag_rows,
            });
            tag
        };
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

    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError> {
        let snap = self.snapshot.as_ref().ok_or_else(|| {
            DomainError::Validation("DeleteTag has not been executed".to_string())
        })?;
        insert_exact_tag_row(tx, &snap.deleted_tag)?;
        for row in &snap.item_tag_rows {
            insert_item_tag_row(tx, row)?;
        }

        Ok(CommandEffect {
            events: vec![
                DomainEvent::TagsChanged {
                    project_id: snap.deleted_tag.project_id,
                },
                DomainEvent::ItemsChanged {
                    project_id: snap.deleted_tag.project_id,
                },
            ],
            ..Default::default()
        })
    }
}
