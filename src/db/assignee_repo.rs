use chrono::Utc;
use rusqlite::{Connection, Transaction, params};

use crate::db::project_repo::validate_hex_color;
use crate::db::{map_sqlite_err, parse_ts_col, parse_uuid_col, ts_to_sql, uuid_to_sql};
use crate::domain::{Assignee, AssigneeId, DomainError};

#[derive(Debug, Clone)]
pub struct UpsertAssigneeInput {
    pub id: Option<AssigneeId>,
    pub name: String,
    pub handle: String,
    pub color: String,
    pub role: Option<String>,
    pub notes: String,
}

pub fn validate_assignee_name(name: &str) -> Result<String, DomainError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(DomainError::Validation(
            "Assignee name cannot be empty".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

pub fn validate_assignee_handle(handle: &str) -> Result<String, DomainError> {
    let trimmed = handle.trim();
    let count = trimmed.chars().count();
    if !(1..=8).contains(&count) {
        return Err(DomainError::Validation(
            "Handle must be 1–8 characters (e.g., MB, AI)".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

fn normalize_role(role: Option<&str>) -> Option<String> {
    role.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub(crate) fn row_to_assignee(row: &rusqlite::Row<'_>) -> rusqlite::Result<Assignee> {
    let archived_opt: Option<String> = row.get(6)?;
    let archived_at = archived_opt
        .map(|raw| parse_ts_col(6, raw))
        .transpose()?;
    Ok(Assignee {
        id: AssigneeId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
        name: row.get(1)?,
        handle: row.get(2)?,
        color: row.get(3)?,
        role: row.get(4)?,
        notes: row.get(5)?,
        archived_at,
        created_at: parse_ts_col(7, row.get(7)?)?,
        updated_at: parse_ts_col(8, row.get(8)?)?,
    })
}

pub fn list_assignees(conn: &Connection) -> Result<Vec<Assignee>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, handle, color, role, notes, archived_at, created_at, updated_at
             FROM assignees
             ORDER BY archived_at IS NOT NULL ASC, name COLLATE NOCASE ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map([], row_to_assignee)
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

pub fn list_active_assignees(conn: &Connection) -> Result<Vec<Assignee>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, handle, color, role, notes, archived_at, created_at, updated_at
             FROM assignees
             WHERE archived_at IS NULL
             ORDER BY name COLLATE NOCASE ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map([], row_to_assignee)
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

pub fn get_assignee(conn: &Connection, assignee_id: AssigneeId) -> Result<Assignee, DomainError> {
    conn.query_row(
        "SELECT id, name, handle, color, role, notes, archived_at, created_at, updated_at
         FROM assignees
         WHERE id = ?1;",
        params![uuid_to_sql(assignee_id.0)],
        row_to_assignee,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => DomainError::AssigneeNotFound(assignee_id),
        other => map_sqlite_err(other),
    })
}

pub fn upsert_assignee(
    tx: &Transaction<'_>,
    input: UpsertAssigneeInput,
) -> Result<(Assignee, bool), DomainError> {
    let name = validate_assignee_name(&input.name)?;
    let handle = validate_assignee_handle(&input.handle)?;
    let color = validate_hex_color(&input.color)?;
    let role = normalize_role(input.role.as_deref());
    let now = Utc::now();
    let now_sql = ts_to_sql(now);

    match input.id {
        Some(aid) => {
            let existing = get_assignee(tx, aid)?;
            let id_sql = uuid_to_sql(aid.0);
            let duplicate_exists: bool = tx
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM assignees
                        WHERE handle = ?1 COLLATE NOCASE AND id != ?2
                     );",
                    params![&handle, &id_sql],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(map_sqlite_err)?
                != 0;
            if duplicate_exists {
                return Err(DomainError::Validation(format!(
                    "Assignee handle '{handle}' is already in use"
                )));
            }

            tx.execute(
                "UPDATE assignees
                 SET name = ?1, handle = ?2, color = ?3, role = ?4, notes = ?5, updated_at = ?6
                 WHERE id = ?7;",
                params![&name, &handle, &color, &role, &input.notes, &now_sql, &id_sql],
            )
            .map_err(map_sqlite_err)?;

            Ok((
                Assignee {
                    id: existing.id,
                    name,
                    handle,
                    color,
                    role,
                    notes: input.notes,
                    archived_at: existing.archived_at,
                    created_at: existing.created_at,
                    updated_at: now,
                },
                false,
            ))
        }
        None => {
            let duplicate_exists: bool = tx
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM assignees WHERE handle = ?1 COLLATE NOCASE
                     );",
                    params![&handle],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(map_sqlite_err)?
                != 0;
            if duplicate_exists {
                return Err(DomainError::Validation(format!(
                    "Assignee handle '{handle}' is already in use"
                )));
            }

            let assignee = Assignee {
                id: AssigneeId::new(),
                name,
                handle,
                color,
                role,
                notes: input.notes,
                archived_at: None,
                created_at: now,
                updated_at: now,
            };
            let id_sql = uuid_to_sql(assignee.id.0);

            tx.execute(
                "INSERT INTO assignees (
                    id, name, handle, color, role, notes, archived_at, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8);",
                params![
                    &id_sql,
                    &assignee.name,
                    &assignee.handle,
                    &assignee.color,
                    &assignee.role,
                    &assignee.notes,
                    &now_sql,
                    &now_sql,
                ],
            )
            .map_err(map_sqlite_err)?;

            Ok((assignee, true))
        }
    }
}

pub fn set_assignee_archived(
    tx: &Transaction<'_>,
    assignee_id: AssigneeId,
    archived: bool,
) -> Result<Assignee, DomainError> {
    let existing = get_assignee(tx, assignee_id)?;
    let now = Utc::now();
    let now_sql = ts_to_sql(now);
    let archived_at = if archived {
        existing.archived_at.or(Some(now))
    } else {
        None
    };

    tx.execute(
        "UPDATE assignees SET archived_at = ?1, updated_at = ?2 WHERE id = ?3;",
        params![
            archived_at.map(ts_to_sql),
            &now_sql,
            uuid_to_sql(assignee_id.0)
        ],
    )
    .map_err(map_sqlite_err)?;

    Ok(Assignee {
        archived_at,
        updated_at: now,
        ..existing
    })
}
