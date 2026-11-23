pub mod actor;
pub mod migrations;

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::domain::{
    Assignee, AssigneeId, DomainError, FeatureWorkItem, Item, ItemId, ItemKind, ItemStatus,
    Priority, Project, ProjectId, ProjectSnapshot, ProjectStatus, Tag, TagId, WorkspaceMeta,
    WorkspaceOverview,
};

pub use actor::{DbRequest, DbService, DomainEventReceiver};
pub use migrations::{
    APP_MAX_SCHEMA_VERSION, checkpoint_and_switch_to_delete_mode, inspect_and_migrate,
    seed_sample_project,
};

pub(crate) fn uuid_to_sql(id: Uuid) -> String {
    id.to_string()
}

pub(crate) fn ts_to_sql(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339()
}

pub(crate) fn parse_uuid_col(col: usize, raw: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Text,
            Box::new(err),
        )
    })
}

pub(crate) fn parse_ts_col(col: usize, raw: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                col,
                rusqlite::types::Type::Text,
                Box::new(err),
            )
        })
}

fn parse_domain_col<T>(
    col: usize,
    res: Result<T, DomainError>,
) -> rusqlite::Result<T> {
    res.map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Text,
            Box::new(err),
        )
    })
}

pub(crate) fn map_sqlite_err(err: rusqlite::Error) -> DomainError {
    DomainError::Storage(err.to_string())
}

pub fn load_workspace_meta(conn: &Connection) -> Result<WorkspaceMeta, DomainError> {
    conn.query_row(
        "SELECT schema_version, app_min_version, workspace_name, created_at, updated_at
         FROM workspace_meta
         WHERE id = 1;",
        [],
        |row| {
            let schema_version_i64: i64 = row.get(0)?;
            let schema_version = u32::try_from(schema_version_i64).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Integer,
                    Box::new(err),
                )
            })?;
            let app_min_version: String = row.get(1)?;
            let workspace_name: String = row.get(2)?;
            let created_at = parse_ts_col(3, row.get(3)?)?;
            let updated_at = parse_ts_col(4, row.get(4)?)?;
            Ok(WorkspaceMeta {
                schema_version,
                app_min_version,
                workspace_name,
                created_at,
                updated_at,
            })
        },
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => {
            DomainError::InvalidWorkspace("workspace_meta row (id = 1) is missing".to_string())
        }
        other => map_sqlite_err(other),
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
        .query_map([], |row| {
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
        })
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
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
        .query_map([], |row| {
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
        })
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

