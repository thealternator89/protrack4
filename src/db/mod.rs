pub mod actor;
pub mod assignee_repo;
pub mod blocker_repo;
pub mod item_repo;
pub mod migrations;
pub mod project_repo;
pub mod tag_repo;

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::domain::{
    DomainError, FeatureWorkItem, ItemId, ProjectId, ProjectSnapshot, TagId, WorkspaceMeta,
    WorkspaceOverview,
};

pub use actor::{DbRequest, DbService, DomainEventReceiver};
pub use assignee_repo::list_assignees;
pub use item_repo::SearchResultItem;
pub use migrations::{
    APP_MAX_SCHEMA_VERSION, checkpoint_and_switch_to_delete_mode, inspect_and_migrate,
    seed_sample_project,
};
pub use project_repo::list_projects;

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

pub(crate) fn parse_domain_col<T>(
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
    let tags = tag_repo::list_tags_for_project(conn, project_id)?;

    let items = {
        let mut stmt = conn
            .prepare(
                "SELECT id, project_id, item_number, item_key, kind, parent_feature_id,
                        title, description, status, priority, assignee_id, sort_order,
                        deleted_at, trash_root_id, created_at, updated_at, link_url, link_label
                 FROM items
                 WHERE project_id = ?1 AND deleted_at IS NULL
                 ORDER BY item_number ASC;",
            )
            .map_err(map_sqlite_err)?;
        let rows = stmt
            .query_map(params![&project_id_sql], item_repo::row_to_item)
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

    let progress_by_feature = item_repo::query_feature_progress_rollup(conn, project_id)?;
    let block_snapshot = blocker_repo::load_project_block_snapshot(conn, project_id)?;
    let trash_groups = item_repo::list_trash_groups(conn, project_id)?;
    let trash_count = trash_groups.len();

    Ok(ProjectSnapshot {
        project,
        projects,
        items,
        assignees,
        tags,
        feature_work_items,
        item_tags,
        progress_by_feature,
        item_blockers: block_snapshot.item_blockers,
        direct_blockers_by_item: block_snapshot.direct_blockers_by_item,
        inherited_blockers_by_item: block_snapshot.inherited_blockers_by_item,
        effective_block_state: block_snapshot.effective_block_state,
        blocker_warnings: block_snapshot.blocker_warnings,
        trash_count,
        trash_groups,
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
        assert_eq!(meta.schema_version, APP_MAX_SCHEMA_VERSION);
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
        let v2_bytes = fs::read(&db_path).unwrap();

        let mut conn = Connection::open(&db_path).unwrap();
        let step_v3 = MigrationStep {
            target_version: 3,
            sql: "ALTER TABLE workspace_meta ADD COLUMN notes TEXT NOT NULL DEFAULT '';",
        };
        let meta = apply_migrations_with_steps(&mut conn, Some(&db_path), "Upgrade", 3, &[step_v3])
            .unwrap();
        assert_eq!(meta.schema_version, 3);
        checkpoint_and_switch_to_delete_mode(&conn).unwrap();
        drop(conn);

        let bak_path = dir.path().join("upgrade.protrack.v2.bak");
        assert!(bak_path.exists());
        assert_eq!(fs::read(&bak_path).unwrap(), v2_bytes);
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

    #[test]
    fn project_crud_validation_and_atomic_key_prefix_rewrite() {
        use crate::domain::ProjectStatus;
        use crate::db::item_repo::{CreateFeatureInput, create_feature};
        use crate::db::project_repo::{
            CreateProjectInput, UpdateProjectInput, count_project_items, create_project,
            get_project, update_project,
        };
        use rusqlite::TransactionBehavior;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "ProjTest").unwrap();

        let p1 = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let p = create_project(
                &tx,
                CreateProjectInput {
                    name: "  Alpha Project  ".to_string(),
                    key_prefix: "OLD".to_string(),
                    description: "Initial".to_string(),
                    color: "#6366f1".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            p
        };
        assert_eq!(p1.name, "Alpha Project");
        assert_eq!(p1.key_prefix, "OLD");
        assert_eq!(p1.color, "#6366F1");
        assert_eq!(p1.next_item_seq, 1);

        // Invalid prefix / duplicate prefix / invalid color rejected
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert!(
                create_project(
                    &tx,
                    CreateProjectInput {
                        name: "Bad".to_string(),
                        key_prefix: "A".to_string(),
                        description: String::new(),
                        color: "#6366F1".to_string(),
                        status: ProjectStatus::Active,
                    },
                )
                .is_err()
            );
            assert!(
                create_project(
                    &tx,
                    CreateProjectInput {
                        name: "Bad2".to_string(),
                        key_prefix: "1ABC".to_string(),
                        description: String::new(),
                        color: "#6366F1".to_string(),
                        status: ProjectStatus::Active,
                    },
                )
                .is_err()
            );
            assert!(
                create_project(
                    &tx,
                    CreateProjectInput {
                        name: "Dup".to_string(),
                        key_prefix: "OLD".to_string(),
                        description: String::new(),
                        color: "#6366F1".to_string(),
                        status: ProjectStatus::Active,
                    },
                )
                .is_err()
            );
            assert!(
                create_project(
                    &tx,
                    CreateProjectInput {
                        name: "BadColor".to_string(),
                        key_prefix: "BAD".to_string(),
                        description: String::new(),
                        color: "red".to_string(),
                        status: ProjectStatus::Active,
                    },
                )
                .is_err()
            );
        }

        // Create two Features (OLD-1, OLD-2) and mark one trashed to verify rewrite hits all items.
        let (f1, f2) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let f1 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: p1.id,
                    parent_feature_id: None,
                    title: "Root One".to_string(),
                    description: String::new(),
                    status: crate::domain::ItemStatus::Todo,
                    priority: crate::domain::Priority::None,
                },
            )
            .unwrap();
            let f2 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: p1.id,
                    parent_feature_id: Some(f1.id),
                    title: "Child Two".to_string(),
                    description: String::new(),
                    status: crate::domain::ItemStatus::InProgress,
                    priority: crate::domain::Priority::High,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            (f1, f2)
        };
        assert_eq!(f1.item_key, "OLD-1");
        assert_eq!(f2.item_key, "OLD-2");
        assert_eq!(count_project_items(&conn, p1.id).unwrap(), 2);
        assert_eq!(get_project(&conn, p1.id).unwrap().next_item_seq, 3);

        // Soft-trash f2 directly to confirm prefix rewrite covers trashed items too
        conn.execute(
            "UPDATE items SET deleted_at = '2026-11-23T12:00:00Z', trash_root_id = id WHERE id = ?1;",
            [uuid_to_sql(f2.id.0)],
        )
        .unwrap();

        // Atomic key_prefix rename OLD -> NEW
        let (updated_proj, changed) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let res = update_project(
                &tx,
                UpdateProjectInput {
                    project_id: p1.id,
                    name: "Alpha Renamed".to_string(),
                    key_prefix: "NEW".to_string(),
                    description: "Updated".to_string(),
                    color: "#10B981".to_string(),
                    status: ProjectStatus::Paused,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            res
        };
        assert!(changed);
        assert_eq!(updated_proj.key_prefix, "NEW");
        assert_eq!(updated_proj.status, ProjectStatus::Paused);
        assert_eq!(item_repo::get_item(&conn, f1.id).unwrap().item_key, "NEW-1");
        assert_eq!(item_repo::get_item(&conn, f2.id).unwrap().item_key, "NEW-2");

        // FTS5 index synced cleanly on item_key rewrite
        let new_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM items_fts WHERE items_fts MATCH 'NEW';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let old_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM items_fts WHERE items_fts MATCH 'OLD';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(new_hits, 2);
        assert_eq!(old_hits, 0);
    }

    #[test]
    fn feature_depth_32_enforced_tree_cte_reparent_and_sibling_reorder_rebalance() {
        use crate::db::item_repo::{
            CreateFeatureInput, create_feature, feature_depth, query_feature_tree,
            reparent_feature, reorder_sibling_features,
        };
        use crate::db::project_repo::{CreateProjectInput, create_project};
        use crate::domain::{ItemStatus, MAX_FEATURE_DEPTH, Priority, ProjectStatus, fractional_index};
        use rusqlite::TransactionBehavior;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "FeatTest").unwrap();

        let (proj_a, proj_b) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let a = create_project(
                &tx,
                CreateProjectInput {
                    name: "Alpha".to_string(),
                    key_prefix: "ALP".to_string(),
                    description: String::new(),
                    color: "#6366F1".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            let b = create_project(
                &tx,
                CreateProjectInput {
                    name: "Beta".to_string(),
                    key_prefix: "BET".to_string(),
                    description: String::new(),
                    color: "#10B981".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            (a, b)
        };

        // Chain of 32 Features (depths 0..=31) succeeds; depth 32 is rejected (INV-02).
        let mut chain = Vec::new();
        let mut parent = None;
        for depth in 0..MAX_FEATURE_DEPTH {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let feat = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: proj_a.id,
                    parent_feature_id: parent,
                    title: format!("Depth {depth}"),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            assert_eq!(feature_depth(&conn, feat.id).unwrap(), depth);
            parent = Some(feat.id);
            chain.push(feat);
        }

        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let err = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: proj_a.id,
                    parent_feature_id: parent,
                    title: "Too Deep".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap_err();
            assert_eq!(
                err,
                DomainError::MaxFeatureDepthExceeded {
                    max_depth: MAX_FEATURE_DEPTH
                }
            );
        }

        let tree = query_feature_tree(&conn, proj_a.id).unwrap();
        assert_eq!(tree.len(), MAX_FEATURE_DEPTH);
        assert_eq!(tree.last().unwrap().depth, 31);

        // Reparent guards: self, descendant, depth overflow, cross-project
        let root = chain[0].id;
        let depth_29 = chain[29].id;
        let depth_31 = chain[31].id;
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert!(reparent_feature(&tx, root, Some(root)).is_err());
            assert!(matches!(
                reparent_feature(&tx, root, Some(depth_31)).unwrap_err(),
                DomainError::DependencyCycle(_)
            ));

            // Create a 3-node branch in proj_a (subtree height = 2) and test depth cap at depth_29 (29+1+2 = 32 -> reject)
            let b0 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: proj_a.id,
                    parent_feature_id: None,
                    title: "Branch 0".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let b1 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: proj_a.id,
                    parent_feature_id: Some(b0.id),
                    title: "Branch 1".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let _b2 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: proj_a.id,
                    parent_feature_id: Some(b1.id),
                    title: "Branch 2".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();

            assert_eq!(
                reparent_feature(&tx, b0.id, Some(depth_29)).unwrap_err(),
                DomainError::MaxFeatureDepthExceeded {
                    max_depth: MAX_FEATURE_DEPTH
                }
            );
        }

        // Cross-project reparent check + sibling reorder rebalance
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let fb = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: proj_b.id,
                    parent_feature_id: None,
                    title: "Beta Root 1".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            assert_eq!(
                reparent_feature(&tx, chain[0].id, Some(fb.id)).unwrap_err(),
                DomainError::CrossProjectReference
            );

            let fb2 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: proj_b.id,
                    parent_feature_id: None,
                    title: "Beta Root 2".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let fb3 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: proj_b.id,
                    parent_feature_id: None,
                    title: "Beta Root 3".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();

            // Move fb3 between fb and fb2
            reorder_sibling_features(&tx, proj_b.id, fb3.id, Some(fb.id), Some(fb2.id)).unwrap();
            // Force a >32-byte key on fb and fb3 so next reorder triggers automatic rebalance
            tx.execute(
                "UPDATE items SET sort_order = ?1 WHERE id = ?2;",
                params!["V".repeat(32), uuid_to_sql(fb.id.0)],
            )
            .unwrap();
            tx.execute(
                "UPDATE items SET sort_order = ?1 WHERE id = ?2;",
                params![format!("{}W", "V".repeat(31)), uuid_to_sql(fb3.id.0)],
            )
            .unwrap();
            reorder_sibling_features(&tx, proj_b.id, fb2.id, Some(fb.id), Some(fb3.id)).unwrap();
            tx.commit().unwrap();

            let beta_tree = query_feature_tree(&conn, proj_b.id).unwrap();
            let titles: Vec<_> = beta_tree.iter().map(|n| n.title.as_str()).collect();
            assert_eq!(titles, vec!["Beta Root 1", "Beta Root 2", "Beta Root 3"]);
            for node in &beta_tree {
                assert!(!fractional_index::needs_rebalance(&node.sort_order));
            }
        }
    }

    #[test]
    fn work_item_multi_feature_membership_unallocated_and_sort_order() {
        use crate::db::item_repo::{
            CreateFeatureInput, CreateWorkItemInput, attach_work_item_to_feature, create_feature,
            create_work_item, detach_work_item_from_feature,
            query_feature_attachments_for_work_item, query_features_for_work_item,
            query_unallocated_work_items, query_work_items_for_feature, set_work_item_features,
        };
        use crate::db::project_repo::{CreateProjectInput, create_project};
        use crate::domain::{ItemStatus, Priority, ProjectStatus};
        use rusqlite::TransactionBehavior;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "WiTest").unwrap();

        let (proj_a, _proj_b, f1, f2, f3, fb) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let pa = create_project(
                &tx,
                CreateProjectInput {
                    name: "Alpha".to_string(),
                    key_prefix: "ALP".to_string(),
                    description: String::new(),
                    color: "#6366F1".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            let pb = create_project(
                &tx,
                CreateProjectInput {
                    name: "Beta".to_string(),
                    key_prefix: "BET".to_string(),
                    description: String::new(),
                    color: "#10B981".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            let f1 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pa.id,
                    parent_feature_id: None,
                    title: "Auth".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::High,
                },
            )
            .unwrap();
            let f2 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pa.id,
                    parent_feature_id: None,
                    title: "Sync".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::Medium,
                },
            )
            .unwrap();
            let f3 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pa.id,
                    parent_feature_id: Some(f1.id),
                    title: "OAuth".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::Low,
                },
            )
            .unwrap();
            let fb = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pb.id,
                    parent_feature_id: None,
                    title: "Beta Feature".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            (pa, pb, f1, f2, f3, fb)
        };

        // Pre-populate f1 with w_other so w1 sits at position #2 in f1 and #1 in f2
        let (w_other, w1) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let w_other = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: proj_a.id,
                    title: "Existing f1 task".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![f1.id],
                },
            )
            .unwrap();
            let w1 = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: proj_a.id,
                    title: "Shared OAuth refresh".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::High,
                    feature_ids: vec![],
                },
            )
            .unwrap();
            tx.commit().unwrap();
            (w_other, w1)
        };

        // Newly created unallocated Work Item appears in Unallocated query (INV-03)
        let unalloc = query_unallocated_work_items(&conn, proj_a.id).unwrap();
        assert_eq!(unalloc.iter().map(|i| i.id).collect::<Vec<_>>(), vec![w1.id]);
        assert!(query_features_for_work_item(&conn, w1.id).unwrap().is_empty());

        // Attach w1 to 2 Features (f1, f2) -> leaves Unallocated & preserves independent sort_order per Feature
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            set_work_item_features(&tx, w1.id, &[f1.id, f2.id]).unwrap();
            tx.commit().unwrap();
        }
        assert!(query_unallocated_work_items(&conn, proj_a.id).unwrap().is_empty());
        assert_eq!(
            query_work_items_for_feature(&conn, f1.id)
                .unwrap()
                .into_iter()
                .map(|i| i.id)
                .collect::<Vec<_>>(),
            vec![w_other.id, w1.id]
        );
        assert_eq!(
            query_work_items_for_feature(&conn, f2.id)
                .unwrap()
                .into_iter()
                .map(|i| i.id)
                .collect::<Vec<_>>(),
            vec![w1.id]
        );

        let initial_links = query_feature_attachments_for_work_item(&conn, w1.id).unwrap();
        let f1_sort = initial_links
            .iter()
            .find(|l| l.feature_id == f1.id)
            .unwrap()
            .sort_order
            .clone();
        let f2_sort = initial_links
            .iter()
            .find(|l| l.feature_id == f2.id)
            .unwrap()
            .sort_order
            .clone();
        assert_ne!(f1_sort, f2_sort);

        // Diffing via set_work_item_features preserves sort_order on retained links
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            set_work_item_features(&tx, w1.id, &[f1.id, f2.id, f3.id]).unwrap();
            tx.commit().unwrap();
        }
        let updated_links = query_feature_attachments_for_work_item(&conn, w1.id).unwrap();
        assert_eq!(updated_links.len(), 3);
        assert_eq!(
            updated_links
                .iter()
                .find(|l| l.feature_id == f1.id)
                .unwrap()
                .sort_order,
            f1_sort
        );
        assert_eq!(
            updated_links
                .iter()
                .find(|l| l.feature_id == f2.id)
                .unwrap()
                .sort_order,
            f2_sort
        );

        // Detaching all Features returns w1 automatically to Unallocated
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert!(detach_work_item_from_feature(&tx, w1.id, f1.id).unwrap());
            assert!(!detach_work_item_from_feature(&tx, w1.id, f1.id).unwrap());
            set_work_item_features(&tx, w1.id, &[]).unwrap();
            tx.commit().unwrap();
        }
        let unalloc_after = query_unallocated_work_items(&conn, proj_a.id).unwrap();
        assert_eq!(
            unalloc_after.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![w1.id]
        );

        // Cross-project & kind guards
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert_eq!(
                attach_work_item_to_feature(&tx, w1.id, fb.id).unwrap_err(),
                DomainError::CrossProjectReference
            );
            assert_eq!(
                create_work_item(
                    &tx,
                    CreateWorkItemInput {
                        project_id: proj_a.id,
                        title: "Cross".to_string(),
                        description: String::new(),
                        status: ItemStatus::Todo,
                        priority: Priority::None,
                        feature_ids: vec![fb.id],
                    },
                )
                .unwrap_err(),
                DomainError::CrossProjectReference
            );
            assert!(attach_work_item_to_feature(&tx, w1.id, w_other.id).is_err());
            assert!(attach_work_item_to_feature(&tx, f2.id, f1.id).is_err());
        }

        // Attach to f1, then mark f1 trashed -> both w_other and w1 now have 0 active attached features -> Unallocated (INV-03)
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            attach_work_item_to_feature(&tx, w1.id, f1.id).unwrap();
            tx.commit().unwrap();
        }
        assert!(query_unallocated_work_items(&conn, proj_a.id).unwrap().is_empty());

        conn.execute(
            "UPDATE items SET deleted_at = '2026-11-23T12:00:00Z', trash_root_id = id WHERE id = ?1;",
            [uuid_to_sql(f1.id.0)],
        )
        .unwrap();
        let unalloc_trashed_parent = query_unallocated_work_items(&conn, proj_a.id).unwrap();
        assert_eq!(
            unalloc_trashed_parent
                .iter()
                .map(|i| i.id)
                .collect::<Vec<_>>(),
            vec![w_other.id, w1.id]
        );
        assert!(
            attach_work_item_to_feature(
                &conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap(),
                w1.id,
                f1.id,
            )
            .is_err()
        );
    }

    #[test]
    fn global_assignees_crud_unique_handle_archive_and_cross_project_selection() {
        use crate::db::assignee_repo::{
            UpsertAssigneeInput, get_assignee, list_active_assignees, list_assignees,
            set_assignee_archived, upsert_assignee,
        };
        use crate::db::item_repo::{
            CreateFeatureInput, CreateWorkItemInput, UpdateItemFieldsInput, create_feature,
            create_work_item, get_item, update_item_fields,
        };
        use crate::db::project_repo::{CreateProjectInput, create_project};
        use crate::domain::{ItemStatus, Priority, ProjectStatus};
        use rusqlite::TransactionBehavior;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "AssigneeTest").unwrap();

        let (proj_a, proj_b) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let a = create_project(
                &tx,
                CreateProjectInput {
                    name: "Alpha".to_string(),
                    key_prefix: "ALP".to_string(),
                    description: String::new(),
                    color: "#6366F1".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            let b = create_project(
                &tx,
                CreateProjectInput {
                    name: "Beta".to_string(),
                    key_prefix: "BET".to_string(),
                    description: String::new(),
                    color: "#10B981".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            (a, b)
        };

        // Validation checks
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert!(
                upsert_assignee(
                    &tx,
                    UpsertAssigneeInput {
                        id: None,
                        name: "   ".to_string(),
                        handle: "MB".to_string(),
                        color: "#64748B".to_string(),
                        role: None,
                        notes: String::new(),
                    },
                )
                .is_err()
            );
            assert!(
                upsert_assignee(
                    &tx,
                    UpsertAssigneeInput {
                        id: None,
                        name: "Mark".to_string(),
                        handle: "TOO_LONG_12".to_string(),
                        color: "#64748B".to_string(),
                        role: None,
                        notes: String::new(),
                    },
                )
                .is_err()
            );
            assert!(
                upsert_assignee(
                    &tx,
                    UpsertAssigneeInput {
                        id: None,
                        name: "Mark".to_string(),
                        handle: "MB".to_string(),
                        color: "blue".to_string(),
                        role: None,
                        notes: String::new(),
                    },
                )
                .is_err()
            );
        }

        // Create two assignees
        let (mb, ai) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let (mb, created_mb) = upsert_assignee(
                &tx,
                UpsertAssigneeInput {
                    id: None,
                    name: "  Mark Benson ".to_string(),
                    handle: " MB ".to_string(),
                    color: "#6366f1".to_string(),
                    role: Some("  Founder ".to_string()),
                    notes: "Core".to_string(),
                },
            )
            .unwrap();
            assert!(created_mb);
            assert_eq!(mb.name, "Mark Benson");
            assert_eq!(mb.handle, "MB");
            assert_eq!(mb.color, "#6366F1");
            assert_eq!(mb.role.as_deref(), Some("Founder"));

            let (ai, created_ai) = upsert_assignee(
                &tx,
                UpsertAssigneeInput {
                    id: None,
                    name: "Claude Agent".to_string(),
                    handle: "AI".to_string(),
                    color: "#10B981".to_string(),
                    role: Some("   ".to_string()),
                    notes: String::new(),
                },
            )
            .unwrap();
            assert!(created_ai);
            assert_eq!(ai.role, None);
            tx.commit().unwrap();
            (mb, ai)
        };

        // Case-insensitive unique handle rejected on create, allowed on self-update
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert!(
                upsert_assignee(
                    &tx,
                    UpsertAssigneeInput {
                        id: None,
                        name: "Duplicate".to_string(),
                        handle: "mb".to_string(),
                        color: "#64748B".to_string(),
                        role: None,
                        notes: String::new(),
                    },
                )
                .is_err()
            );

            let (updated_mb, created) = upsert_assignee(
                &tx,
                UpsertAssigneeInput {
                    id: Some(mb.id),
                    name: "Mark Benson".to_string(),
                    handle: "mb".to_string(),
                    color: "#6366F1".to_string(),
                    role: Some("Lead".to_string()),
                    notes: "Updated".to_string(),
                },
            )
            .unwrap();
            assert!(!created);
            assert_eq!(updated_mb.handle, "mb");
            tx.commit().unwrap();
        }

        // INV-14: Global assignees are immediately selectable and assignable in both Project A and Project B
        let (fa, wb, wa_unassigned) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let fa = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: proj_a.id,
                    parent_feature_id: None,
                    title: "Alpha Core".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::High,
                },
            )
            .unwrap();
            let wa_unassigned = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: proj_a.id,
                    title: "New Task".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: Vec::new(),
                },
            )
            .unwrap();
            let wb = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: proj_b.id,
                    title: "Beta Task".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::Medium,
                    feature_ids: Vec::new(),
                },
            )
            .unwrap();

            update_item_fields(
                &tx,
                UpdateItemFieldsInput {
                    item_id: fa.id,
                    assignee_id: Some(Some(mb.id)),
                    ..Default::default()
                },
            )
            .unwrap();
            update_item_fields(
                &tx,
                UpdateItemFieldsInput {
                    item_id: wb.id,
                    assignee_id: Some(Some(mb.id)),
                    ..Default::default()
                },
            )
            .unwrap();
            tx.commit().unwrap();
            (fa, wb, wa_unassigned)
        };

        let snap_a = load_project_snapshot(&conn, proj_a.id).unwrap();
        let snap_b = load_project_snapshot(&conn, proj_b.id).unwrap();
        assert_eq!(snap_a.assignees.len(), 2);
        assert_eq!(snap_b.assignees.len(), 2);
        assert_eq!(snap_a.item(fa.id).unwrap().assignee_id, Some(mb.id));
        assert_eq!(snap_b.item(wb.id).unwrap().assignee_id, Some(mb.id));

        // Archive Mark Benson -> preserved on existing items, hidden from active pickers & new assignments
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let archived = set_assignee_archived(&tx, mb.id, true).unwrap();
            assert!(archived.is_archived());
            tx.commit().unwrap();
        }

        assert_eq!(list_assignees(&conn).unwrap().len(), 2);
        let active = list_active_assignees(&conn).unwrap();
        assert_eq!(active.iter().map(|a| a.id).collect::<Vec<_>>(), vec![ai.id]);

        let snap_a_arch = load_project_snapshot(&conn, proj_a.id).unwrap();
        assert_eq!(get_item(&conn, fa.id).unwrap().assignee_id, Some(mb.id));
        assert_eq!(
            snap_a_arch
                .selectable_assignees(None)
                .into_iter()
                .map(|a| a.id)
                .collect::<Vec<_>>(),
            vec![ai.id]
        );
        assert_eq!(
            snap_a_arch
                .selectable_assignees(Some(mb.id))
                .into_iter()
                .map(|a| a.id)
                .collect::<Vec<_>>(),
            vec![ai.id, mb.id]
        );

        // Assigning archived assignee to a new item is rejected
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert!(
                update_item_fields(
                    &tx,
                    UpdateItemFieldsInput {
                        item_id: wa_unassigned.id,
                        assignee_id: Some(Some(mb.id)),
                        ..Default::default()
                    },
                )
                .is_err()
            );
        }

        // Unarchive restores availability
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let restored = set_assignee_archived(&tx, mb.id, false).unwrap();
            assert!(!restored.is_archived());
            tx.commit().unwrap();
        }
        assert!(!get_assignee(&conn, mb.id).unwrap().is_archived());
        assert_eq!(list_active_assignees(&conn).unwrap().len(), 2);
    }

    #[test]
    fn project_scoped_tags_crud_isolation_and_composite_fk() {
        use crate::db::item_repo::{CreateWorkItemInput, create_work_item};
        use crate::db::project_repo::{CreateProjectInput, create_project};
        use crate::db::tag_repo::{
            UpsertTagInput, delete_tag, list_tag_ids_for_item, list_tags_for_project,
            set_item_tags, upsert_tag,
        };
        use crate::domain::{ItemStatus, Priority, ProjectStatus};
        use rusqlite::TransactionBehavior;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "TagTest").unwrap();

        let (proj_a, proj_b) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let a = create_project(
                &tx,
                CreateProjectInput {
                    name: "Alpha".to_string(),
                    key_prefix: "ALP".to_string(),
                    description: String::new(),
                    color: "#6366F1".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            let b = create_project(
                &tx,
                CreateProjectInput {
                    name: "Beta".to_string(),
                    key_prefix: "BET".to_string(),
                    description: String::new(),
                    color: "#10B981".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            (a, b)
        };

        let (wi_a, wi_b) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let wa = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: proj_a.id,
                    title: "Alpha item".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::Medium,
                    feature_ids: Vec::new(),
                },
            )
            .unwrap();
            let wb = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: proj_b.id,
                    title: "Beta item".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::Medium,
                    feature_ids: Vec::new(),
                },
            )
            .unwrap();
            tx.commit().unwrap();
            (wa, wb)
        };

        // Create tags in Project A and Project B (same name allowed across distinct projects)
        let (tag_a_bug, tag_a_ui, tag_b_bug) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let (ab, created1) = upsert_tag(
                &tx,
                UpsertTagInput {
                    id: None,
                    project_id: proj_a.id,
                    name: "  bug ".to_string(),
                    color: "#ef4444".to_string(),
                },
            )
            .unwrap();
            assert!(created1);
            assert_eq!(ab.name, "bug");
            assert_eq!(ab.color, "#EF4444");

            let (au, _) = upsert_tag(
                &tx,
                UpsertTagInput {
                    id: None,
                    project_id: proj_a.id,
                    name: "ui".to_string(),
                    color: "#0EA5E9".to_string(),
                },
            )
            .unwrap();

            // Case-insensitive duplicate within Project A rejected
            assert!(
                upsert_tag(
                    &tx,
                    UpsertTagInput {
                        id: None,
                        project_id: proj_a.id,
                        name: "BUG".to_string(),
                        color: "#EF4444".to_string(),
                    },
                )
                .is_err()
            );

            // Same name in Project B succeeds (INV-15)
            let (bb, _) = upsert_tag(
                &tx,
                UpsertTagInput {
                    id: None,
                    project_id: proj_b.id,
                    name: "bug".to_string(),
                    color: "#F59E0B".to_string(),
                },
            )
            .unwrap();

            tx.commit().unwrap();
            (ab, au, bb)
        };

        assert_eq!(list_tags_for_project(&conn, proj_a.id).unwrap().len(), 2);
        assert_eq!(list_tags_for_project(&conn, proj_b.id).unwrap().len(), 1);
        assert!(
            load_project_snapshot(&conn, proj_b.id)
                .unwrap()
                .tags
                .iter()
                .all(|t| t.project_id == proj_b.id)
        );

        // Attach Project A tags to Project A item
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            set_item_tags(&tx, wi_a.id, &[tag_a_ui.id, tag_a_bug.id]).unwrap();
            tx.commit().unwrap();
        }
        let snap_a = load_project_snapshot(&conn, proj_a.id).unwrap();
        assert_eq!(
            snap_a
                .tags_for_item(wi_a.id)
                .into_iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["bug", "ui"]
        );

        // Cross-project tag attachment rejected by repo AND by SQLite composite FK (INV-01, INV-15)
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert_eq!(
                set_item_tags(&tx, wi_b.id, &[tag_a_bug.id]).unwrap_err(),
                DomainError::CrossProjectReference
            );
        }
        assert!(
            conn.execute(
                "INSERT INTO item_tags (project_id, item_id, tag_id, created_at)
                 VALUES (?1, ?2, ?3, '2026-11-23T12:00:00Z');",
                params![
                    uuid_to_sql(proj_b.id.0),
                    uuid_to_sql(wi_b.id.0),
                    uuid_to_sql(tag_a_bug.id.0),
                ],
            )
            .is_err(),
            "SQLite composite FK must reject attaching Project A's tag to Project B's item"
        );
        assert!(
            conn.execute(
                "INSERT INTO item_tags (project_id, item_id, tag_id, created_at)
                 VALUES (?1, ?2, ?3, '2026-11-23T12:00:00Z');",
                params![
                    uuid_to_sql(proj_a.id.0),
                    uuid_to_sql(wi_b.id.0),
                    uuid_to_sql(tag_a_bug.id.0),
                ],
            )
            .is_err(),
            "SQLite composite FK must reject mismatched item project_id"
        );

        // Deleting a tag cascades item_tags without affecting other tags
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            delete_tag(&tx, tag_a_bug.id).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            list_tag_ids_for_item(&conn, wi_a.id).unwrap(),
            vec![tag_a_ui.id]
        );
        assert_eq!(list_tags_for_project(&conn, proj_b.id).unwrap(), vec![tag_b_bug]);
    }

    #[test]
    fn inv11_deduplicated_progress_rollup_cte_and_subtree_query() {
        use rusqlite::TransactionBehavior;

        use crate::db::item_repo::{
            CreateFeatureInput, CreateWorkItemInput, UpdateItemFieldsInput, create_feature,
            create_work_item, get_item, query_feature_progress_rollup, query_feature_subtree,
            update_item_fields,
        };
        use crate::db::project_repo::{CreateProjectInput, create_project};
        use crate::domain::{FeatureProgress, ItemStatus, Priority, ProjectStatus};

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "ProgressCteTest").unwrap();

        let pid = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let p = create_project(
                &tx,
                CreateProjectInput {
                    name: "Rollup".to_string(),
                    key_prefix: "ROL".to_string(),
                    description: String::new(),
                    color: "#6366F1".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            p.id
        };

        let (f1, f2a, f2b, f_empty, f_sub_done, f_sub_todo, f_leaf, wi_todo) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let f1 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: None,
                    title: "Parent F1".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::High,
                },
            )
            .unwrap();
            let f2a = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: Some(f1.id),
                    title: "Child F2a".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::Medium,
                },
            )
            .unwrap();
            let f2b = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: Some(f1.id),
                    title: "Child F2b".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::Medium,
                },
            )
            .unwrap();

            // Shared Done WI attached to both F2a and F2b
            let _wi_shared = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "Shared done task".to_string(),
                    description: String::new(),
                    status: ItemStatus::Done,
                    priority: Priority::High,
                    feature_ids: vec![f2a.id, f2b.id],
                },
            )
            .unwrap();

            // Open WI on F2b
            let wi_todo = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "Remaining task".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::Low,
                    feature_ids: vec![f2b.id],
                },
            )
            .unwrap();

            // Cancelled WI on F2a (excluded from denominators)
            let _wi_cancel = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "Cancelled task".to_string(),
                    description: String::new(),
                    status: ItemStatus::Cancelled,
                    priority: Priority::None,
                    feature_ids: vec![f2a.id],
                },
            )
            .unwrap();

            // Empty-WI fallback tree: F_empty has 1 cancelled WI + 3 child Features (Done, Todo, Cancelled)
            let f_empty = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: None,
                    title: "Empty WI Feature".to_string(),
                    description: String::new(),
                    status: ItemStatus::InProgress,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let f_sub_done = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: Some(f_empty.id),
                    title: "Sub Done".to_string(),
                    description: String::new(),
                    status: ItemStatus::Done,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let f_sub_todo = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: Some(f_empty.id),
                    title: "Sub Todo".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let _f_sub_cancel = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: Some(f_empty.id),
                    title: "Sub Cancelled".to_string(),
                    description: String::new(),
                    status: ItemStatus::Cancelled,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let _wi_empty_cancel = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "Cancelled in empty feature".to_string(),
                    description: String::new(),
                    status: ItemStatus::Cancelled,
                    priority: Priority::None,
                    feature_ids: vec![f_empty.id],
                },
            )
            .unwrap();

            let f_leaf = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: None,
                    title: "Leaf Feature".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();

            tx.commit().unwrap();
            (f1, f2a, f2b, f_empty, f_sub_done, f_sub_todo, f_leaf, wi_todo)
        };

        let rollup = query_feature_progress_rollup(&conn, pid).unwrap();
        let snap = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(snap.progress_by_feature, rollup);

        // Shared WI counted once under F1; cancelled WI excluded: 1/2 (50%)
        assert_eq!(
            rollup.get(&f1.id).copied(),
            Some(FeatureProgress {
                work_items_done: 1,
                work_items_total: 2,
                child_features_done: 0,
                child_features_total: 2,
            })
        );
        assert_eq!(snap.feature_progress_detail(f1.id).detailed_label(), "1/2 (50%)");
        assert_eq!(
            rollup.get(&f2a.id).copied(),
            Some(FeatureProgress {
                work_items_done: 1,
                work_items_total: 1,
                child_features_done: 0,
                child_features_total: 0,
            })
        );
        assert_eq!(
            rollup.get(&f2b.id).copied(),
            Some(FeatureProgress {
                work_items_done: 1,
                work_items_total: 2,
                child_features_done: 0,
                child_features_total: 0,
            })
        );

        // Empty-WI fallback: 1 Done / 2 non-cancelled child Features -> 50%
        let empty_prog = snap.feature_progress_detail(f_empty.id);
        assert!(empty_prog.is_using_feature_fallback());
        assert_eq!(empty_prog.effective_counts(), (1, 2));
        assert_eq!(empty_prog.detailed_label(), "1/2 (50%)");

        // Truly empty leaf
        let leaf_prog = snap.feature_progress_detail(f_leaf.id);
        assert!(leaf_prog.is_empty());
        assert_eq!(leaf_prog.detailed_label(), "—");

        // Mark wi_todo Done -> F1 becomes 2/2 (100%), while F1 & F2b statuses stay Todo (INV-10)
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            update_item_fields(
                &tx,
                UpdateItemFieldsInput {
                    item_id: wi_todo.id,
                    status: Some(ItemStatus::Done),
                    ..Default::default()
                },
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let snap_after = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(
            snap_after.feature_progress_detail(f1.id).detailed_label(),
            "2/2 (100%)"
        );
        assert!(snap_after.feature_progress_detail(f1.id).is_complete());
        assert_eq!(get_item(&conn, f1.id).unwrap().status, ItemStatus::Todo);
        assert_eq!(get_item(&conn, f2b.id).unwrap().status, ItemStatus::Todo);

        // query_feature_subtree scopes to F_empty and its descendants starting at depth 0
        let subtree = query_feature_subtree(&conn, pid, f_empty.id).unwrap();
        assert_eq!(subtree[0].id, f_empty.id);
        assert_eq!(subtree[0].depth, 0);
        let child_ids: Vec<_> = subtree[1..].iter().map(|n| n.id).collect();
        assert!(child_ids.contains(&f_sub_done.id));
        assert!(child_ids.contains(&f_sub_todo.id));
        assert!(!child_ids.contains(&f1.id));
        assert_eq!(subtree.len(), 4);
    }

    #[test]
    fn inv08_inv09_conversion_matrix_and_stale_trashed_link_pruning() {
        use crate::db::assignee_repo::{UpsertAssigneeInput, upsert_assignee};
        use crate::db::blocker_repo::add_blocker;
        use crate::db::item_repo::{
            CreateFeatureInput, CreateWorkItemInput, UpdateItemFieldsInput,
            check_feature_to_work_item_eligibility, check_work_item_to_feature_eligibility,
            convert_feature_to_work_item, convert_work_item_to_feature, create_feature,
            create_work_item, get_item, query_features_for_work_item,
            query_work_items_for_feature, update_item_fields,
        };
        use crate::db::project_repo::{CreateProjectInput, create_project};
        use crate::db::tag_repo::{UpsertTagInput, set_item_tags, upsert_tag};
        use crate::domain::{
            ConversionBlockReason, ConversionEligibility, ItemKind, ItemStatus, MAX_FEATURE_DEPTH,
            Priority, ProjectStatus,
        };
        use rusqlite::TransactionBehavior;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "ConvMatrix").unwrap();

        let (pid, assignee_id, tag_id) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let p = create_project(
                &tx,
                CreateProjectInput {
                    name: "Matrix".to_string(),
                    key_prefix: "MTX".to_string(),
                    description: String::new(),
                    color: "#6366F1".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            let (a, _) = upsert_assignee(
                &tx,
                UpsertAssigneeInput {
                    id: None,
                    name: "Mark".to_string(),
                    handle: "MB".to_string(),
                    color: "#6366F1".to_string(),
                    role: None,
                    notes: String::new(),
                },
            )
            .unwrap();
            let (t, _) = upsert_tag(
                &tx,
                UpsertTagInput {
                    id: None,
                    project_id: p.id,
                    name: "core".to_string(),
                    color: "#8B5CF6".to_string(),
                },
            )
            .unwrap();
            tx.commit().unwrap();
            (p.id, a.id, t.id)
        };

        // Helper Features: P (root), F_other (root), F_blocker (root)
        let (f_p, f_other, f_blocker) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let p = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: None,
                    title: "Parent P".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let o = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: None,
                    title: "Other".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let b = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: None,
                    title: "Blocker".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            (p, o, b)
        };

        // Case 1 & 5: Unallocated WI (with tags, assignee, description, outgoing blocker) -> Root Feature -> Unallocated WI
        let w_unalloc = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let w = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "Solo Task".to_string(),
                    description: "Detailed **markdown**".to_string(),
                    status: ItemStatus::InProgress,
                    priority: Priority::High,
                    feature_ids: vec![],
                },
            )
            .unwrap();
            update_item_fields(
                &tx,
                UpdateItemFieldsInput {
                    item_id: w.id,
                    assignee_id: Some(Some(assignee_id)),
                    ..Default::default()
                },
            )
            .unwrap();
            set_item_tags(&tx, w.id, &[tag_id]).unwrap();
            add_blocker(&tx, w.id, f_blocker.id).unwrap();
            tx.commit().unwrap();
            w
        };

        let snap = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(
            snap.conversion_eligibility(w_unalloc.id),
            Some(ConversionEligibility::Allowed {
                target_kind: ItemKind::Feature,
                resulting_parent: None,
                moves_to_unallocated: false,
            })
        );
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let feat = convert_work_item_to_feature(&tx, w_unalloc.id).unwrap();
            assert_eq!(feat.id, w_unalloc.id);
            assert_eq!(feat.item_key, w_unalloc.item_key);
            assert_eq!(feat.kind, ItemKind::Feature);
            assert_eq!(feat.parent_feature_id, None);
            assert_eq!(feat.assignee_id, Some(assignee_id));
            assert_eq!(feat.description, "Detailed **markdown**");
            tx.commit().unwrap();
        }
        let snap_root_feat = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(snap_root_feat.tags_for_item(w_unalloc.id).len(), 1);
        assert_eq!(snap_root_feat.direct_blockers(w_unalloc.id).len(), 1);
        assert!(snap_root_feat.unallocated_work_items().is_empty());

        // Case 5: Convert that leaf root Feature back to an Unallocated Work Item
        assert_eq!(
            snap_root_feat.conversion_eligibility(w_unalloc.id),
            Some(ConversionEligibility::Allowed {
                target_kind: ItemKind::WorkItem,
                resulting_parent: None,
                moves_to_unallocated: true,
            })
        );
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let (wi_back, old_parent) = convert_feature_to_work_item(&tx, w_unalloc.id).unwrap();
            assert_eq!(old_parent, None);
            assert_eq!(wi_back.item_key, w_unalloc.item_key);
            assert_eq!(wi_back.kind, ItemKind::WorkItem);
            assert_eq!(wi_back.parent_feature_id, None);
            assert_eq!(wi_back.assignee_id, Some(assignee_id));
            tx.commit().unwrap();
        }
        let snap_back = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(snap_back.unallocated_work_items().len(), 1);
        assert_eq!(snap_back.tags_for_item(w_unalloc.id).len(), 1);
        assert_eq!(snap_back.direct_blockers(w_unalloc.id).len(), 1);

        // Case 2 & 6: 1-Feature WI -> Child Feature under P -> Work Item attached to [P]
        let w_single = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let w = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "Under P".to_string(),
                    description: "Child body".to_string(),
                    status: ItemStatus::Todo,
                    priority: Priority::Medium,
                    feature_ids: vec![f_p.id],
                },
            )
            .unwrap();
            set_item_tags(&tx, w.id, &[tag_id]).unwrap();
            add_blocker(&tx, w.id, f_blocker.id).unwrap();
            tx.commit().unwrap();
            w
        };
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let child_feat = convert_work_item_to_feature(&tx, w_single.id).unwrap();
            assert_eq!(child_feat.kind, ItemKind::Feature);
            assert_eq!(child_feat.parent_feature_id, Some(f_p.id));
            assert_eq!(child_feat.item_key, w_single.item_key);
            assert!(query_features_for_work_item(&tx, w_single.id).unwrap().is_empty());
            tx.commit().unwrap();
        }
        let snap_child = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(snap_child.direct_blockers(w_single.id).len(), 1);
        assert_eq!(snap_child.tags_for_item(w_single.id).len(), 1);

        // Case 6: Leaf Child Feature (parent = P) -> Work Item attached to [P]
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let (wi, old_parent) = convert_feature_to_work_item(&tx, w_single.id).unwrap();
            assert_eq!(old_parent, Some(f_p.id));
            assert_eq!(wi.kind, ItemKind::WorkItem);
            assert_eq!(wi.parent_feature_id, None);
            assert_eq!(wi.item_key, w_single.item_key);
            let feats = query_features_for_work_item(&tx, w_single.id).unwrap();
            assert_eq!(feats.len(), 1);
            assert_eq!(feats[0].id, f_p.id);
            tx.commit().unwrap();
        }

        // Case 4: 2+-Feature WI -> Rejected (AttachedToMultipleFeatures)
        let w_shared = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let w = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "Shared WI".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![f_p.id, f_other.id],
                },
            )
            .unwrap();
            tx.commit().unwrap();
            w
        };
        assert_eq!(
            check_work_item_to_feature_eligibility(&conn, w_shared.id).unwrap(),
            ConversionEligibility::Blocked(vec![
                ConversionBlockReason::AttachedToMultipleFeatures(2)
            ])
        );
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let err = convert_work_item_to_feature(&tx, w_shared.id).unwrap_err();
            assert!(matches!(err, DomainError::ConversionBlocked(msg) if msg.contains("2 Features")));
        }

        // Case 3: 1-Feature WI attached to Feature at depth 31 -> Rejected (ExceedsMaxFeatureDepth)
        let deep_leaf_id = {
            let mut parent = Some(f_p.id); // f_p is depth 0; create depths 1..=31
            for d in 1..MAX_FEATURE_DEPTH {
                let tx = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                let f = create_feature(
                    &tx,
                    CreateFeatureInput {
                        project_id: pid,
                        parent_feature_id: parent,
                        title: format!("Deep {d}"),
                        description: String::new(),
                        status: ItemStatus::Todo,
                        priority: Priority::None,
                    },
                )
                .unwrap();
                tx.commit().unwrap();
                parent = Some(f.id);
            }
            parent.unwrap()
        };
        let w_at_depth_31 = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let w = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "Too deep WI".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![deep_leaf_id],
                },
            )
            .unwrap();
            tx.commit().unwrap();
            w
        };
        assert_eq!(
            check_work_item_to_feature_eligibility(&conn, w_at_depth_31.id).unwrap(),
            ConversionEligibility::Blocked(vec![ConversionBlockReason::ExceedsMaxFeatureDepth {
                max_depth: MAX_FEATURE_DEPTH
            }])
        );
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert!(matches!(
                convert_work_item_to_feature(&tx, w_at_depth_31.id),
                Err(DomainError::ConversionBlocked(_))
            ));
        }

        // Cases 7, 8, 9: Feature with active child Features, attached Work Items, and active dependents
        // f_p has: 1 direct child feature (Deep 1), 2 attached WIs (w_single, w_shared)
        let snap_guards = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(
            snap_guards.conversion_eligibility(f_p.id),
            Some(ConversionEligibility::Blocked(vec![
                ConversionBlockReason::HasChildFeatures(1),
                ConversionBlockReason::HasAttachedWorkItems(2),
            ]))
        );
        assert_eq!(
            check_feature_to_work_item_eligibility(&conn, f_p.id).unwrap(),
            ConversionEligibility::Blocked(vec![
                ConversionBlockReason::HasChildFeatures(1),
                ConversionBlockReason::HasAttachedWorkItems(2),
            ])
        );
        // f_blocker has 2 active dependents (w_unalloc, w_single)
        assert_eq!(
            check_feature_to_work_item_eligibility(&conn, f_blocker.id).unwrap(),
            ConversionEligibility::Blocked(vec![ConversionBlockReason::HasActiveDependents(2)])
        );
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let err_p = convert_feature_to_work_item(&tx, f_p.id).unwrap_err();
            assert!(matches!(
                err_p,
                DomainError::ConversionBlocked(msg)
                    if msg.contains("1 child Feature") && msg.contains("2 attached Work Items")
            ));
            let err_b = convert_feature_to_work_item(&tx, f_blocker.id).unwrap_err();
            assert!(matches!(
                err_b,
                DomainError::ConversionBlocked(msg) if msg.contains("2 items")
            ));
        }

        // Stale trashed-link pruning:
        // (a) WI attached to 1 active Feature + 1 trashed Feature converts cleanly to Feature.
        // (b) Leaf Feature with trashed child Feature, trashed attached WI, and trashed dependent converts cleanly to Work Item.
        let (f_trashed_parent, f_leaf_with_trash, wi_with_trashed_link) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let ftp = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: None,
                    title: "Trashed Feat".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let flt = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: None,
                    title: "Leaf With Trash".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let trashed_child = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: Some(flt.id),
                    title: "Trashed Child".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let trashed_wi = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "Trashed Attached WI".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![flt.id],
                },
            )
            .unwrap();
            let trashed_dep = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "Trashed Dependent".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![],
                },
            )
            .unwrap();
            add_blocker(&tx, trashed_dep.id, flt.id).unwrap();
            // Outgoing blocker on flt itself MUST survive conversion
            add_blocker(&tx, flt.id, f_blocker.id).unwrap();

            let wi_stale = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "Has Stale Link".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![f_other.id, ftp.id],
                },
            )
            .unwrap();
            tx.commit().unwrap();

            for tid in [ftp.id, trashed_child.id, trashed_wi.id, trashed_dep.id] {
                conn.execute(
                    "UPDATE items SET deleted_at = '2026-11-24T00:00:00Z', trash_root_id = id WHERE id = ?1;",
                    [uuid_to_sql(tid.0)],
                )
                .unwrap();
            }

            (ftp, flt, wi_stale)
        };

        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let converted_wi = convert_work_item_to_feature(&tx, wi_with_trashed_link.id).unwrap();
            assert_eq!(converted_wi.kind, ItemKind::Feature);
            assert_eq!(converted_wi.parent_feature_id, Some(f_other.id));

            let (converted_leaf, old_parent) =
                convert_feature_to_work_item(&tx, f_leaf_with_trash.id).unwrap();
            assert_eq!(old_parent, None);
            assert_eq!(converted_leaf.kind, ItemKind::WorkItem);
            assert!(
                query_work_items_for_feature(&tx, f_trashed_parent.id)
                    .unwrap()
                    .is_empty()
            );
            tx.commit().unwrap();
        }

        let final_snap = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(final_snap.direct_blockers(f_leaf_with_trash.id).len(), 1);
        assert_eq!(get_item(&conn, f_leaf_with_trash.id).unwrap().kind, ItemKind::WorkItem);
    }

    #[test]
    fn inv12_cascade_trash_shared_work_item_protection_restore_purge_and_empty() {
        use crate::db::blocker_repo::add_blocker;
        use crate::db::item_repo::{
            CreateFeatureInput, CreateWorkItemInput, convert_feature_to_work_item, create_feature,
            create_work_item, empty_project_trash, get_item, list_trash_groups, purge_trash_group,
            query_features_for_work_item, restore_trash_group, trash_item_subtree,
        };
        use crate::db::project_repo::{CreateProjectInput, create_project};
        use crate::domain::{
            BlockerWarning, EffectiveBlockState, ItemStatus, Priority, ProjectStatus,
        };
        use rusqlite::TransactionBehavior;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "TrashTest").unwrap();

        let (pid_a, pid_b) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let pa = create_project(
                &tx,
                CreateProjectInput {
                    name: "Alpha".to_string(),
                    key_prefix: "ALP".to_string(),
                    description: String::new(),
                    color: "#6366F1".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            let pb = create_project(
                &tx,
                CreateProjectInput {
                    name: "Beta".to_string(),
                    key_prefix: "BET".to_string(),
                    description: String::new(),
                    color: "#10B981".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            (pa.id, pb.id)
        };

        // Setup: F1 -> F2, F3, F_dep (blocked on F1), exclusive W2 on [F2], shared W1 on [F2, F3]
        let (f1, f2, f3, f_dep, w1_shared, w2_excl, fb) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let f1 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: None,
                    title: "Auth Epic".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::High,
                },
            )
            .unwrap();
            let f2 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: Some(f1.id),
                    title: "OAuth Flow".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::Medium,
                },
            )
            .unwrap();
            let f3 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: None,
                    title: "CLI Client".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::Medium,
                },
            )
            .unwrap();
            let f_dep = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: None,
                    title: "Launch App".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::High,
                },
            )
            .unwrap();
            add_blocker(&tx, f_dep.id, f1.id).unwrap();

            let w1 = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid_a,
                    title: "Token Refresh".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::High,
                    feature_ids: vec![f2.id, f3.id],
                },
            )
            .unwrap();
            let w2 = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid_a,
                    title: "Callback Handler".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::Low,
                    feature_ids: vec![f2.id],
                },
            )
            .unwrap();

            let fb = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_b,
                    parent_feature_id: None,
                    title: "Beta Trashed".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::Low,
                },
            )
            .unwrap();
            trash_item_subtree(&tx, fb.id).unwrap();
            tx.commit().unwrap();
            (f1, f2, f3, f_dep, w1, w2, fb)
        };

        // 1. Trash F1: F1, F2, W2 move to Trash; shared W1 stays active on F3; F_dep unblocks w/ InTrash warning
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let res = trash_item_subtree(&tx, f1.id).unwrap();
            assert_eq!(res.trashed_feature_ids, vec![f1.id, f2.id]);
            assert_eq!(res.trashed_work_item_ids, vec![w2_excl.id]);
            assert_eq!(res.spared_shared_work_item_count, 1);
            tx.commit().unwrap();
        }

        assert!(!get_item(&conn, f1.id).unwrap().is_active());
        assert_eq!(get_item(&conn, f2.id).unwrap().trash_root_id, Some(f1.id));
        assert_eq!(
            get_item(&conn, w2_excl.id).unwrap().trash_root_id,
            Some(f1.id)
        );
        assert!(get_item(&conn, w1_shared.id).unwrap().is_active());
        let w1_feats = query_features_for_work_item(&conn, w1_shared.id).unwrap();
        assert_eq!(w1_feats.iter().map(|f| f.id).collect::<Vec<_>>(), vec![f3.id]);

        let snap_trashed = load_project_snapshot(&conn, pid_a).unwrap();
        assert_eq!(snap_trashed.trash_count, 1);
        assert_eq!(snap_trashed.trash_groups.len(), 1);
        assert_eq!(snap_trashed.trash_groups[0].descendant_feature_count, 1);
        assert_eq!(snap_trashed.trash_groups[0].descendant_work_item_count, 1);
        assert_eq!(snap_trashed.trash_groups[0].total_item_count, 3);
        assert_eq!(
            snap_trashed.effective_block_state(f_dep.id),
            EffectiveBlockState::Unblocked
        );
        assert_eq!(
            snap_trashed.blocker_warnings(f_dep.id),
            vec![BlockerWarning::InTrash {
                blocker_id: f1.id,
                blocker_key: f1.item_key.clone(),
                title: f1.title.clone(),
            }]
        );

        // 2. Restore F1: F1, F2, W2 return active, W1 regains link to F2, F_dep is blocked again
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let res = restore_trash_group(&tx, f1.id).unwrap();
            assert_eq!(res.restored_count, 3);
            assert!(!res.promoted_to_root);
            assert_eq!(res.pruned_blocker_count, 0);
            tx.commit().unwrap();
        }

        let snap_restored = load_project_snapshot(&conn, pid_a).unwrap();
        assert_eq!(snap_restored.trash_count, 0);
        assert!(snap_restored.trash_groups.is_empty());
        assert_eq!(snap_restored.features_for_work_item(w1_shared.id).len(), 2);
        assert_eq!(
            snap_restored.effective_block_state(f_dep.id),
            EffectiveBlockState::DirectlyBlocked { active_count: 1 }
        );
        assert!(snap_restored.blocker_warnings(f_dep.id).is_empty());

        // 3. Trash F1 again and permanently purge F1: F1, F2, W2 deleted; W1 survives intact on F3
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            trash_item_subtree(&tx, f1.id).unwrap();
            let purged = purge_trash_group(&tx, f1.id).unwrap();
            assert_eq!(purged.purged_item_count, 3);
            assert_eq!(purged.root_key, f1.item_key);
            tx.commit().unwrap();
        }

        assert!(matches!(
            get_item(&conn, f1.id),
            Err(DomainError::ItemNotFound(_))
        ));
        assert!(matches!(
            get_item(&conn, f2.id),
            Err(DomainError::ItemNotFound(_))
        ));
        assert!(matches!(
            get_item(&conn, w2_excl.id),
            Err(DomainError::ItemNotFound(_))
        ));
        assert!(get_item(&conn, w1_shared.id).unwrap().is_active());
        let w1_feats_after = query_features_for_work_item(&conn, w1_shared.id).unwrap();
        assert_eq!(
            w1_feats_after.iter().map(|f| f.id).collect::<Vec<_>>(),
            vec![f3.id]
        );
        let snap_after_purge = load_project_snapshot(&conn, pid_a).unwrap();
        assert_eq!(snap_after_purge.trash_count, 0);
        assert!(snap_after_purge.direct_blockers(f_dep.id).is_empty());
        assert!(snap_after_purge.blocker_warnings(f_dep.id).is_empty());

        // FTS5 index integrity remains clean after cascade DELETE
        conn.execute("INSERT INTO items_fts(items_fts) VALUES('integrity-check');", [])
            .unwrap();

        // 4. Restore edge cases:
        //    (a) Parent currently in Trash promotes restored child to root (`promoted_to_root == true`)
        //    (b) Parent converted to Work Item restores child at root (`parent_feature_id == None`)
        //    (c) Purging a parent Trash group spares an earlier-trashed child group
        //    (d) Restoring with conflicting external blocker edges prunes cycles & WI exclusions (INV-05)
        let (child_promoted, child_conv_parent, orphan_child, f_cycle, wi_excl_pruned) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let p_trashed = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: None,
                    title: "Trashed Parent".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let c_promo = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: Some(p_trashed.id),
                    title: "Promoted Child".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            trash_item_subtree(&tx, c_promo.id).unwrap();
            trash_item_subtree(&tx, p_trashed.id).unwrap();

            let p_conv = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: None,
                    title: "Soon WorkItem".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let c_conv = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: Some(p_conv.id),
                    title: "Converted Parent Child".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            trash_item_subtree(&tx, c_conv.id).unwrap();
            convert_feature_to_work_item(&tx, p_conv.id).unwrap();

            let p_del = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: None,
                    title: "Parent Group".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let c_sep = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: Some(p_del.id),
                    title: "Earlier Trashed Child".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            trash_item_subtree(&tx, c_sep.id).unwrap();
            trash_item_subtree(&tx, p_del.id).unwrap();
            purge_trash_group(&tx, p_del.id).unwrap();

            // Cycle + WI exclusion on restore:
            // f_anc (root active) -> f_sub (child), f_ext (root active).
            // f_ext blocked on f_sub; w_shared on [f_sub, f3].
            // Trash f_sub -> while trashed, block f_anc on f_ext AND block w_shared on f_anc.
            // Restoring f_sub must prune both f_ext->f_sub (cycle) and w_shared->f_anc (ancestor exclusion).
            let f_anc = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: None,
                    title: "Active Ancestor".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let f_sub = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: Some(f_anc.id),
                    title: "Subtree Restored".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let f_ext = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: None,
                    title: "External Feature".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            add_blocker(&tx, f_ext.id, f_sub.id).unwrap();

            let w_shared_edge = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid_a,
                    title: "Shared Exclusion WI".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![f_sub.id, f3.id],
                },
            )
            .unwrap();

            trash_item_subtree(&tx, f_sub.id).unwrap();
            add_blocker(&tx, f_anc.id, f_ext.id).unwrap();
            add_blocker(&tx, w_shared_edge.id, f_anc.id).unwrap();

            tx.commit().unwrap();
            (c_promo, c_conv, c_sep, f_sub, w_shared_edge)
        };

        // Earlier-trashed separate child group survived parent purge
        assert!(!get_item(&conn, orphan_child.id).unwrap().is_active());
        assert_eq!(get_item(&conn, orphan_child.id).unwrap().parent_feature_id, None);

        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let promo_res = restore_trash_group(&tx, child_promoted.id).unwrap();
            assert!(promo_res.promoted_to_root);
            assert_eq!(promo_res.root.parent_feature_id, None);

            let conv_res = restore_trash_group(&tx, child_conv_parent.id).unwrap();
            assert_eq!(conv_res.root.parent_feature_id, None);

            let cycle_res = restore_trash_group(&tx, f_cycle.id).unwrap();
            assert_eq!(cycle_res.pruned_blocker_count, 2);
            tx.commit().unwrap();
        }
        let snap_pruned = load_project_snapshot(&conn, pid_a).unwrap();
        assert!(snap_pruned.direct_blockers(wi_excl_pruned.id).is_empty());

        // 5. Empty Trash deletes all remaining trashed items in Project A while isolating Project B
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let emptied = empty_project_trash(&tx, pid_a).unwrap();
            assert_eq!(emptied.purged_group_count, 2);
            assert_eq!(emptied.purged_item_count, 2);
            tx.commit().unwrap();
        }
        assert!(list_trash_groups(&conn, pid_a).unwrap().is_empty());
        assert_eq!(list_trash_groups(&conn, pid_b).unwrap().len(), 1);
        assert!(!get_item(&conn, fb.id).unwrap().is_active());
        assert!(get_item(&conn, w1_shared.id).unwrap().is_active());
    }

    #[test]
    fn fts5_search_ranks_exact_key_first_supports_prefixes_punctuation_and_project_scope() {
        use crate::db::item_repo::{
            CreateFeatureInput, CreateWorkItemInput, build_fts_query, create_feature,
            create_work_item, search_items_fts, trash_item_subtree,
        };
        use crate::db::project_repo::{CreateProjectInput, create_project};
        use crate::domain::{ItemStatus, Priority, ProjectStatus};
        use rusqlite::TransactionBehavior;

        assert_eq!(build_fts_query("PRO-12"), Some("\"PRO\"* \"12\"*".to_string()));
        assert_eq!(build_fts_query("  --- \"foo:bar\"  "), Some("\"foo\"* \"bar\"*".to_string()));
        assert_eq!(build_fts_query("   !!!   "), None);

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "SearchTest").unwrap();

        let (pid_a, pid_b) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let pa = create_project(
                &tx,
                CreateProjectInput {
                    name: "Alpha".to_string(),
                    key_prefix: "PRO".to_string(),
                    description: String::new(),
                    color: "#6366F1".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            let pb = create_project(
                &tx,
                CreateProjectInput {
                    name: "Beta".to_string(),
                    key_prefix: "BET".to_string(),
                    description: String::new(),
                    color: "#10B981".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            (pa.id, pb.id)
        };

        let (pro_1, pro_2, _pro_3, _pro_10, bet_1) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            // PRO-1
            let p1 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: None,
                    title: "Schema Migrations".to_string(),
                    description: "Core database setup".to_string(),
                    status: ItemStatus::InProgress,
                    priority: Priority::High,
                },
            )
            .unwrap();
            // PRO-2: mentions PRO-1 in body
            let p2 = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid_a,
                    title: "Document upgrade steps".to_string(),
                    description: "See PRO-1 for migration rules".to_string(),
                    status: ItemStatus::Todo,
                    priority: Priority::Low,
                    feature_ids: vec![],
                },
            )
            .unwrap();
            // PRO-3: trashed item should never appear
            let p3 = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid_a,
                    title: "Trashed migration draft".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![],
                },
            )
            .unwrap();
            trash_item_subtree(&tx, p3.id).unwrap();

            // Advance sequence to PRO-10 so PRO-1 vs PRO-10 prefix ranking is tested
            for n in 4..=9 {
                create_work_item(
                    &tx,
                    CreateWorkItemInput {
                        project_id: pid_a,
                        title: format!("Filler {n}"),
                        description: String::new(),
                        status: ItemStatus::Todo,
                        priority: Priority::None,
                        feature_ids: vec![],
                    },
                )
                .unwrap();
            }
            let p10 = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid_a,
                    title: "Migrate telemetry pipeline".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::Medium,
                    feature_ids: vec![],
                },
            )
            .unwrap();

            // BET-1 in Project B
            let b1 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_b,
                    parent_feature_id: None,
                    title: "Beta Migration Helper".to_string(),
                    description: "Cross project search check".to_string(),
                    status: ItemStatus::Todo,
                    priority: Priority::Urgent,
                },
            )
            .unwrap();

            tx.commit().unwrap();
            (p1, p2, p3, p10, b1)
        };

        // Exact key match ranks #1 ahead of body mention (PRO-2) and prefix match (PRO-10)
        let key_hits = search_items_fts(&conn, "PRO-1", Some(pid_a), 50).unwrap();
        assert!(!key_hits.is_empty());
        assert_eq!(key_hits[0].item_key, "PRO-1");
        assert_eq!(key_hits[0].id, pro_1.id);
        assert!(key_hits.iter().any(|r| r.id == pro_2.id));

        // Prefix stem query matches PRO-1 and PRO-10 while excluding trashed PRO-3 and Project B BET-1
        let scoped_hits = search_items_fts(&conn, "migrat", Some(pid_a), 50).unwrap();
        let scoped_keys: Vec<_> = scoped_hits.iter().map(|r| r.item_key.as_str()).collect();
        assert!(scoped_keys.contains(&"PRO-1"));
        assert!(scoped_keys.contains(&"PRO-10"));
        assert!(!scoped_keys.contains(&"PRO-3"));
        assert!(!scoped_keys.contains(&"BET-1"));

        // Global search (project_id = None) includes BET-1 with project prefix
        let global_hits = search_items_fts(&conn, "migrat", None, 50).unwrap();
        let bet_hit = global_hits.iter().find(|r| r.id == bet_1.id).unwrap();
        assert_eq!(bet_hit.project_key_prefix, "BET");

        // Arbitrary punctuation never fails
        assert!(search_items_fts(&conn, "\"unterminated: (foo*bar)", None, 50)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn dnd_reorder_feature_work_items_unallocated_and_atomic_move() {
        use crate::db::blocker_repo::add_blocker;
        use crate::db::item_repo::{
            CreateFeatureInput, CreateWorkItemInput, create_feature, create_work_item,
            move_work_item_between_features, query_unallocated_work_items,
            query_work_items_for_feature, reorder_feature_work_items,
            reorder_unallocated_work_items,
        };
        use crate::db::project_repo::{CreateProjectInput, create_project};
        use crate::domain::{ItemStatus, Priority, ProjectStatus};
        use rusqlite::TransactionBehavior;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "DndRepoTest").unwrap();

        let pid = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let p = create_project(
                &tx,
                CreateProjectInput {
                    name: "DnD".to_string(),
                    key_prefix: "DND".to_string(),
                    description: String::new(),
                    color: "#6366F1".to_string(),
                    status: ProjectStatus::Active,
                },
            )
            .unwrap();
            tx.commit().unwrap();
            p.id
        };

        let (fa, fb, fc, w1, w2, w3, u1, u2, u3) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let fa = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: None,
                    title: "Feature A".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let fb = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: None,
                    title: "Feature B".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let fc = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid,
                    parent_feature_id: None,
                    title: "Blocker C".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            let w1 = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "W1".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![fa.id, fb.id],
                },
            )
            .unwrap();
            let w2 = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "W2".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![fa.id, fb.id],
                },
            )
            .unwrap();
            let w3 = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "W3".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![fa.id],
                },
            )
            .unwrap();
            add_blocker(&tx, w3.id, fc.id).unwrap();

            let u1 = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "U1".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![],
                },
            )
            .unwrap();
            let u2 = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "U2".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![],
                },
            )
            .unwrap();
            let u3 = create_work_item(
                &tx,
                CreateWorkItemInput {
                    project_id: pid,
                    title: "U3".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                    feature_ids: vec![],
                },
            )
            .unwrap();

            tx.commit().unwrap();
            (fa, fb, fc, w1, w2, w3, u1, u2, u3)
        };

        // Reorder W3 before W1 inside Feature A; Feature B order [W1, W2] remains untouched
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            reorder_feature_work_items(&tx, pid, fa.id, w3.id, None, Some(w1.id)).unwrap();
            tx.commit().unwrap();
        }
        let fa_wis: Vec<_> = query_work_items_for_feature(&conn, fa.id)
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        let fb_wis: Vec<_> = query_work_items_for_feature(&conn, fb.id)
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(fa_wis, vec![w3.id, w1.id, w2.id]);
        assert_eq!(fb_wis, vec![w1.id, w2.id]);

        // Reorder unallocated U3 before U1, and verify allocated W1 is rejected
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            reorder_unallocated_work_items(&tx, pid, u3.id, None, Some(u1.id)).unwrap();
            assert!(reorder_unallocated_work_items(&tx, pid, w1.id, None, None).is_err());
            tx.commit().unwrap();
        }
        let unalloc_ids: Vec<_> = query_unallocated_work_items(&conn, pid)
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(unalloc_ids, vec![u3.id, u1.id, u2.id]);

        // Move W3 from FA to FC (blocker) must fail atomically without detaching FA
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert!(move_work_item_between_features(&tx, w3.id, fa.id, fc.id).is_err());
        }
        assert!(
            query_work_items_for_feature(&conn, fa.id)
                .unwrap()
                .iter()
                .any(|i| i.id == w3.id)
        );

        // Move W3 from FA to FB succeeds and detaches FA
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            move_work_item_between_features(&tx, w3.id, fa.id, fb.id).unwrap();
            tx.commit().unwrap();
        }
        assert!(
            !query_work_items_for_feature(&conn, fa.id)
                .unwrap()
                .iter()
                .any(|i| i.id == w3.id)
        );
        assert!(
            query_work_items_for_feature(&conn, fb.id)
                .unwrap()
                .iter()
                .any(|i| i.id == w3.id)
        );
    }

    #[test]
    fn migration_001_to_002_upgrades_existing_db_with_backup_and_supports_item_links() {
        use crate::db::item_repo::{
            UpdateItemFieldsInput, get_item, search_items_fts, update_item_fields,
        };
        use crate::db::migrations::MIGRATION_001_SQL;
        use crate::domain::ItemLink;
        use rusqlite::TransactionBehavior;

        let dir = TempDir::new().unwrap();
        let fresh_path = dir.path().join("fresh_v2.protrack");
        {
            let mut conn = Connection::open(&fresh_path).unwrap();
            let meta = inspect_and_migrate(&mut conn, Some(&fresh_path), "Fresh").unwrap();
            assert_eq!(meta.schema_version, APP_MAX_SCHEMA_VERSION);
            checkpoint_and_switch_to_delete_mode(&conn).unwrap();
        }
        assert!(
            !dir.path().join("fresh_v2.protrack.v1.bak").exists(),
            "fresh v2 workspace must not create a spurious .v1.bak"
        );

        let db_path = dir.path().join("legacy_v1.protrack");
        let now = "2026-11-23T12:00:00Z";
        let proj_id = ProjectId::new();
        let item_id = ItemId::new();
        {
            let mut conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            tx.execute_batch(MIGRATION_001_SQL).unwrap();
            tx.execute(
                "INSERT INTO workspace_meta (id, schema_version, app_min_version, workspace_name, created_at, updated_at)
                 VALUES (1, 1, '0.1.0', 'Legacy', ?1, ?1);",
                [now],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO projects (id, name, key_prefix, sort_order, created_at, updated_at)
                 VALUES (?1, 'Legacy', 'LEG', 'a0', ?2, ?2);",
                params![uuid_to_sql(proj_id.0), now],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO items (id, project_id, item_number, item_key, kind, title, sort_order, created_at, updated_at)
                 VALUES (?1, ?2, 1, 'LEG-1', 'feature', 'Legacy Feature', 'a0', ?3, ?3);",
                params![uuid_to_sql(item_id.0), uuid_to_sql(proj_id.0), now],
            )
            .unwrap();
            tx.execute_batch("PRAGMA user_version = 1;").unwrap();
            tx.commit().unwrap();
            checkpoint_and_switch_to_delete_mode(&conn).unwrap();
        }

        let v1_bytes = fs::read(&db_path).unwrap();
        let mut conn = Connection::open(&db_path).unwrap();
        let meta = inspect_and_migrate(&mut conn, Some(&db_path), "Legacy").unwrap();
        assert_eq!(meta.schema_version, APP_MAX_SCHEMA_VERSION);
        let bak_path = dir.path().join("legacy_v1.protrack.v1.bak");
        assert!(bak_path.exists());
        assert_eq!(fs::read(&bak_path).unwrap(), v1_bytes);

        let existing = get_item(&conn, item_id).unwrap();
        assert_eq!(existing.link, None);

        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let updated = update_item_fields(
                &tx,
                UpdateItemFieldsInput {
                    item_id,
                    link: Some(Some(ItemLink {
                        url: "https://acme.atlassian.net/browse/abc-123".to_string(),
                        label: "".to_string(),
                    })),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                updated.link,
                Some(ItemLink {
                    url: "https://acme.atlassian.net/browse/abc-123".to_string(),
                    label: "ABC-123".to_string(),
                })
            );
            tx.commit().unwrap();
        }

        let hits = search_items_fts(&conn, "ABC-123", Some(proj_id), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, item_id);
        assert_eq!(
            hits[0].link,
            Some(ItemLink {
                url: "https://acme.atlassian.net/browse/abc-123".to_string(),
                label: "ABC-123".to_string(),
            })
        );

        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let cleared = update_item_fields(
                &tx,
                UpdateItemFieldsInput {
                    item_id,
                    link: Some(None),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(cleared.link, None);
            tx.commit().unwrap();
        }

        conn.execute(
            "INSERT INTO items_fts(items_fts) VALUES ('integrity-check');",
            [],
        )
        .unwrap();
    }
}

