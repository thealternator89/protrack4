use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};

use crate::db::{load_workspace_meta, map_sqlite_err, ts_to_sql, uuid_to_sql};
use crate::domain::{DomainError, ItemKind, ProjectId, WorkspaceMeta};
use crate::state::sample_snapshot;

pub const APP_MAX_SCHEMA_VERSION: u32 = 1;
pub const APP_MIN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MIGRATION_001_SQL: &str = include_str!("../../migrations/001_initial.sql");

#[derive(Debug, Clone, Copy)]
pub struct MigrationStep {
    pub target_version: u32,
    pub sql: &'static str,
}

/// Strictly read-only inspection of the database's schema version.
///
/// Returns `0` for an empty brand-new SQLite database with no tables and `user_version == 0`.
pub fn inspect_schema_version(conn: &Connection) -> Result<u32, DomainError> {
    let user_version_i64: i64 = conn
        .query_row("PRAGMA user_version;", [], |row| row.get(0))
        .map_err(map_sqlite_err)?;
    let user_version = u32::try_from(user_version_i64).map_err(|_| {
        DomainError::InvalidWorkspace(format!("invalid negative user_version: {user_version_i64}"))
    })?;

    let table_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%';",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;

    let has_meta: i64 = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'workspace_meta');",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;

    if table_count == 0 && user_version == 0 {
        return Ok(0);
    }

    if has_meta != 0 {
        let meta_version_opt: Option<i64> = conn
            .query_row(
                "SELECT schema_version FROM workspace_meta WHERE id = 1;",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite_err)?;

        return match meta_version_opt {
            Some(meta_version_i64) => {
                let meta_version = u32::try_from(meta_version_i64).map_err(|_| {
                    DomainError::InvalidWorkspace(format!(
                        "invalid workspace_meta.schema_version: {meta_version_i64}"
                    ))
                })?;
                Ok(meta_version.max(user_version))
            }
            None if user_version > APP_MAX_SCHEMA_VERSION => Ok(user_version),
            None => Err(DomainError::InvalidWorkspace(
                "workspace_meta row (id = 1) is missing".to_string(),
            )),
        };
    }

    if user_version > APP_MAX_SCHEMA_VERSION {
        Ok(user_version)
    } else {
        Err(DomainError::InvalidWorkspace(
            "SQLite file is not a valid ProTrack workspace (missing workspace_meta table)"
                .to_string(),
        ))
    }
}

/// Atomically creates `<db_path>.v{from_version}.bak` before applying any upgrade migration.
pub fn create_pre_migration_backup(db_path: &Path, from_version: u32) -> Result<PathBuf, DomainError> {
    let mut bak_os = db_path.as_os_str().to_os_string();
    bak_os.push(format!(".v{from_version}.bak"));
    let bak_path = PathBuf::from(bak_os);

    let mut tmp_os = bak_path.as_os_str().to_os_string();
    tmp_os.push(".tmp");
    let tmp_path = PathBuf::from(tmp_os);

    fs::copy(db_path, &tmp_path).map_err(|err| {
        DomainError::Storage(format!(
            "failed to copy workspace backup to '{}': {err}",
            tmp_path.display()
        ))
    })?;

    {
        let mut tmp_file = File::options().write(true).open(&tmp_path).map_err(|err| {
            DomainError::Storage(format!(
                "failed to open backup temp file '{}' for sync: {err}",
                tmp_path.display()
            ))
        })?;
        tmp_file.flush().map_err(|err| {
            DomainError::Storage(format!("failed to flush backup temp file: {err}"))
        })?;
        tmp_file.sync_all().map_err(|err| {
            DomainError::Storage(format!("failed to fsync backup temp file: {err}"))
        })?;
    }

    fs::rename(&tmp_path, &bak_path).map_err(|err| {
        let _ = fs::remove_file(&tmp_path);
        DomainError::Storage(format!(
            "failed to finalize pre-migration backup '{}': {err}",
            bak_path.display()
        ))
    })?;

    Ok(bak_path)
}

/// Configures active session SQLite pragmas (`WAL`, `foreign_keys = ON`, `NORMAL`, `busy_timeout`).
pub fn configure_session_wal(conn: &Connection) -> Result<(), DomainError> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;\n\
         PRAGMA journal_mode = WAL;\n\
         PRAGMA synchronous = NORMAL;\n\
         PRAGMA busy_timeout = 3000;",
    )
    .map_err(map_sqlite_err)
}

