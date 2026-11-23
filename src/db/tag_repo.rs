use std::collections::HashSet;

use chrono::Utc;
use rusqlite::{Connection, Transaction, params};

use crate::db::item_repo::get_item;
use crate::db::project_repo::{get_project, validate_hex_color};
use crate::db::{map_sqlite_err, parse_ts_col, parse_uuid_col, ts_to_sql, uuid_to_sql};
use crate::domain::{DomainError, Item, ItemId, ProjectId, Tag, TagId};

#[derive(Debug, Clone)]
pub struct UpsertTagInput {
    pub id: Option<TagId>,
    pub project_id: ProjectId,
    pub name: String,
    pub color: String,
}

pub fn validate_tag_name(name: &str) -> Result<String, DomainError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(DomainError::Validation(
            "Tag name cannot be empty".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn row_to_tag(row: &rusqlite::Row<'_>) -> rusqlite::Result<Tag> {
    Ok(Tag {
        id: TagId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
        project_id: ProjectId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
        name: row.get(2)?,
        color: row.get(3)?,
        created_at: parse_ts_col(4, row.get(4)?)?,
    })
}

pub fn list_tags_for_project(
    conn: &Connection,
    project_id: ProjectId,
) -> Result<Vec<Tag>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, project_id, name, color, created_at
             FROM tags
             WHERE project_id = ?1
             ORDER BY name COLLATE NOCASE ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(params![uuid_to_sql(project_id.0)], row_to_tag)
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

pub fn get_tag(conn: &Connection, tag_id: TagId) -> Result<Tag, DomainError> {
    conn.query_row(
        "SELECT id, project_id, name, color, created_at
         FROM tags
         WHERE id = ?1;",
        params![uuid_to_sql(tag_id.0)],
        row_to_tag,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => DomainError::TagNotFound(tag_id),
        other => map_sqlite_err(other),
    })
}

pub fn list_tag_ids_for_item(
    conn: &Connection,
    item_id: ItemId,
) -> Result<Vec<TagId>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT tag_id
             FROM item_tags
             WHERE item_id = ?1
             ORDER BY created_at ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(params![uuid_to_sql(item_id.0)], |row| {
            Ok(TagId::from_uuid(parse_uuid_col(0, row.get(0)?)?))
        })
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

pub fn upsert_tag(
    tx: &Transaction<'_>,
    input: UpsertTagInput,
) -> Result<(Tag, bool), DomainError> {
    let name = validate_tag_name(&input.name)?;
    let color = validate_hex_color(&input.color)?;
    let _project = get_project(tx, input.project_id)?;
    let project_sql = uuid_to_sql(input.project_id.0);

    match input.id {
        Some(tid) => {
            let existing = get_tag(tx, tid)?;
            if existing.project_id != input.project_id {
                return Err(DomainError::CrossProjectReference);
            }
            let id_sql = uuid_to_sql(tid.0);
            let duplicate_exists: bool = tx
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM tags
                        WHERE project_id = ?1 AND name = ?2 COLLATE NOCASE AND id != ?3
                     );",
                    params![&project_sql, &name, &id_sql],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(map_sqlite_err)?
                != 0;
            if duplicate_exists {
                return Err(DomainError::Validation(format!(
                    "Tag '{name}' already exists in this project"
                )));
            }

            tx.execute(
                "UPDATE tags SET name = ?1, color = ?2 WHERE id = ?3;",
                params![&name, &color, &id_sql],
            )
            .map_err(map_sqlite_err)?;

            Ok((
                Tag {
                    id: existing.id,
                    project_id: existing.project_id,
                    name,
                    color,
                    created_at: existing.created_at,
                },
                false,
            ))
        }
        None => {
            let duplicate_exists: bool = tx
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM tags
                        WHERE project_id = ?1 AND name = ?2 COLLATE NOCASE
                     );",
                    params![&project_sql, &name],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(map_sqlite_err)?
                != 0;
            if duplicate_exists {
                return Err(DomainError::Validation(format!(
                    "Tag '{name}' already exists in this project"
                )));
            }

            let now = Utc::now();
            let now_sql = ts_to_sql(now);
            let tag = Tag {
                id: TagId::new(),
                project_id: input.project_id,
                name,
                color,
                created_at: now,
            };

            tx.execute(
                "INSERT INTO tags (id, project_id, name, color, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5);",
                params![
                    uuid_to_sql(tag.id.0),
                    &project_sql,
                    &tag.name,
                    &tag.color,
                    &now_sql,
                ],
            )
            .map_err(map_sqlite_err)?;

            Ok((tag, true))
        }
    }
}

pub fn delete_tag(tx: &Transaction<'_>, tag_id: TagId) -> Result<Tag, DomainError> {
    let tag = get_tag(tx, tag_id)?;
    tx.execute(
        "DELETE FROM tags WHERE id = ?1;",
        params![uuid_to_sql(tag_id.0)],
    )
    .map_err(map_sqlite_err)?;
    Ok(tag)
}

pub fn set_item_tags(
    tx: &Transaction<'_>,
    item_id: ItemId,
    tag_ids: &[TagId],
) -> Result<Item, DomainError> {
    let item = get_item(tx, item_id)?;
    if !item.is_active() {
        return Err(DomainError::Validation(
            "Cannot modify tags of an item in Trash".to_string(),
        ));
    }

    let mut seen = HashSet::new();
    let mut desired_order = Vec::with_capacity(tag_ids.len());
    for &tid in tag_ids {
        if seen.insert(tid) {
            let tag = get_tag(tx, tid)?;
            if tag.project_id != item.project_id {
                return Err(DomainError::CrossProjectReference);
            }
            desired_order.push(tid);
        }
    }

    let current = list_tag_ids_for_item(tx, item_id)?;
    let current_set: HashSet<TagId> = current.iter().copied().collect();
    let desired_set: HashSet<TagId> = desired_order.iter().copied().collect();

    let item_sql = uuid_to_sql(item_id.0);
    let project_sql = uuid_to_sql(item.project_id.0);
    let mut mutated = false;

    for existing_tid in &current {
        if !desired_set.contains(existing_tid) {
            let deleted = tx
                .execute(
                    "DELETE FROM item_tags WHERE item_id = ?1 AND tag_id = ?2;",
                    params![&item_sql, uuid_to_sql(existing_tid.0)],
                )
                .map_err(map_sqlite_err)?;
            if deleted > 0 {
                mutated = true;
            }
        }
    }

    let now = Utc::now();
    let now_sql = ts_to_sql(now);

    for desired_tid in desired_order {
        if !current_set.contains(&desired_tid) {
            tx.execute(
                "INSERT INTO item_tags (project_id, item_id, tag_id, created_at)
                 VALUES (?1, ?2, ?3, ?4);",
                params![&project_sql, &item_sql, uuid_to_sql(desired_tid.0), &now_sql],
            )
            .map_err(map_sqlite_err)?;
            mutated = true;
        }
    }

    if !mutated {
        return Ok(item);
    }

    tx.execute(
        "UPDATE items SET updated_at = ?1 WHERE id = ?2;",
        params![&now_sql, &item_sql],
    )
    .map_err(map_sqlite_err)?;

    Ok(Item {
        updated_at: now,
        ..item
    })
}