pub fn load_project_snapshot(
    conn: &Connection,
    project_id: ProjectId,
) -> Result<ProjectSnapshot, DomainError> {
    let projects = list_projects(conn)?;
    let project = projects
        .iter()
        .find(|p| p.id == project_id)
        .cloned()
        .ok_or(DomainError::ProjectNotFound(project_id))?;

    let project_id_sql = uuid_to_sql(project_id.0);
    let assignees = list_assignees(conn)?;

    let tags = {
        let mut stmt = conn
            .prepare(
                "SELECT id, project_id, name, color, created_at
                 FROM tags
                 WHERE project_id = ?1
                 ORDER BY name COLLATE NOCASE ASC;",
            )
            .map_err(map_sqlite_err)?;
        let rows = stmt
            .query_map(params![&project_id_sql], |row| {
                Ok(Tag {
                    id: TagId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                    project_id: ProjectId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                    name: row.get(2)?,
                    color: row.get(3)?,
                    created_at: parse_ts_col(4, row.get(4)?)?,
                })
            })
            .map_err(map_sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)?
    };

    let items = {
        let mut stmt = conn
            .prepare(
                "SELECT id, project_id, item_number, item_key, kind, parent_feature_id,
                        title, description, status, priority, assignee_id, sort_order,
                        deleted_at, trash_root_id, created_at, updated_at
                 FROM items
                 WHERE project_id = ?1 AND deleted_at IS NULL
                 ORDER BY item_number ASC;",
            )
            .map_err(map_sqlite_err)?;
        let rows = stmt
            .query_map(params![&project_id_sql], |row| {
                let kind_raw: String = row.get(4)?;
                let kind = parse_domain_col(4, ItemKind::from_db_str(&kind_raw))?;
                let parent_opt: Option<String> = row.get(5)?;
                let parent_feature_id = parent_opt
                    .map(|raw| parse_uuid_col(5, raw).map(ItemId::from_uuid))
                    .transpose()?;
                let status_raw: String = row.get(8)?;
                let status = parse_domain_col(8, ItemStatus::from_db_str(&status_raw))?;
                let priority_raw: String = row.get(9)?;
                let priority = parse_domain_col(9, Priority::from_db_str(&priority_raw))?;
                let assignee_opt: Option<String> = row.get(10)?;
                let assignee_id = assignee_opt
                    .map(|raw| parse_uuid_col(10, raw).map(AssigneeId::from_uuid))
                    .transpose()?;
                let deleted_opt: Option<String> = row.get(12)?;
                let deleted_at = deleted_opt
                    .map(|raw| parse_ts_col(12, raw))
                    .transpose()?;
                let trash_root_opt: Option<String> = row.get(13)?;
                let trash_root_id = trash_root_opt
                    .map(|raw| parse_uuid_col(13, raw).map(ItemId::from_uuid))
                    .transpose()?;

                Ok(Item {
                    id: ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                    project_id: ProjectId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                    item_number: row.get(2)?,
                    item_key: row.get(3)?,
                    kind,
                    parent_feature_id,
                    title: row.get(6)?,
                    description: row.get(7)?,
                    status,
                    priority,
                    assignee_id,
                    sort_order: row.get(11)?,
                    deleted_at,
                    trash_root_id,
                    created_at: parse_ts_col(14, row.get(14)?)?,
                    updated_at: parse_ts_col(15, row.get(15)?)?,
                })
            })
            .map_err(map_sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)?
    };

    let feature_work_items = {
        let mut stmt = conn
            .prepare(
                "SELECT fwi.project_id, fwi.feature_id, fwi.work_item_id, fwi.sort_order, fwi.created_at
                 FROM feature_work_items fwi
                 JOIN items f ON f.id = fwi.feature_id AND f.deleted_at IS NULL
                 JOIN items wi ON wi.id = fwi.work_item_id AND wi.deleted_at IS NULL
                 WHERE fwi.project_id = ?1
                 ORDER BY fwi.feature_id ASC, fwi.sort_order ASC;",
            )
            .map_err(map_sqlite_err)?;
        let rows = stmt
            .query_map(params![&project_id_sql], |row| {
                Ok(FeatureWorkItem {
                    project_id: ProjectId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                    feature_id: ItemId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                    work_item_id: ItemId::from_uuid(parse_uuid_col(2, row.get(2)?)?),
                    sort_order: row.get(3)?,
                    created_at: parse_ts_col(4, row.get(4)?)?,
                })
            })
            .map_err(map_sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)?
    };

    let mut item_tags: HashMap<ItemId, Vec<TagId>> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT it.item_id, it.tag_id
                 FROM item_tags it
                 JOIN items i ON i.id = it.item_id
                 WHERE it.project_id = ?1 AND i.deleted_at IS NULL
                 ORDER BY it.created_at ASC;",
            )
            .map_err(map_sqlite_err)?;
        let rows = stmt
            .query_map(params![&project_id_sql], |row| {
                let item_id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
                let tag_id = TagId::from_uuid(parse_uuid_col(1, row.get(1)?)?);
                Ok((item_id, tag_id))
            })
            .map_err(map_sqlite_err)?;
        for pair in rows {
            let (item_id, tag_id) = pair.map_err(map_sqlite_err)?;
            item_tags.entry(item_id).or_default().push(tag_id);
        }
    }

    let trash_count_i64: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT trash_root_id) FROM items WHERE project_id = ?1 AND deleted_at IS NOT NULL;",
            params![&project_id_sql],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;
    let trash_count = usize::try_from(trash_count_i64).unwrap_or(0);

    Ok(ProjectSnapshot {
        project,
        projects,
        items,
        assignees,
        tags,
        feature_work_items,
        item_tags,
        trash_count,
    })
}