/// Collapses `-wal` and `-shm` into the main `.protrack` file before closing or switching.
pub fn checkpoint_and_switch_to_delete_mode(conn: &Connection) -> Result<(), DomainError> {
    let (busy, log_frames, checkpointed): (i64, i64, i64) = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE);", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(map_sqlite_err)?;

    if busy != 0 {
        tracing::warn!(
            busy,
            log_frames,
            checkpointed,
            "SQLite WAL checkpoint reported busy frames"
        );
    }

    let mode: String = conn
        .query_row("PRAGMA journal_mode = DELETE;", [], |row| row.get(0))
        .map_err(map_sqlite_err)?;

    if !mode.eq_ignore_ascii_case("delete") {
        return Err(DomainError::Storage(format!(
            "failed to reset journal_mode to DELETE (got '{mode}')"
        )));
    }

    Ok(())
}

pub fn inspect_and_migrate(
    conn: &mut Connection,
    db_path: Option<&Path>,
    default_workspace_name: &str,
) -> Result<WorkspaceMeta, DomainError> {
    apply_migrations_with_steps(
        conn,
        db_path,
        default_workspace_name,
        APP_MAX_SCHEMA_VERSION,
        &[],
    )
}

pub fn apply_migrations_with_steps(
    conn: &mut Connection,
    db_path: Option<&Path>,
    default_workspace_name: &str,
    max_supported_version: u32,
    extra_steps: &[MigrationStep],
) -> Result<WorkspaceMeta, DomainError> {
    // Only non-mutating pragmas before schema version inspection.
    // Never switch to WAL before checking the forward-version guard.
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 3000;")
        .map_err(map_sqlite_err)?;

    let file_version = inspect_schema_version(conn)?;
    if file_version > max_supported_version {
        return Err(DomainError::ForwardSchemaVersion {
            file_version,
            max_supported: max_supported_version,
        });
    }

    let workspace_name = if default_workspace_name.trim().is_empty() {
        "Personal Workspace"
    } else {
        default_workspace_name.trim()
    };

    if file_version == 0 {
        let now = Utc::now();
        let now_sql = ts_to_sql(now);
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        tx.execute_batch(MIGRATION_001_SQL).map_err(map_sqlite_err)?;
        tx.execute(
            "INSERT INTO workspace_meta (id, schema_version, app_min_version, workspace_name, created_at, updated_at)
             VALUES (1, 1, ?1, ?2, ?3, ?4);",
            params![APP_MIN_VERSION, workspace_name, now_sql, now_sql],
        )
        .map_err(map_sqlite_err)?;
        tx.execute_batch("PRAGMA user_version = 1;")
            .map_err(map_sqlite_err)?;
        tx.commit().map_err(map_sqlite_err)?;
    }

    let current_version = if file_version == 0 { 1 } else { file_version };
    if current_version < max_supported_version {
        if let Some(path) = db_path {
            create_pre_migration_backup(path, current_version)?;
        }

        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;

        let mut applied_version = current_version;
        for step in extra_steps {
            if step.target_version > applied_version
                && step.target_version <= max_supported_version
            {
                tx.execute_batch(step.sql).map_err(map_sqlite_err)?;
                applied_version = step.target_version;
            }
        }

        if applied_version != max_supported_version {
            return Err(DomainError::Storage(format!(
                "missing migration steps between v{current_version} and v{max_supported_version}"
            )));
        }

        let now_sql = ts_to_sql(Utc::now());
        tx.execute(
            "UPDATE workspace_meta SET schema_version = ?1, updated_at = ?2 WHERE id = 1;",
            params![i64::from(applied_version), now_sql],
        )
        .map_err(map_sqlite_err)?;
        tx.execute_batch(&format!("PRAGMA user_version = {applied_version};"))
            .map_err(map_sqlite_err)?;
        tx.commit().map_err(map_sqlite_err)?;
    }

    configure_session_wal(conn)?;
    load_workspace_meta(conn)
}

