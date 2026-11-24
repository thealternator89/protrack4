use std::collections::{HashMap, HashSet};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use crate::db::assignee_repo;
use crate::db::project_repo::{allocate_next_item_key, get_project};
use crate::db::{
    map_sqlite_err, parse_domain_col, parse_ts_col, parse_uuid_col, ts_to_sql, uuid_to_sql,
};
use crate::domain::fractional_index;
use crate::domain::{
    AssigneeId, DomainError, FeatureProgress, FeatureWorkItem, Item, ItemId, ItemKind,
    ItemStatus, MAX_FEATURE_DEPTH, Priority, ProjectId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureTreeNode {
    pub id: ItemId,
    pub project_id: ProjectId,
    pub parent_feature_id: Option<ItemId>,
    pub item_key: String,
    pub title: String,
    pub status: ItemStatus,
    pub priority: Priority,
    pub assignee_id: Option<AssigneeId>,
    pub sort_order: String,
    pub depth: usize,
    pub sort_path: String,
}

#[derive(Debug, Clone)]
pub struct CreateFeatureInput {
    pub project_id: ProjectId,
    pub parent_feature_id: Option<ItemId>,
    pub title: String,
    pub description: String,
    pub status: ItemStatus,
    pub priority: Priority,
}

#[derive(Debug, Clone)]
pub struct CreateWorkItemInput {
    pub project_id: ProjectId,
    pub title: String,
    pub description: String,
    pub status: ItemStatus,
    pub priority: Priority,
    pub feature_ids: Vec<ItemId>,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateItemFieldsInput {
    pub item_id: ItemId,
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<ItemStatus>,
    pub priority: Option<Priority>,
    pub assignee_id: Option<Option<AssigneeId>>,
}

pub fn validate_item_title(title: &str) -> Result<String, DomainError> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err(DomainError::Validation(
            "Item title cannot be empty".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
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
}

pub fn get_item(conn: &Connection, item_id: ItemId) -> Result<Item, DomainError> {
    conn.query_row(
        "SELECT id, project_id, item_number, item_key, kind, parent_feature_id,
                title, description, status, priority, assignee_id, sort_order,
                deleted_at, trash_root_id, created_at, updated_at
         FROM items
         WHERE id = ?1;",
        params![uuid_to_sql(item_id.0)],
        row_to_item,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => DomainError::ItemNotFound(item_id),
        other => map_sqlite_err(other),
    })
}

/// Executes the recursive Feature Tree CTE (`DATA_MODEL.md` §4.1).
pub fn query_feature_tree(
    conn: &Connection,
    project_id: ProjectId,
) -> Result<Vec<FeatureTreeNode>, DomainError> {
    let mut stmt = conn
        .prepare(
            "WITH RECURSIVE feature_tree AS (
                SELECT
                    id,
                    project_id,
                    parent_feature_id,
                    item_key,
                    title,
                    status,
                    priority,
                    assignee_id,
                    sort_order,
                    0 AS depth,
                    sort_order AS sort_path
                FROM items
                WHERE project_id = ?1
                  AND kind = 'feature'
                  AND parent_feature_id IS NULL
                  AND deleted_at IS NULL

                UNION ALL

                SELECT
                    c.id,
                    c.project_id,
                    c.parent_feature_id,
                    c.item_key,
                    c.title,
                    c.status,
                    c.priority,
                    c.assignee_id,
                    c.sort_order,
                    ft.depth + 1 AS depth,
                    ft.sort_path || '/' || c.sort_order AS sort_path
                FROM items c
                JOIN feature_tree ft ON c.parent_feature_id = ft.id
                WHERE c.kind = 'feature'
                  AND c.deleted_at IS NULL
                  AND ft.depth < 31
            )
            SELECT
                id, project_id, parent_feature_id, item_key, title, status,
                priority, assignee_id, sort_order, depth, sort_path
            FROM feature_tree
            ORDER BY sort_path ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(params![uuid_to_sql(project_id.0)], |row| {
            let parent_opt: Option<String> = row.get(2)?;
            let parent_feature_id = parent_opt
                .map(|raw| parse_uuid_col(2, raw).map(ItemId::from_uuid))
                .transpose()?;
            let status_raw: String = row.get(5)?;
            let status = parse_domain_col(5, ItemStatus::from_db_str(&status_raw))?;
            let priority_raw: String = row.get(6)?;
            let priority = parse_domain_col(6, Priority::from_db_str(&priority_raw))?;
            let assignee_opt: Option<String> = row.get(7)?;
            let assignee_id = assignee_opt
                .map(|raw| parse_uuid_col(7, raw).map(AssigneeId::from_uuid))
                .transpose()?;
            let depth_i64: i64 = row.get(9)?;
            let depth = usize::try_from(depth_i64).unwrap_or(0);

            Ok(FeatureTreeNode {
                id: ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                project_id: ProjectId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                parent_feature_id,
                item_key: row.get(3)?,
                title: row.get(4)?,
                status,
                priority,
                assignee_id,
                sort_order: row.get(8)?,
                depth,
                sort_path: row.get(10)?,
            })
        })
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

/// Executes the recursive Feature Tree CTE scoped to `root_feature_id` and its descendants.
pub fn query_feature_subtree(
    conn: &Connection,
    project_id: ProjectId,
    root_feature_id: ItemId,
) -> Result<Vec<FeatureTreeNode>, DomainError> {
    let mut stmt = conn
        .prepare(
            "WITH RECURSIVE feature_tree AS (
                SELECT
                    id,
                    project_id,
                    parent_feature_id,
                    item_key,
                    title,
                    status,
                    priority,
                    assignee_id,
                    sort_order,
                    0 AS depth,
                    sort_order AS sort_path
                FROM items
                WHERE project_id = ?1
                  AND id = ?2
                  AND kind = 'feature'
                  AND deleted_at IS NULL

                UNION ALL

                SELECT
                    c.id,
                    c.project_id,
                    c.parent_feature_id,
                    c.item_key,
                    c.title,
                    c.status,
                    c.priority,
                    c.assignee_id,
                    c.sort_order,
                    ft.depth + 1 AS depth,
                    ft.sort_path || '/' || c.sort_order AS sort_path
                FROM items c
                JOIN feature_tree ft ON c.parent_feature_id = ft.id
                WHERE c.kind = 'feature'
                  AND c.deleted_at IS NULL
                  AND ft.depth < 31
            )
            SELECT
                id, project_id, parent_feature_id, item_key, title, status,
                priority, assignee_id, sort_order, depth, sort_path
            FROM feature_tree
            ORDER BY sort_path ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(
            params![uuid_to_sql(project_id.0), uuid_to_sql(root_feature_id.0)],
            |row| {
                let parent_opt: Option<String> = row.get(2)?;
                let parent_feature_id = parent_opt
                    .map(|raw| parse_uuid_col(2, raw).map(ItemId::from_uuid))
                    .transpose()?;
                let status_raw: String = row.get(5)?;
                let status = parse_domain_col(5, ItemStatus::from_db_str(&status_raw))?;
                let priority_raw: String = row.get(6)?;
                let priority = parse_domain_col(6, Priority::from_db_str(&priority_raw))?;
                let assignee_opt: Option<String> = row.get(7)?;
                let assignee_id = assignee_opt
                    .map(|raw| parse_uuid_col(7, raw).map(AssigneeId::from_uuid))
                    .transpose()?;
                let depth_i64: i64 = row.get(9)?;
                let depth = usize::try_from(depth_i64).unwrap_or(0);

                Ok(FeatureTreeNode {
                    id: ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                    project_id: ProjectId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                    parent_feature_id,
                    item_key: row.get(3)?,
                    title: row.get(4)?,
                    status,
                    priority,
                    assignee_id,
                    sort_order: row.get(8)?,
                    depth,
                    sort_path: row.get(10)?,
                })
            },
        )
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

/// Executes the deduplicated subtree progress roll-up CTE (`DATA_MODEL.md` §4.5 / `INV-11`).
pub fn query_feature_progress_rollup(
    conn: &Connection,
    project_id: ProjectId,
) -> Result<HashMap<ItemId, FeatureProgress>, DomainError> {
    let mut stmt = conn
        .prepare(
            "WITH RECURSIVE feature_closure(root_id, descendant_id, hop) AS (
                SELECT id, id, 0
                FROM items
                WHERE project_id = ?1
                  AND kind = 'feature'
                  AND deleted_at IS NULL
                UNION ALL
                SELECT fc.root_id, c.id, fc.hop + 1
                FROM feature_closure fc
                JOIN items c ON c.parent_feature_id = fc.descendant_id
                WHERE c.kind = 'feature'
                  AND c.deleted_at IS NULL
                  AND fc.hop < 31
            ),
            distinct_subtree_wis AS (
                SELECT DISTINCT
                    fc.root_id AS feature_id,
                    wi.id AS work_item_id,
                    wi.status AS wi_status
                FROM feature_closure fc
                JOIN feature_work_items fwi ON fwi.feature_id = fc.descendant_id
                JOIN items wi ON wi.id = fwi.work_item_id
                WHERE wi.deleted_at IS NULL
                  AND wi.status != 'cancelled'
            ),
            wi_rollup AS (
                SELECT
                    feature_id,
                    COUNT(*) AS wi_total,
                    SUM(CASE WHEN wi_status = 'done' THEN 1 ELSE 0 END) AS wi_done
                FROM distinct_subtree_wis
                GROUP BY feature_id
            ),
            descendant_feature_rollup AS (
                SELECT
                    fc.root_id AS feature_id,
                    COUNT(*) AS feat_total,
                    SUM(CASE WHEN df.status = 'done' THEN 1 ELSE 0 END) AS feat_done
                FROM feature_closure fc
                JOIN items df ON df.id = fc.descendant_id
                WHERE fc.hop > 0
                  AND df.status != 'cancelled'
                GROUP BY fc.root_id
            )
            SELECT
                f.id AS feature_id,
                COALESCE(w.wi_done, 0) AS work_items_done,
                COALESCE(w.wi_total, 0) AS work_items_total,
                COALESCE(d.feat_done, 0) AS child_features_done,
                COALESCE(d.feat_total, 0) AS child_features_total
            FROM items f
            LEFT JOIN wi_rollup w ON w.feature_id = f.id
            LEFT JOIN descendant_feature_rollup d ON d.feature_id = f.id
            WHERE f.project_id = ?1
              AND f.kind = 'feature'
              AND f.deleted_at IS NULL;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(params![uuid_to_sql(project_id.0)], |row| {
            let feature_id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
            let wi_done: i64 = row.get(1)?;
            let wi_total: i64 = row.get(2)?;
            let feat_done: i64 = row.get(3)?;
            let feat_total: i64 = row.get(4)?;
            Ok((
                feature_id,
                FeatureProgress {
                    work_items_done: usize::try_from(wi_done).unwrap_or(0),
                    work_items_total: usize::try_from(wi_total).unwrap_or(0),
                    child_features_done: usize::try_from(feat_done).unwrap_or(0),
                    child_features_total: usize::try_from(feat_total).unwrap_or(0),
                },
            ))
        })
        .map_err(map_sqlite_err)?;

    let mut map = HashMap::new();
    for pair in rows {
        let (fid, progress) = pair.map_err(map_sqlite_err)?;
        map.insert(fid, progress);
    }
    Ok(map)
}

/// Returns zero-based depth of a Feature (`0` for root Feature).
pub fn feature_depth(conn: &Connection, feature_id: ItemId) -> Result<usize, DomainError> {
    let max_depth: Option<i64> = conn
        .query_row(
            "WITH RECURSIVE ancestors(id, parent_id, depth) AS (
                SELECT id, parent_feature_id, 0
                FROM items
                WHERE id = ?1
                UNION ALL
                SELECT p.id, p.parent_feature_id, anc.depth + 1
                FROM items p
                JOIN ancestors anc ON p.id = anc.parent_id
                WHERE anc.depth < 64
            )
            SELECT MAX(depth) FROM ancestors;",
            params![uuid_to_sql(feature_id.0)],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sqlite_err)?
        .flatten();

    match max_depth {
        Some(depth) => Ok(usize::try_from(depth).unwrap_or(0)),
        None => Err(DomainError::ItemNotFound(feature_id)),
    }
}

/// Returns the maximum descendant depth offset below `feature_id` (`0` for a leaf Feature).
pub fn feature_subtree_height(conn: &Connection, feature_id: ItemId) -> Result<usize, DomainError> {
    let max_offset: i64 = conn
        .query_row(
            "WITH RECURSIVE subtree(id, offset) AS (
                SELECT id, 0
                FROM items
                WHERE id = ?1 AND kind = 'feature' AND deleted_at IS NULL
                UNION ALL
                SELECT c.id, s.offset + 1
                FROM items c
                JOIN subtree s ON c.parent_feature_id = s.id
                WHERE c.kind = 'feature' AND c.deleted_at IS NULL AND s.offset < 64
            )
            SELECT COALESCE(MAX(offset), 0) FROM subtree;",
            params![uuid_to_sql(feature_id.0)],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;

    Ok(usize::try_from(max_offset).unwrap_or(0))
}

/// Returns `true` when `candidate_id` is a strict active descendant Feature of `ancestor_id`.
pub fn is_descendant_of(
    conn: &Connection,
    candidate_id: ItemId,
    ancestor_id: ItemId,
) -> Result<bool, DomainError> {
    let found: i64 = conn
        .query_row(
            "WITH RECURSIVE descendants(id, depth) AS (
                SELECT id, 1
                FROM items
                WHERE parent_feature_id = ?1
                  AND kind = 'feature'
                  AND deleted_at IS NULL
                UNION ALL
                SELECT c.id, d.depth + 1
                FROM items c
                JOIN descendants d ON c.parent_feature_id = d.id
                WHERE c.kind = 'feature'
                  AND c.deleted_at IS NULL
                  AND d.depth < 64
            )
            SELECT EXISTS(SELECT 1 FROM descendants WHERE id = ?2);",
            params![uuid_to_sql(ancestor_id.0), uuid_to_sql(candidate_id.0)],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;

    Ok(found != 0)
}

fn load_sibling_feature_keys(
    tx: &Transaction<'_>,
    project_id: ProjectId,
    parent_feature_id: Option<ItemId>,
    exclude_id: Option<ItemId>,
) -> Result<Vec<(ItemId, String)>, DomainError> {
    let project_id_sql = uuid_to_sql(project_id.0);
    let parent_sql = parent_feature_id.map(|id| uuid_to_sql(id.0));
    let exclude_sql = exclude_id.map(|id| uuid_to_sql(id.0));

    let mut stmt = tx
        .prepare(
            "SELECT id, sort_order
             FROM items
             WHERE project_id = ?1
               AND kind = 'feature'
               AND deleted_at IS NULL
               AND (?2 IS NULL AND parent_feature_id IS NULL OR parent_feature_id = ?2)
               AND (?3 IS NULL OR id != ?3)
             ORDER BY sort_order ASC, item_number ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(params![project_id_sql, parent_sql, exclude_sql], |row| {
            let id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
            let sort_order: String = row.get(1)?;
            Ok((id, sort_order))
        })
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

fn rebalance_sibling_feature_ids(
    tx: &Transaction<'_>,
    ordered_ids: &[ItemId],
    now_sql: &str,
) -> Result<Vec<String>, DomainError> {
    let keys = fractional_index::rebalance_keys(ordered_ids.len());
    for (id, key) in ordered_ids.iter().zip(keys.iter()) {
        tx.execute(
            "UPDATE items SET sort_order = ?1, updated_at = ?2 WHERE id = ?3;",
            params![key, now_sql, uuid_to_sql(id.0)],
        )
        .map_err(map_sqlite_err)?;
    }
    Ok(keys)
}

fn append_sort_order_for_parent(
    tx: &Transaction<'_>,
    project_id: ProjectId,
    parent_feature_id: Option<ItemId>,
    exclude_id: Option<ItemId>,
    now_sql: &str,
) -> Result<String, DomainError> {
    let siblings = load_sibling_feature_keys(tx, project_id, parent_feature_id, exclude_id)?;
    let Some((_, last_key)) = siblings.last() else {
        return Ok(fractional_index::first());
    };

    match fractional_index::after(last_key) {
        Ok(next) if !fractional_index::needs_rebalance(&next) => Ok(next),
        _ => {
            let mut ids: Vec<ItemId> = siblings.iter().map(|(id, _)| *id).collect();
            // Generate N+1 evenly spaced keys so existing siblings get [0..N-1] and the new tail gets [N].
            let keys = fractional_index::rebalance_keys(ids.len() + 1);
            for (id, key) in ids.drain(..).zip(keys.iter()) {
                tx.execute(
                    "UPDATE items SET sort_order = ?1, updated_at = ?2 WHERE id = ?3;",
                    params![key, now_sql, uuid_to_sql(id.0)],
                )
                .map_err(map_sqlite_err)?;
            }
            Ok(keys.last().cloned().unwrap_or_else(fractional_index::first))
        }
    }
}

pub fn create_feature(tx: &Transaction<'_>, input: CreateFeatureInput) -> Result<Item, DomainError> {
    let title = validate_item_title(&input.title)?;
    let _project = get_project(tx, input.project_id)?;

    if let Some(parent_id) = input.parent_feature_id {
        let parent = get_item(tx, parent_id)?;
        if parent.project_id != input.project_id {
            return Err(DomainError::CrossProjectReference);
        }
        if !parent.is_active() {
            return Err(DomainError::Validation(
                "Cannot create a sub-Feature under a trashed Feature".to_string(),
            ));
        }
        if parent.kind != ItemKind::Feature {
            return Err(DomainError::Validation(
                "Parent of a Feature must be a Feature".to_string(),
            ));
        }
        let parent_depth = feature_depth(tx, parent_id)?;
        if parent_depth + 1 >= MAX_FEATURE_DEPTH {
            return Err(DomainError::MaxFeatureDepthExceeded {
                max_depth: MAX_FEATURE_DEPTH,
            });
        }
    }

    let now = Utc::now();
    let now_sql = ts_to_sql(now);
    let sort_order = append_sort_order_for_parent(
        tx,
        input.project_id,
        input.parent_feature_id,
        None,
        &now_sql,
    )?;
    let (item_number, item_key) = allocate_next_item_key(tx, input.project_id)?;

    let item = Item {
        id: ItemId::new(),
        project_id: input.project_id,
        item_number,
        item_key,
        kind: ItemKind::Feature,
        parent_feature_id: input.parent_feature_id,
        title,
        description: input.description,
        status: input.status,
        priority: input.priority,
        assignee_id: None,
        sort_order,
        deleted_at: None,
        trash_root_id: None,
        created_at: now,
        updated_at: now,
    };

    tx.execute(
        "INSERT INTO items (
            id, project_id, item_number, item_key, kind, parent_feature_id,
            title, description, status, priority, assignee_id, sort_order,
            deleted_at, trash_root_id, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, NULL, ?13, ?14);",
        params![
            uuid_to_sql(item.id.0),
            uuid_to_sql(item.project_id.0),
            item.item_number,
            &item.item_key,
            item.kind.as_str(),
            item.parent_feature_id.map(|id| uuid_to_sql(id.0)),
            &item.title,
            &item.description,
            item.status.as_str(),
            item.priority.as_str(),
            item.assignee_id.map(|id| uuid_to_sql(id.0)),
            &item.sort_order,
            &now_sql,
            &now_sql,
        ],
    )
    .map_err(map_sqlite_err)?;

    Ok(item)
}

pub fn update_item_fields(
    tx: &Transaction<'_>,
    input: UpdateItemFieldsInput,
) -> Result<Item, DomainError> {
    let existing = get_item(tx, input.item_id)?;
    if !existing.is_active() {
        return Err(DomainError::Validation(
            "Cannot edit an item in Trash".to_string(),
        ));
    }

    let title = match input.title {
        Some(t) => validate_item_title(&t)?,
        None => existing.title.clone(),
    };
    let description = input.description.unwrap_or_else(|| existing.description.clone());
    let status = input.status.unwrap_or(existing.status);
    let priority = input.priority.unwrap_or(existing.priority);
    let assignee_id = match input.assignee_id {
        Some(Some(aid)) => {
            let assignee = assignee_repo::get_assignee(tx, aid)?;
            if existing.assignee_id != Some(aid) && assignee.is_archived() {
                return Err(DomainError::Validation(format!(
                    "Assignee '{}' is archived and cannot be assigned to new items",
                    assignee.name
                )));
            }
            Some(aid)
        }
        Some(None) => None,
        None => existing.assignee_id,
    };

    let now = Utc::now();
    let now_sql = ts_to_sql(now);

    tx.execute(
        "UPDATE items
         SET title = ?1,
             description = ?2,
             status = ?3,
             priority = ?4,
             assignee_id = ?5,
             updated_at = ?6
         WHERE id = ?7;",
        params![
            &title,
            &description,
            status.as_str(),
            priority.as_str(),
            assignee_id.map(|id| uuid_to_sql(id.0)),
            &now_sql,
            uuid_to_sql(existing.id.0),
        ],
    )
    .map_err(map_sqlite_err)?;

    Ok(Item {
        title,
        description,
        status,
        priority,
        assignee_id,
        updated_at: now,
        ..existing
    })
}

pub fn reparent_feature(
    tx: &Transaction<'_>,
    feature_id: ItemId,
    new_parent_id: Option<ItemId>,
) -> Result<Item, DomainError> {
    let feature = get_item(tx, feature_id)?;
    if !feature.is_active() {
        return Err(DomainError::Validation(
            "Cannot move a Feature that is in Trash".to_string(),
        ));
    }
    if feature.kind != ItemKind::Feature {
        return Err(DomainError::Validation(
            "Only Features can have a parent Feature".to_string(),
        ));
    }

    if feature.parent_feature_id == new_parent_id {
        return Ok(feature);
    }

    if let Some(pid) = new_parent_id {
        if pid == feature_id {
            return Err(DomainError::Validation(
                "A Feature cannot be its own parent".to_string(),
            ));
        }

        let parent = get_item(tx, pid)?;
        if parent.project_id != feature.project_id {
            return Err(DomainError::CrossProjectReference);
        }
        if !parent.is_active() {
            return Err(DomainError::Validation(
                "Cannot move a Feature under a trashed Feature".to_string(),
            ));
        }
        if parent.kind != ItemKind::Feature {
            return Err(DomainError::Validation(
                "Target parent must be a Feature".to_string(),
            ));
        }
        if is_descendant_of(tx, pid, feature_id)? {
            return Err(DomainError::DependencyCycle(
                "Cannot move a Feature under one of its own descendants".to_string(),
            ));
        }

        let target_parent_depth = feature_depth(tx, pid)?;
        let subtree_height = feature_subtree_height(tx, feature_id)?;
        if target_parent_depth + 1 + subtree_height >= MAX_FEATURE_DEPTH {
            return Err(DomainError::MaxFeatureDepthExceeded {
                max_depth: MAX_FEATURE_DEPTH,
            });
        }

        crate::db::blocker_repo::validate_reparent_blockers(
            tx,
            feature.project_id,
            feature_id,
            pid,
        )?;
    }

    let now = Utc::now();
    let now_sql = ts_to_sql(now);
    let new_sort_order = append_sort_order_for_parent(
        tx,
        feature.project_id,
        new_parent_id,
        Some(feature_id),
        &now_sql,
    )?;

    tx.execute(
        "UPDATE items
         SET parent_feature_id = ?1,
             sort_order = ?2,
             updated_at = ?3
         WHERE id = ?4;",
        params![
            new_parent_id.map(|id| uuid_to_sql(id.0)),
            &new_sort_order,
            &now_sql,
            uuid_to_sql(feature_id.0),
        ],
    )
    .map_err(map_sqlite_err)?;

    Ok(Item {
        parent_feature_id: new_parent_id,
        sort_order: new_sort_order,
        updated_at: now,
        ..feature
    })
}

/// Moves `feature_id` within its sibling list according to `before_sibling_id` and/or `after_sibling_id`.
///
/// - `(Some(b), Some(a))`: place strictly after `b` and before `a`.
/// - `(Some(b), None)`: place immediately after `b`.
/// - `(None, Some(a))`: place immediately before `a`.
/// - `(None, None)`: place at the end of siblings.
pub fn reorder_sibling_features(
    tx: &Transaction<'_>,
    project_id: ProjectId,
    feature_id: ItemId,
    before_sibling_id: Option<ItemId>,
    after_sibling_id: Option<ItemId>,
) -> Result<Item, DomainError> {
    let feature = get_item(tx, feature_id)?;
    if feature.project_id != project_id {
        return Err(DomainError::CrossProjectReference);
    }
    if !feature.is_active() {
        return Err(DomainError::Validation(
            "Cannot reorder a trashed Feature".to_string(),
        ));
    }
    if feature.kind != ItemKind::Feature {
        return Err(DomainError::Validation(
            "Only Features can be reordered with reorder_sibling_features".to_string(),
        ));
    }

    let siblings =
        load_sibling_feature_keys(tx, project_id, feature.parent_feature_id, Some(feature_id))?;

    let before_idx = match before_sibling_id {
        Some(bid) => {
            if bid == feature_id {
                return Err(DomainError::Validation(
                    "before_sibling_id cannot equal feature_id".to_string(),
                ));
            }
            let pos = siblings.iter().position(|(id, _)| *id == bid).ok_or_else(|| {
                DomainError::Validation("before_sibling_id is not a sibling Feature".to_string())
            })?;
            Some(pos)
        }
        None => None,
    };

    let after_idx = match after_sibling_id {
        Some(aid) => {
            if aid == feature_id {
                return Err(DomainError::Validation(
                    "after_sibling_id cannot equal feature_id".to_string(),
                ));
            }
            let pos = siblings.iter().position(|(id, _)| *id == aid).ok_or_else(|| {
                DomainError::Validation("after_sibling_id is not a sibling Feature".to_string())
            })?;
            Some(pos)
        }
        None => None,
    };

    if let (Some(b), Some(a)) = (before_idx, after_idx)
        && b >= a
    {
        return Err(DomainError::Validation(
            "before_sibling_id must precede after_sibling_id among siblings".to_string(),
        ));
    }

    let (insert_idx, lower_key, upper_key): (usize, Option<&str>, Option<&str>) =
        match (before_idx, after_idx) {
            (Some(b), Some(_a)) => (
                b + 1,
                Some(siblings[b].1.as_str()),
                siblings.get(b + 1).map(|(_, k)| k.as_str()),
            ),
            (Some(b), None) => (
                b + 1,
                Some(siblings[b].1.as_str()),
                siblings.get(b + 1).map(|(_, k)| k.as_str()),
            ),
            (None, Some(a)) => (
                a,
                a.checked_sub(1).map(|i| siblings[i].1.as_str()),
                Some(siblings[a].1.as_str()),
            ),
            (None, None) => (
                siblings.len(),
                siblings.last().map(|(_, k)| k.as_str()),
                None,
            ),
        };

    let now = Utc::now();
    let now_sql = ts_to_sql(now);

    let sort_order = match fractional_index::midpoint(lower_key, upper_key) {
        Ok(key) if !fractional_index::needs_rebalance(&key) => {
            tx.execute(
                "UPDATE items SET sort_order = ?1, updated_at = ?2 WHERE id = ?3;",
                params![&key, &now_sql, uuid_to_sql(feature_id.0)],
            )
            .map_err(map_sqlite_err)?;
            key
        }
        _ => {
            let mut ordered_ids: Vec<ItemId> = siblings.iter().map(|(id, _)| *id).collect();
            ordered_ids.insert(insert_idx, feature_id);
            let keys = rebalance_sibling_feature_ids(tx, &ordered_ids, &now_sql)?;
            keys[insert_idx].clone()
        }
    };

    Ok(Item {
        sort_order,
        updated_at: now,
        ..feature
    })
}

fn dedupe_preserve_order(ids: &[ItemId]) -> Vec<ItemId> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids {
        if seen.insert(id) {
            out.push(id);
        }
    }
    out
}

fn validate_active_feature_in_project(
    conn: &Connection,
    project_id: ProjectId,
    feature_id: ItemId,
) -> Result<Item, DomainError> {
    let feat = get_item(conn, feature_id)?;
    if feat.project_id != project_id {
        return Err(DomainError::CrossProjectReference);
    }
    if !feat.is_active() {
        return Err(DomainError::Validation(
            "Cannot attach a Work Item to a trashed Feature".to_string(),
        ));
    }
    if feat.kind != ItemKind::Feature {
        return Err(DomainError::Validation(
            "Work Items can only be attached to Features".to_string(),
        ));
    }
    Ok(feat)
}

fn validate_active_work_item(conn: &Connection, work_item_id: ItemId) -> Result<Item, DomainError> {
    let item = get_item(conn, work_item_id)?;
    if !item.is_active() {
        return Err(DomainError::Validation(
            "Cannot modify Feature attachments of a trashed Work Item".to_string(),
        ));
    }
    if item.kind != ItemKind::WorkItem {
        return Err(DomainError::Validation(
            "Only Work Items can be attached to Features".to_string(),
        ));
    }
    Ok(item)
}

fn append_work_item_sort_order(
    tx: &Transaction<'_>,
    project_id: ProjectId,
    now_sql: &str,
) -> Result<String, DomainError> {
    let mut stmt = tx
        .prepare(
            "SELECT id, sort_order
             FROM items
             WHERE project_id = ?1
               AND kind = 'work_item'
               AND deleted_at IS NULL
             ORDER BY sort_order ASC, item_number ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(params![uuid_to_sql(project_id.0)], |row| {
            let id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
            let sort_order: String = row.get(1)?;
            Ok((id, sort_order))
        })
        .map_err(map_sqlite_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_err)?;
    drop(stmt);

    let Some((_, last_key)) = rows.last() else {
        return Ok(fractional_index::first());
    };

    match fractional_index::after(last_key) {
        Ok(next) if !fractional_index::needs_rebalance(&next) => Ok(next),
        _ => {
            let keys = fractional_index::rebalance_keys(rows.len() + 1);
            for ((id, _), key) in rows.iter().zip(keys.iter()) {
                tx.execute(
                    "UPDATE items SET sort_order = ?1, updated_at = ?2 WHERE id = ?3;",
                    params![key, now_sql, uuid_to_sql(id.0)],
                )
                .map_err(map_sqlite_err)?;
            }
            Ok(keys.last().cloned().unwrap_or_else(fractional_index::first))
        }
    }
}

fn append_fwi_sort_order(
    tx: &Transaction<'_>,
    project_id: ProjectId,
    feature_id: ItemId,
) -> Result<String, DomainError> {
    let project_sql = uuid_to_sql(project_id.0);
    let feature_sql = uuid_to_sql(feature_id.0);

    let mut stmt = tx
        .prepare(
            "SELECT work_item_id, sort_order
             FROM feature_work_items
             WHERE project_id = ?1
               AND feature_id = ?2
             ORDER BY sort_order ASC, work_item_id ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(params![&project_sql, &feature_sql], |row| {
            let id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
            let sort_order: String = row.get(1)?;
            Ok((id, sort_order))
        })
        .map_err(map_sqlite_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_err)?;
    drop(stmt);

    let Some((_, last_key)) = rows.last() else {
        return Ok(fractional_index::first());
    };

    match fractional_index::after(last_key) {
        Ok(next) if !fractional_index::needs_rebalance(&next) => Ok(next),
        _ => {
            let keys = fractional_index::rebalance_keys(rows.len() + 1);
            for ((wi_id, _), key) in rows.iter().zip(keys.iter()) {
                tx.execute(
                    "UPDATE feature_work_items
                     SET sort_order = ?1
                     WHERE feature_id = ?2 AND work_item_id = ?3;",
                    params![key, &feature_sql, uuid_to_sql(wi_id.0)],
                )
                .map_err(map_sqlite_err)?;
            }
            Ok(keys.last().cloned().unwrap_or_else(fractional_index::first))
        }
    }
}

pub fn create_work_item(
    tx: &Transaction<'_>,
    input: CreateWorkItemInput,
) -> Result<Item, DomainError> {
    let title = validate_item_title(&input.title)?;
    let _project = get_project(tx, input.project_id)?;

    let feature_ids = dedupe_preserve_order(&input.feature_ids);
    for &fid in &feature_ids {
        validate_active_feature_in_project(tx, input.project_id, fid)?;
    }

    let now = Utc::now();
    let now_sql = ts_to_sql(now);
    let sort_order = append_work_item_sort_order(tx, input.project_id, &now_sql)?;
    let (item_number, item_key) = allocate_next_item_key(tx, input.project_id)?;

    let item = Item {
        id: ItemId::new(),
        project_id: input.project_id,
        item_number,
        item_key,
        kind: ItemKind::WorkItem,
        parent_feature_id: None,
        title,
        description: input.description,
        status: input.status,
        priority: input.priority,
        assignee_id: None,
        sort_order,
        deleted_at: None,
        trash_root_id: None,
        created_at: now,
        updated_at: now,
    };

    let project_sql = uuid_to_sql(item.project_id.0);
    let item_sql = uuid_to_sql(item.id.0);

    tx.execute(
        "INSERT INTO items (
            id, project_id, item_number, item_key, kind, parent_feature_id,
            title, description, status, priority, assignee_id, sort_order,
            deleted_at, trash_root_id, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, 'work_item', NULL, ?5, ?6, ?7, ?8, ?9, ?10, NULL, NULL, ?11, ?12);",
        params![
            &item_sql,
            &project_sql,
            item.item_number,
            &item.item_key,
            &item.title,
            &item.description,
            item.status.as_str(),
            item.priority.as_str(),
            item.assignee_id.map(|id| uuid_to_sql(id.0)),
            &item.sort_order,
            &now_sql,
            &now_sql,
        ],
    )
    .map_err(map_sqlite_err)?;

    for fid in feature_ids {
        let fwi_sort_order = append_fwi_sort_order(tx, item.project_id, fid)?;
        tx.execute(
            "INSERT INTO feature_work_items (project_id, feature_id, work_item_id, sort_order, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5);",
            params![
                &project_sql,
                uuid_to_sql(fid.0),
                &item_sql,
                &fwi_sort_order,
                &now_sql,
            ],
        )
        .map_err(map_sqlite_err)?;
    }

    Ok(item)
}

/// Executes the Unallocated Bucket query (`DATA_MODEL.md` §4.2 / `INV-03`).
pub fn query_unallocated_work_items(
    conn: &Connection,
    project_id: ProjectId,
) -> Result<Vec<Item>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT i.id, i.project_id, i.item_number, i.item_key, i.kind, i.parent_feature_id,
                    i.title, i.description, i.status, i.priority, i.assignee_id, i.sort_order,
                    i.deleted_at, i.trash_root_id, i.created_at, i.updated_at
             FROM items i
             WHERE i.project_id = ?1
               AND i.kind = 'work_item'
               AND i.deleted_at IS NULL
               AND NOT EXISTS (
                   SELECT 1
                   FROM feature_work_items fwi
                   JOIN items f ON f.id = fwi.feature_id
                   WHERE fwi.work_item_id = i.id
                     AND f.deleted_at IS NULL
               )
             ORDER BY i.sort_order ASC, i.item_number ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(params![uuid_to_sql(project_id.0)], row_to_item)
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

/// Returns all active Features currently attached to `work_item_id`.
pub fn query_features_for_work_item(
    conn: &Connection,
    work_item_id: ItemId,
) -> Result<Vec<Item>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT f.id, f.project_id, f.item_number, f.item_key, f.kind, f.parent_feature_id,
                    f.title, f.description, f.status, f.priority, f.assignee_id, f.sort_order,
                    f.deleted_at, f.trash_root_id, f.created_at, f.updated_at
             FROM feature_work_items fwi
             JOIN items f ON f.id = fwi.feature_id
             WHERE fwi.work_item_id = ?1
               AND f.deleted_at IS NULL
               AND f.kind = 'feature'
             ORDER BY f.item_number ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(params![uuid_to_sql(work_item_id.0)], row_to_item)
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

/// Returns active `feature_work_items` links for `work_item_id`.
pub fn query_feature_attachments_for_work_item(
    conn: &Connection,
    work_item_id: ItemId,
) -> Result<Vec<FeatureWorkItem>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT fwi.project_id, fwi.feature_id, fwi.work_item_id, fwi.sort_order, fwi.created_at
             FROM feature_work_items fwi
             JOIN items f ON f.id = fwi.feature_id
             WHERE fwi.work_item_id = ?1
               AND f.deleted_at IS NULL
             ORDER BY fwi.feature_id ASC, fwi.sort_order ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(params![uuid_to_sql(work_item_id.0)], |row| {
            Ok(FeatureWorkItem {
                project_id: ProjectId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                feature_id: ItemId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                work_item_id: ItemId::from_uuid(parse_uuid_col(2, row.get(2)?)?),
                sort_order: row.get(3)?,
                created_at: parse_ts_col(4, row.get(4)?)?,
            })
        })
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

/// Returns active Work Items attached to `feature_id` ordered by per-Feature `sort_order`.
pub fn query_work_items_for_feature(
    conn: &Connection,
    feature_id: ItemId,
) -> Result<Vec<Item>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT wi.id, wi.project_id, wi.item_number, wi.item_key, wi.kind, wi.parent_feature_id,
                    wi.title, wi.description, wi.status, wi.priority, wi.assignee_id, wi.sort_order,
                    wi.deleted_at, wi.trash_root_id, wi.created_at, wi.updated_at
             FROM feature_work_items fwi
             JOIN items wi ON wi.id = fwi.work_item_id
             WHERE fwi.feature_id = ?1
               AND wi.deleted_at IS NULL
               AND wi.kind = 'work_item'
             ORDER BY fwi.sort_order ASC, wi.item_number ASC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(params![uuid_to_sql(feature_id.0)], row_to_item)
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

/// Replaces the set of active Features attached to `work_item_id`, preserving `sort_order`
/// on retained links and leaving any inert links to trashed Features untouched.
pub fn set_work_item_features(
    tx: &Transaction<'_>,
    work_item_id: ItemId,
    feature_ids: &[ItemId],
) -> Result<Item, DomainError> {
    let work_item = validate_active_work_item(tx, work_item_id)?;
    let desired_order = dedupe_preserve_order(feature_ids);
    for &fid in &desired_order {
        validate_active_feature_in_project(tx, work_item.project_id, fid)?;
        crate::db::blocker_repo::validate_attach_work_item_blockers(tx, work_item_id, fid)?;
    }

    let wi_sql = uuid_to_sql(work_item_id.0);
    let project_sql = uuid_to_sql(work_item.project_id.0);

    let current_active: Vec<ItemId> = {
        let mut stmt = tx
            .prepare(
                "SELECT fwi.feature_id
                 FROM feature_work_items fwi
                 JOIN items f ON f.id = fwi.feature_id
                 WHERE fwi.work_item_id = ?1
                   AND f.deleted_at IS NULL;",
            )
            .map_err(map_sqlite_err)?;
        let rows = stmt
            .query_map(params![&wi_sql], |row| {
                Ok(ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?))
            })
            .map_err(map_sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)?
    };

    let current_set: HashSet<ItemId> = current_active.iter().copied().collect();
    let desired_set: HashSet<ItemId> = desired_order.iter().copied().collect();

    let mut mutated = false;
    for existing_fid in &current_active {
        if !desired_set.contains(existing_fid) {
            let deleted = tx
                .execute(
                    "DELETE FROM feature_work_items
                     WHERE feature_id = ?1 AND work_item_id = ?2;",
                    params![uuid_to_sql(existing_fid.0), &wi_sql],
                )
                .map_err(map_sqlite_err)?;
            if deleted > 0 {
                mutated = true;
            }
        }
    }

    let now = Utc::now();
    let now_sql = ts_to_sql(now);

    for desired_fid in desired_order {
        if !current_set.contains(&desired_fid) {
            let fwi_sort_order = append_fwi_sort_order(tx, work_item.project_id, desired_fid)?;
            tx.execute(
                "INSERT INTO feature_work_items (project_id, feature_id, work_item_id, sort_order, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5);",
                params![
                    &project_sql,
                    uuid_to_sql(desired_fid.0),
                    &wi_sql,
                    &fwi_sort_order,
                    &now_sql,
                ],
            )
            .map_err(map_sqlite_err)?;
            mutated = true;
        }
    }

    if !mutated {
        return Ok(work_item);
    }

    tx.execute(
        "UPDATE items SET updated_at = ?1 WHERE id = ?2;",
        params![&now_sql, &wi_sql],
    )
    .map_err(map_sqlite_err)?;

    Ok(Item {
        updated_at: now,
        ..work_item
    })
}

pub fn attach_work_item_to_feature(
    tx: &Transaction<'_>,
    work_item_id: ItemId,
    feature_id: ItemId,
) -> Result<FeatureWorkItem, DomainError> {
    let work_item = validate_active_work_item(tx, work_item_id)?;
    let _feature = validate_active_feature_in_project(tx, work_item.project_id, feature_id)?;
    crate::db::blocker_repo::validate_attach_work_item_blockers(tx, work_item_id, feature_id)?;

    let wi_sql = uuid_to_sql(work_item_id.0);
    let feat_sql = uuid_to_sql(feature_id.0);
    let project_sql = uuid_to_sql(work_item.project_id.0);

    if let Some(existing) = tx
        .query_row(
            "SELECT project_id, feature_id, work_item_id, sort_order, created_at
             FROM feature_work_items
             WHERE feature_id = ?1 AND work_item_id = ?2;",
            params![&feat_sql, &wi_sql],
            |row| {
                Ok(FeatureWorkItem {
                    project_id: ProjectId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                    feature_id: ItemId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                    work_item_id: ItemId::from_uuid(parse_uuid_col(2, row.get(2)?)?),
                    sort_order: row.get(3)?,
                    created_at: parse_ts_col(4, row.get(4)?)?,
                })
            },
        )
        .optional()
        .map_err(map_sqlite_err)?
    {
        return Ok(existing);
    }

    let now = Utc::now();
    let now_sql = ts_to_sql(now);
    let sort_order = append_fwi_sort_order(tx, work_item.project_id, feature_id)?;

    tx.execute(
        "INSERT INTO feature_work_items (project_id, feature_id, work_item_id, sort_order, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5);",
        params![&project_sql, &feat_sql, &wi_sql, &sort_order, &now_sql],
    )
    .map_err(map_sqlite_err)?;

    tx.execute(
        "UPDATE items SET updated_at = ?1 WHERE id = ?2;",
        params![&now_sql, &wi_sql],
    )
    .map_err(map_sqlite_err)?;

    Ok(FeatureWorkItem {
        project_id: work_item.project_id,
        feature_id,
        work_item_id,
        sort_order,
        created_at: now,
    })
}

pub fn detach_work_item_from_feature(
    tx: &Transaction<'_>,
    work_item_id: ItemId,
    feature_id: ItemId,
) -> Result<bool, DomainError> {
    let work_item = validate_active_work_item(tx, work_item_id)?;
    let feat_project: Option<String> = tx
        .query_row(
            "SELECT project_id FROM items WHERE id = ?1;",
            params![uuid_to_sql(feature_id.0)],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sqlite_err)?;

    if let Some(pid_raw) = feat_project {
        let pid = ProjectId::from_uuid(parse_uuid_col(0, pid_raw).map_err(map_sqlite_err)?);
        if pid != work_item.project_id {
            return Err(DomainError::CrossProjectReference);
        }
    }

    let wi_sql = uuid_to_sql(work_item_id.0);
    let deleted = tx
        .execute(
            "DELETE FROM feature_work_items WHERE feature_id = ?1 AND work_item_id = ?2;",
            params![uuid_to_sql(feature_id.0), &wi_sql],
        )
        .map_err(map_sqlite_err)?;

    if deleted > 0 {
        let now_sql = ts_to_sql(Utc::now());
        tx.execute(
            "UPDATE items SET updated_at = ?1 WHERE id = ?2;",
            params![&now_sql, &wi_sql],
        )
        .map_err(map_sqlite_err)?;
        Ok(true)
    } else {
        Ok(false)
    }
}