pub fn load_workspace_overview(
    conn: &Connection,
    preferred_project_id: Option<ProjectId>,
) -> Result<WorkspaceOverview, DomainError> {
    let meta = load_workspace_meta(conn)?;
    let projects = list_projects(conn)?;
    let assignees = list_assignees(conn)?;

    let selected_id = preferred_project_id
        .filter(|pid| projects.iter().any(|p| p.id == *pid))
        .or_else(|| projects.first().map(|p| p.id));

    let active_snapshot = match selected_id {
        Some(project_id) => Some(load_project_snapshot(conn, project_id)?),
        None => None,
    };

    Ok(WorkspaceOverview {
        meta,
        projects,
        assignees,
        active_snapshot,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::migrations::{MigrationStep, apply_migrations_with_steps};
    use super::*;

    #[test]
    fn schema_001_creates_tables_triggers_fts5_and_rejects_cross_project_fks() {
        let mut conn = Connection::open_in_memory().unwrap();
        let meta = inspect_and_migrate(&mut conn, None, "Test Workspace").unwrap();
        assert_eq!(meta.schema_version, 1);
        assert_eq!(meta.workspace_name, "Test Workspace");

        let expected_tables = [
            "workspace_meta",
            "projects",
            "assignees",
            "tags",
            "items",
            "feature_work_items",
            "item_tags",
            "item_blockers",
            "items_fts",
        ];
        for table in expected_tables {
            let exists: i64 = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1);",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "missing schema object '{table}'");
        }

        let now = "2026-11-23T12:00:00Z";
        conn.execute(
            "INSERT INTO projects (id, name, key_prefix, sort_order, created_at, updated_at)
             VALUES ('p-a', 'Alpha', 'AL', 'V', ?1, ?1), ('p-b', 'Beta', 'BE', 'W', ?1, ?1);",
            [now],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO tags (id, project_id, name, color, created_at)
             VALUES ('tag-a', 'p-a', 'core', '#8B5CF6', ?1), ('tag-b', 'p-b', 'ui', '#0EA5E9', ?1);",
            [now],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO items (id, project_id, item_number, item_key, kind, title, sort_order, created_at, updated_at)
             VALUES
                ('fa', 'p-a', 1, 'AL-1', 'feature', 'Feature A', 'V', ?1, ?1),
                ('wa', 'p-a', 2, 'AL-2', 'work_item', 'Work Item A', 'V', ?1, ?1),
                ('fb', 'p-b', 1, 'BE-1', 'feature', 'Feature B', 'V', ?1, ?1),
                ('wb', 'p-b', 2, 'BE-2', 'work_item', 'Work Item B', 'V', ?1, ?1);",
            [now],
        )
        .unwrap();

        // INV-01: Cross-project composite FK rejections
        assert!(
            conn.execute(
                "INSERT INTO items (id, project_id, item_number, item_key, kind, parent_feature_id, title, sort_order, created_at, updated_at)
                 VALUES ('f-cross', 'p-b', 3, 'BE-3', 'feature', 'fa', 'Cross parent', 'W', ?1, ?1);",
                [now],
            )
            .is_err(),
            "cross-project parent_feature_id must be rejected"
        );

        assert!(
            conn.execute(
                "INSERT INTO feature_work_items (project_id, feature_id, work_item_id, sort_order, created_at)
                 VALUES ('p-a', 'fa', 'wb', 'V', ?1);",
                [now],
            )
            .is_err(),
            "cross-project feature_work_items must be rejected"
        );

        assert!(
            conn.execute(
                "INSERT INTO item_tags (project_id, item_id, tag_id, created_at)
                 VALUES ('p-a', 'wa', 'tag-b', ?1);",
                [now],
            )
            .is_err(),
            "cross-project item_tags must be rejected"
        );

        assert!(
            conn.execute(
                "INSERT INTO item_blockers (project_id, blocked_item_id, blocker_feature_id, created_at)
                 VALUES ('p-a', 'fa', 'fb', ?1);",
                [now],
            )
            .is_err(),
            "cross-project item_blockers must be rejected"
        );

        // INV-02: parent_feature_id must reference a feature
        assert!(
            conn.execute(
                "INSERT INTO items (id, project_id, item_number, item_key, kind, parent_feature_id, title, sort_order, created_at, updated_at)
                 VALUES ('f-bad-parent', 'p-a', 3, 'AL-3', 'feature', 'wa', 'Bad parent', 'W', ?1, ?1);",
                [now],
            )
            .is_err(),
            "work_item cannot be parent_feature_id"
        );

        // INV-04: blocker_feature_id must be a feature
        assert!(
            conn.execute(
                "INSERT INTO item_blockers (project_id, blocked_item_id, blocker_feature_id, created_at)
                 VALUES ('p-a', 'fa', 'wa', ?1);",
                [now],
            )
            .is_err(),
            "work_item cannot be blocker_feature_id"
        );

        // INV-08 / INV-09: kind flip guards
        conn.execute(
            "INSERT INTO feature_work_items (project_id, feature_id, work_item_id, sort_order, created_at)
             VALUES ('p-a', 'fa', 'wa', 'V', ?1);",
            [now],
        )
        .unwrap();

        assert!(
            conn.execute("UPDATE items SET kind = 'work_item' WHERE id = 'fa';", [])
                .is_err(),
            "feature with attached work item cannot flip kind"
        );
        assert!(
            conn.execute("UPDATE items SET kind = 'feature' WHERE id = 'wa';", [])
                .is_err(),
            "work_item with feature_work_items link cannot flip kind"
        );

        // FTS5 sync triggers (insert / update / delete)
        let fts_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM items_fts WHERE items_fts MATCH 'Feature';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fts_hits, 2);

        conn.execute("UPDATE items SET title = 'Updated Alpha' WHERE id = 'fa';", [])
            .unwrap();
        let updated_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM items_fts WHERE items_fts MATCH 'Updated';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(updated_hits, 1);

        conn.execute("DELETE FROM items WHERE id = 'fb';", []).unwrap();
        let remaining_feature_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM items_fts WHERE items_fts MATCH 'Feature';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining_feature_hits, 0);
    }

    #[test]
    fn forward_schema_version_guard_never_mutates_file() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("future.protrack");

        {
            let mut conn = Connection::open(&db_path).unwrap();
            inspect_and_migrate(&mut conn, Some(&db_path), "Future").unwrap();
            conn.execute("UPDATE workspace_meta SET schema_version = 999 WHERE id = 1;", [])
                .unwrap();
            checkpoint_and_switch_to_delete_mode(&conn).unwrap();
        }

        let original_bytes = fs::read(&db_path).unwrap();
        let mut conn = Connection::open(&db_path).unwrap();
        let err = inspect_and_migrate(&mut conn, Some(&db_path), "Future").unwrap_err();
        drop(conn);

        assert_eq!(
            err,
            DomainError::ForwardSchemaVersion {
                file_version: 999,
                max_supported: APP_MAX_SCHEMA_VERSION,
            }
        );
        assert_eq!(fs::read(&db_path).unwrap(), original_bytes);

        let wal_path = crate::workspace::wal_path_for(&db_path);
        let shm_path = crate::workspace::shm_path_for(&db_path);
        assert!(!wal_path.exists());
        assert!(!shm_path.exists());
    }

    #[test]
    fn pre_migration_backup_copies_exact_bytes_atomically() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("upgrade.protrack");

        {
            let mut conn = Connection::open(&db_path).unwrap();
            inspect_and_migrate(&mut conn, Some(&db_path), "Upgrade").unwrap();
            checkpoint_and_switch_to_delete_mode(&conn).unwrap();
        }
        let v1_bytes = fs::read(&db_path).unwrap();

        let mut conn = Connection::open(&db_path).unwrap();
        let step_v2 = MigrationStep {
            target_version: 2,
            sql: "ALTER TABLE workspace_meta ADD COLUMN notes TEXT NOT NULL DEFAULT '';",
        };
        let meta = apply_migrations_with_steps(&mut conn, Some(&db_path), "Upgrade", 2, &[step_v2])
            .unwrap();
        assert_eq!(meta.schema_version, 2);
        checkpoint_and_switch_to_delete_mode(&conn).unwrap();
        drop(conn);

        let bak_path = dir.path().join("upgrade.protrack.v1.bak");
        assert!(bak_path.exists());
        assert_eq!(fs::read(&bak_path).unwrap(), v1_bytes);
    }

    #[test]
    fn db_actor_seeds_loads_snapshot_and_shuts_down_to_delete_mode() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("actor.protrack");

        let mut conn = Connection::open(&db_path).unwrap();
        inspect_and_migrate(&mut conn, Some(&db_path), "Actor").unwrap();
        let service = DbService::spawn(conn).unwrap();

        let seeded_id = service.seed_sample_project_blocking().unwrap();
        let overview = service.load_workspace_overview_blocking(None).unwrap();
        let snap = overview.active_snapshot.unwrap();
        assert_eq!(snap.project.id, seeded_id);
        assert_eq!(snap.items.len(), 9);
        assert_eq!(snap.unallocated_work_items().len(), 2);
        assert_eq!(snap.assignees.len(), 2);
        assert_eq!(snap.tags.len(), 2);

        service.shutdown().unwrap();

        let wal_path = crate::workspace::wal_path_for(&db_path);
        let shm_path = crate::workspace::shm_path_for(&db_path);
        assert!(!wal_path.exists());
        assert!(!shm_path.exists());

        // Verify SQLite header journal mode bytes [18, 19] == [1, 1] (legacy rollback journal, not WAL=2)
        let bytes = fs::read(&db_path).unwrap();
        assert_eq!(bytes[18], 1);
        assert_eq!(bytes[19], 1);
    }
}

