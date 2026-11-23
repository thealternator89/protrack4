use chrono::Utc;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use crate::db::{map_sqlite_err, parse_domain_col, parse_ts_col, parse_uuid_col, ts_to_sql, uuid_to_sql};
use crate::domain::fractional_index;
use crate::domain::{DomainError, Project, ProjectId, ProjectStatus};

#[derive(Debug, Clone)]
pub struct CreateProjectInput {
    pub name: String,
    pub key_prefix: String,
    pub description: String,
    pub color: String,
    pub status: ProjectStatus,
}

#[derive(Debug, Clone)]
pub struct UpdateProjectInput {
    pub project_id: ProjectId,
    pub name: String,
    pub key_prefix: String,
    pub description: String,
    pub color: String,
    pub status: ProjectStatus,
}

pub fn validate_project_name(name: &str) -> Result<String, DomainError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(DomainError::Validation(
            "Project name cannot be empty".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

pub fn validate_key_prefix(prefix: &str) -> Result<String, DomainError> {
    let trimmed = prefix.trim();
    let bytes = trimmed.as_bytes();
    if !(2..=10).contains(&bytes.len()) {
        return Err(DomainError::Validation(
            "Key prefix must be 2–10 characters (e.g. PRO, HOME2)".to_string(),
        ));
    }
    if !bytes[0].is_ascii_uppercase()
        || !bytes[1..]
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Err(DomainError::Validation(
            "Key prefix must start with A–Z and contain only A–Z and 0–9".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

pub fn validate_hex_color(color: &str) -> Result<String, DomainError> {
    let trimmed = color.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' || !bytes[1..].iter().all(|b| b.is_ascii_hexdigit()) {
        return Err(DomainError::Validation(format!(
            "Project color '{trimmed}' must be a 6-digit hex color like #6366F1"
        )));
    }
    Ok(format!("#{}", trimmed[1..].to_ascii_uppercase()))
}

pub(crate) fn row_to_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    let id = ProjectId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
    let status_raw: String = row.get(6)?;
    let status = parse_domain_col(6, ProjectStatus::from_db_str(&status_raw))?;
    Ok(Project {
        id,
        name: row.get(1)?,
        key_prefix: row.get(2)?,
        next_item_seq: row.get(3)?,
        description: row.get(4)?,
        color: row.get(5)?,
        status,
        sort_order: row.get(7)?,
        created_at: parse_ts_col(8, row.get(8)?)?,
        updated_at: parse_ts_col(9, row.get(9)?)?,
    })
}

pub fn list_projects(conn: &Connection) -> Result<Vec<Project>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, key_prefix, next_item_seq, description, color, status, sort_order, created_at, updated_at
             FROM projects
             ORDER BY sort_order ASC, created_at ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map([], row_to_project)
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

pub fn get_project(conn: &Connection, project_id: ProjectId) -> Result<Project, DomainError> {
    conn.query_row(
        "SELECT id, name, key_prefix, next_item_seq, description, color, status, sort_order, created_at, updated_at
         FROM projects
         WHERE id = ?1;",
        params![uuid_to_sql(project_id.0)],
        row_to_project,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => DomainError::ProjectNotFound(project_id),
        other => map_sqlite_err(other),
    })
}

/// Counts all items (active and trashed) in `project_id`.
pub fn count_project_items(conn: &Connection, project_id: ProjectId) -> Result<usize, DomainError> {
    let count_i64: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM items WHERE project_id = ?1;",
            params![uuid_to_sql(project_id.0)],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;
    Ok(usize::try_from(count_i64).unwrap_or(0))
}

pub fn create_project(
    tx: &Transaction<'_>,
    input: CreateProjectInput,
) -> Result<Project, DomainError> {
    let name = validate_project_name(&input.name)?;
    let key_prefix = validate_key_prefix(&input.key_prefix)?;
    let color = validate_hex_color(&input.color)?;

    let duplicate_exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE key_prefix = ?1);",
            params![&key_prefix],
            |row| row.get::<_, i64>(0),
        )
        .map_err(map_sqlite_err)?
        != 0;
    if duplicate_exists {
        return Err(DomainError::Validation(format!(
            "Project key prefix '{key_prefix}' is already in use"
        )));
    }

    let existing_orders: Vec<(String, String)> = {
        let mut stmt = tx
            .prepare(
                "SELECT id, sort_order
                 FROM projects
                 ORDER BY sort_order ASC, created_at ASC;",
            )
            .map_err(map_sqlite_err)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map_err(map_sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_err)?
    };

    let now = Utc::now();
    let now_sql = ts_to_sql(now);

    let sort_order = if let Some((_, last_key)) = existing_orders.last() {
        match fractional_index::after(last_key) {
            Ok(next) if !fractional_index::needs_rebalance(&next) => next,
            _ => {
                let rebalanced = fractional_index::rebalance_keys(existing_orders.len() + 1);
                for ((proj_id_sql, _), new_key) in existing_orders.iter().zip(rebalanced.iter()) {
                    tx.execute(
                        "UPDATE projects SET sort_order = ?1, updated_at = ?2 WHERE id = ?3;",
                        params![new_key, &now_sql, proj_id_sql],
                    )
                    .map_err(map_sqlite_err)?;
                }
                rebalanced
                    .last()
                    .cloned()
                    .unwrap_or_else(fractional_index::first)
            }
        }
    } else {
        fractional_index::first()
    };

    let project = Project {
        id: ProjectId::new(),
        name,
        key_prefix,
        next_item_seq: 1,
        description: input.description,
        color,
        status: input.status,
        sort_order,
        created_at: now,
        updated_at: now,
    };

    tx.execute(
        "INSERT INTO projects (
            id, name, key_prefix, next_item_seq, description, color, status, sort_order, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10);",
        params![
            uuid_to_sql(project.id.0),
            &project.name,
            &project.key_prefix,
            project.next_item_seq,
            &project.description,
            &project.color,
            project.status.as_str(),
            &project.sort_order,
            &now_sql,
            &now_sql,
        ],
    )
    .map_err(map_sqlite_err)?;

    Ok(project)
}