/// Inserts the starter ProTrack v1 sample project into a freshly created workspace.
pub fn seed_sample_project(conn: &mut Connection) -> Result<ProjectId, DomainError> {
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(map_sqlite_err)?;

    let snapshot = sample_snapshot();
    let project = &snapshot.project;

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite_err)?;

    tx.execute(
        "INSERT INTO projects (id, name, key_prefix, next_item_seq, description, color, status, sort_order, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10);",
        params![
            uuid_to_sql(project.id.0),
            project.name,
            project.key_prefix,
            project.next_item_seq,
            project.description,
            project.color,
            project.status.as_str(),
            project.sort_order,
            ts_to_sql(project.created_at),
            ts_to_sql(project.updated_at),
        ],
    )
    .map_err(map_sqlite_err)?;

    for assignee in &snapshot.assignees {
        tx.execute(
            "INSERT INTO assignees (id, name, handle, color, role, notes, archived_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9);",
            params![
                uuid_to_sql(assignee.id.0),
                assignee.name,
                assignee.handle,
                assignee.color,
                assignee.role,
                assignee.notes,
                assignee.archived_at.map(ts_to_sql),
                ts_to_sql(assignee.created_at),
                ts_to_sql(assignee.updated_at),
            ],
        )
        .map_err(map_sqlite_err)?;
    }

    for tag in &snapshot.tags {
        tx.execute(
            "INSERT INTO tags (id, project_id, name, color, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5);",
            params![
                uuid_to_sql(tag.id.0),
                uuid_to_sql(tag.project_id.0),
                tag.name,
                tag.color,
                ts_to_sql(tag.created_at),
            ],
        )
        .map_err(map_sqlite_err)?;
    }

    // Insert Features topologically (parents before children) before Work Items.
    let mut inserted_features = std::collections::HashSet::new();
    let features: Vec<_> = snapshot
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Feature)
        .collect();
    while inserted_features.len() < features.len() {
        let mut progressed = false;
        for feature in &features {
            if inserted_features.contains(&feature.id) {
                continue;
            }
            let parent_ready = feature
                .parent_feature_id
                .is_none_or(|pid| inserted_features.contains(&pid));
            if !parent_ready {
                continue;
            }
            insert_item_row(&tx, feature)?;
            inserted_features.insert(feature.id);
            progressed = true;
        }
        if !progressed {
            return Err(DomainError::InvalidWorkspace(
                "sample seed contains invalid feature parent cycle".to_string(),
            ));
        }
    }

    for item in snapshot
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::WorkItem)
    {
        insert_item_row(&tx, item)?;
    }

    for link in &snapshot.feature_work_items {
        tx.execute(
            "INSERT INTO feature_work_items (project_id, feature_id, work_item_id, sort_order, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5);",
            params![
                uuid_to_sql(link.project_id.0),
                uuid_to_sql(link.feature_id.0),
                uuid_to_sql(link.work_item_id.0),
                link.sort_order,
                ts_to_sql(link.created_at),
            ],
        )
        .map_err(map_sqlite_err)?;
    }

    for (item_id, tag_ids) in &snapshot.item_tags {
        for tag_id in tag_ids {
            tx.execute(
                "INSERT INTO item_tags (project_id, item_id, tag_id, created_at)
                 VALUES (?1, ?2, ?3, ?4);",
                params![
                    uuid_to_sql(project.id.0),
                    uuid_to_sql(item_id.0),
                    uuid_to_sql(tag_id.0),
                    ts_to_sql(project.created_at),
                ],
            )
            .map_err(map_sqlite_err)?;
        }
    }

    tx.commit().map_err(map_sqlite_err)?;
    Ok(project.id)
}

fn insert_item_row(tx: &rusqlite::Transaction<'_>, item: &crate::domain::Item) -> Result<(), DomainError> {
    tx.execute(
        "INSERT INTO items (
            id, project_id, item_number, item_key, kind, parent_feature_id,
            title, description, status, priority, assignee_id, sort_order,
            deleted_at, trash_root_id, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16);",
        params![
            uuid_to_sql(item.id.0),
            uuid_to_sql(item.project_id.0),
            item.item_number,
            item.item_key,
            item.kind.as_str(),
            item.parent_feature_id.map(|id| uuid_to_sql(id.0)),
            item.title,
            item.description,
            item.status.as_str(),
            item.priority.as_str(),
            item.assignee_id.map(|id| uuid_to_sql(id.0)),
            item.sort_order,
            item.deleted_at.map(ts_to_sql),
            item.trash_root_id.map(|id| uuid_to_sql(id.0)),
            ts_to_sql(item.created_at),
            ts_to_sql(item.updated_at),
        ],
    )
    .map_err(map_sqlite_err)?;
    Ok(())
}