/// Updates a Project's metadata and atomically rewrites all `item_key` values if `key_prefix` changed.
pub fn update_project(
    tx: &Transaction<'_>,
    input: UpdateProjectInput,
) -> Result<(Project, bool), DomainError> {
    let existing = get_project(tx, input.project_id)?;
    let name = validate_project_name(&input.name)?;
    let key_prefix = validate_key_prefix(&input.key_prefix)?;
    let color = validate_hex_color(&input.color)?;
    let project_id_sql = uuid_to_sql(input.project_id.0);

    let duplicate_exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE key_prefix = ?1 AND id != ?2);",
            params![&key_prefix, &project_id_sql],
            |row| row.get::<_, i64>(0),
        )
        .map_err(map_sqlite_err)?
        != 0;
    if duplicate_exists {
        return Err(DomainError::Validation(format!(
            "Project key prefix '{key_prefix}' is already in use"
        )));
    }

    let now = Utc::now();
    let now_sql = ts_to_sql(now);
    let prefix_changed = key_prefix != existing.key_prefix;

    tx.execute(
        "UPDATE projects
         SET name = ?1, key_prefix = ?2, description = ?3, color = ?4, status = ?5, updated_at = ?6
         WHERE id = ?7;",
        params![
            &name,
            &key_prefix,
            &input.description,
            &color,
            input.status.as_str(),
            &now_sql,
            &project_id_sql,
        ],
    )
    .map_err(map_sqlite_err)?;

    if prefix_changed {
        tx.execute(
            "UPDATE items
             SET item_key = ?1 || '-' || item_number,
                 updated_at = ?2
             WHERE project_id = ?3;",
            params![&key_prefix, &now_sql, &project_id_sql],
        )
        .map_err(map_sqlite_err)?;
    }

    let updated = Project {
        id: existing.id,
        name,
        key_prefix,
        next_item_seq: existing.next_item_seq,
        description: input.description,
        color,
        status: input.status,
        sort_order: existing.sort_order,
        created_at: existing.created_at,
        updated_at: now,
    };

    Ok((updated, prefix_changed))
}

/// Atomically consumes `projects.next_item_seq` and formats `{key_prefix}-{item_number}` (`INV-13`).
pub fn allocate_next_item_key(
    tx: &Transaction<'_>,
    project_id: ProjectId,
) -> Result<(i64, String), DomainError> {
    let project_id_sql = uuid_to_sql(project_id.0);
    let row: Option<(String, i64)> = tx
        .query_row(
            "SELECT key_prefix, next_item_seq FROM projects WHERE id = ?1;",
            params![&project_id_sql],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(map_sqlite_err)?;

    let Some((key_prefix, item_number)) = row else {
        return Err(DomainError::ProjectNotFound(project_id));
    };

    let now_sql = ts_to_sql(Utc::now());
    tx.execute(
        "UPDATE projects
         SET next_item_seq = next_item_seq + 1,
             updated_at = ?1
         WHERE id = ?2;",
        params![&now_sql, &project_id_sql],
    )
    .map_err(map_sqlite_err)?;

    Ok((item_number, format!("{key_prefix}-{item_number}")))
}
