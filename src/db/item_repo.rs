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
    AssigneeId, ConversionBlockReason, ConversionEligibility, DomainError, FeatureProgress,
    FeatureWorkItem, Item, ItemId, ItemKind, ItemLink, ItemStatus, MAX_FEATURE_DEPTH, Priority,
    ProjectId, TrashGroup, format_conversion_block_message, suggest_link_label, validate_item_link,
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
    pub link: Option<Option<ItemLink>>,
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
    let link_url_opt: Option<String> = row.get(16)?;
    let link_label_opt: Option<String> = row.get(17)?;
    let link = link_url_opt.and_then(|raw_url| {
        let trimmed_url = raw_url.trim();
        if trimmed_url.is_empty() {
            return None;
        }
        let label = link_label_opt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| suggest_link_label(trimmed_url))
            .unwrap_or_else(|| "Link".to_string());
        Some(ItemLink {
            url: trimmed_url.to_string(),
            label,
        })
    });

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
        link,
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
                deleted_at, trash_root_id, created_at, updated_at, link_url, link_label
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
        link: None,
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
            deleted_at, trash_root_id, created_at, updated_at, link_url, link_label
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, NULL, ?13, ?14, NULL, NULL);",
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
    let link = match input.link {
        Some(Some(raw_link)) => Some(validate_item_link(&raw_link.url, &raw_link.label)?),
        Some(None) => None,
        None => existing.link.clone(),
    };
    let (link_url, link_label) = match &link {
        Some(l) => (Some(l.url.as_str()), Some(l.label.as_str())),
        None => (None, None),
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
             link_url = ?6,
             link_label = ?7,
             updated_at = ?8
         WHERE id = ?9;",
        params![
            &title,
            &description,
            status.as_str(),
            priority.as_str(),
            assignee_id.map(|id| uuid_to_sql(id.0)),
            link_url,
            link_label,
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
        link,
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
        link: None,
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
            deleted_at, trash_root_id, created_at, updated_at, link_url, link_label
         ) VALUES (?1, ?2, ?3, ?4, 'work_item', NULL, ?5, ?6, ?7, ?8, ?9, ?10, NULL, NULL, ?11, ?12, NULL, NULL);",
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
                    i.deleted_at, i.trash_root_id, i.created_at, i.updated_at, i.link_url, i.link_label
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
                    f.deleted_at, f.trash_root_id, f.created_at, f.updated_at, f.link_url, f.link_label
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
                    wi.deleted_at, wi.trash_root_id, wi.created_at, wi.updated_at, wi.link_url, wi.link_label
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

/// Checks `INV-08` Work Item → Feature conversion eligibility against live SQLite state.
pub fn check_work_item_to_feature_eligibility(
    conn: &Connection,
    work_item_id: ItemId,
) -> Result<ConversionEligibility, DomainError> {
    let item = get_item(conn, work_item_id)?;
    if !item.is_active() {
        return Err(DomainError::Validation(
            "Cannot convert an item in Trash".to_string(),
        ));
    }
    if item.kind != ItemKind::WorkItem {
        return Err(DomainError::ConversionBlocked(
            "Item is already a Feature".to_string(),
        ));
    }

    let attached = query_features_for_work_item(conn, work_item_id)?;
    match attached.len() {
        0 => Ok(ConversionEligibility::Allowed {
            target_kind: ItemKind::Feature,
            resulting_parent: None,
            moves_to_unallocated: false,
        }),
        1 => {
            let parent = &attached[0];
            if feature_depth(conn, parent.id)? + 1 >= MAX_FEATURE_DEPTH {
                Ok(ConversionEligibility::Blocked(vec![
                    ConversionBlockReason::ExceedsMaxFeatureDepth {
                        max_depth: MAX_FEATURE_DEPTH,
                    },
                ]))
            } else {
                Ok(ConversionEligibility::Allowed {
                    target_kind: ItemKind::Feature,
                    resulting_parent: Some(parent.id),
                    moves_to_unallocated: false,
                })
            }
        }
        n => Ok(ConversionEligibility::Blocked(vec![
            ConversionBlockReason::AttachedToMultipleFeatures(n),
        ])),
    }
}

/// Checks `INV-09` Feature → Work Item conversion eligibility against live SQLite state.
pub fn check_feature_to_work_item_eligibility(
    conn: &Connection,
    feature_id: ItemId,
) -> Result<ConversionEligibility, DomainError> {
    let item = get_item(conn, feature_id)?;
    if !item.is_active() {
        return Err(DomainError::Validation(
            "Cannot convert an item in Trash".to_string(),
        ));
    }
    if item.kind != ItemKind::Feature {
        return Err(DomainError::ConversionBlocked(
            "Item is already a Work Item".to_string(),
        ));
    }

    let fid_sql = uuid_to_sql(feature_id.0);
    let child_count_i64: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM items WHERE parent_feature_id = ?1 AND deleted_at IS NULL;",
            params![&fid_sql],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;
    let wi_count_i64: i64 = conn
        .query_row(
            "SELECT COUNT(*)
             FROM feature_work_items fwi
             JOIN items wi ON wi.id = fwi.work_item_id
             WHERE fwi.feature_id = ?1 AND wi.deleted_at IS NULL;",
            params![&fid_sql],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;
    let dep_count_i64: i64 = conn
        .query_row(
            "SELECT COUNT(*)
             FROM item_blockers ib
             JOIN items b ON b.id = ib.blocked_item_id
             WHERE ib.blocker_feature_id = ?1 AND b.deleted_at IS NULL;",
            params![&fid_sql],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;

    let mut reasons = Vec::new();
    let child_count = usize::try_from(child_count_i64).unwrap_or(0);
    if child_count > 0 {
        reasons.push(ConversionBlockReason::HasChildFeatures(child_count));
    }
    let wi_count = usize::try_from(wi_count_i64).unwrap_or(0);
    if wi_count > 0 {
        reasons.push(ConversionBlockReason::HasAttachedWorkItems(wi_count));
    }
    let dep_count = usize::try_from(dep_count_i64).unwrap_or(0);
    if dep_count > 0 {
        reasons.push(ConversionBlockReason::HasActiveDependents(dep_count));
    }

    if !reasons.is_empty() {
        return Ok(ConversionEligibility::Blocked(reasons));
    }

    let active_parent = match item.parent_feature_id {
        Some(pid) => {
            let parent = get_item(conn, pid)?;
            if parent.is_active() && parent.kind == ItemKind::Feature {
                Some(pid)
            } else {
                None
            }
        }
        None => None,
    };

    Ok(ConversionEligibility::Allowed {
        target_kind: ItemKind::WorkItem,
        resulting_parent: active_parent,
        moves_to_unallocated: active_parent.is_none(),
    })
}

/// Losslessly converts an eligible Work Item (`0` or `1` active Feature) into a Feature (`DATA_MODEL.md` §4.6.A / `INV-08`).
pub fn convert_work_item_to_feature(
    tx: &Transaction<'_>,
    work_item_id: ItemId,
) -> Result<Item, DomainError> {
    let item = get_item(tx, work_item_id)?;
    if !item.is_active() {
        return Err(DomainError::Validation(
            "Cannot convert an item in Trash".to_string(),
        ));
    }
    if item.kind != ItemKind::WorkItem {
        return Err(DomainError::ConversionBlocked(format!(
            "{} is already a Feature",
            item.item_key
        )));
    }

    let attached = query_features_for_work_item(tx, work_item_id)?;
    let parent_id = match attached.len() {
        0 => None,
        1 => {
            let parent = &attached[0];
            if feature_depth(tx, parent.id)? + 1 >= MAX_FEATURE_DEPTH {
                return Err(DomainError::ConversionBlocked(
                    format_conversion_block_message(
                        ItemKind::WorkItem,
                        &[ConversionBlockReason::ExceedsMaxFeatureDepth {
                            max_depth: MAX_FEATURE_DEPTH,
                        }],
                        &[],
                        &[],
                    ),
                ));
            }
            Some(parent.id)
        }
        n => {
            let names: Vec<String> = attached.iter().map(|f| f.title.clone()).collect();
            return Err(DomainError::ConversionBlocked(
                format_conversion_block_message(
                    ItemKind::WorkItem,
                    &[ConversionBlockReason::AttachedToMultipleFeatures(n)],
                    &names,
                    &[],
                ),
            ));
        }
    };

    let wi_sql = uuid_to_sql(work_item_id.0);
    // Clear all feature_work_items rows (active link + any stale links to trashed Features) before flipping kind.
    tx.execute(
        "DELETE FROM feature_work_items WHERE work_item_id = ?1;",
        params![&wi_sql],
    )
    .map_err(map_sqlite_err)?;

    let now = Utc::now();
    let now_sql = ts_to_sql(now);
    let new_sort_order =
        append_sort_order_for_parent(tx, item.project_id, parent_id, None, &now_sql)?;

    tx.execute(
        "UPDATE items
         SET kind = 'feature',
             parent_feature_id = ?1,
             sort_order = ?2,
             updated_at = ?3
         WHERE id = ?4;",
        params![
            parent_id.map(|id| uuid_to_sql(id.0)),
            &new_sort_order,
            &now_sql,
            &wi_sql,
        ],
    )
    .map_err(map_sqlite_err)?;

    Ok(Item {
        kind: ItemKind::Feature,
        parent_feature_id: parent_id,
        sort_order: new_sort_order,
        updated_at: now,
        ..item
    })
}

/// Losslessly converts an eligible empty leaf Feature into a Work Item (`DATA_MODEL.md` §4.6.B / `INV-09`).
pub fn convert_feature_to_work_item(
    tx: &Transaction<'_>,
    feature_id: ItemId,
) -> Result<(Item, Option<ItemId>), DomainError> {
    let item = get_item(tx, feature_id)?;
    if !item.is_active() {
        return Err(DomainError::Validation(
            "Cannot convert an item in Trash".to_string(),
        ));
    }
    if item.kind != ItemKind::Feature {
        return Err(DomainError::ConversionBlocked(format!(
            "{} is already a Work Item",
            item.item_key
        )));
    }

    let eligibility = check_feature_to_work_item_eligibility(tx, feature_id)?;
    if let ConversionEligibility::Blocked(reasons) = eligibility {
        let mut stmt = tx
            .prepare(
                "SELECT b.item_key
                 FROM item_blockers ib
                 JOIN items b ON b.id = ib.blocked_item_id
                 WHERE ib.blocker_feature_id = ?1
                   AND b.deleted_at IS NULL
                 ORDER BY b.item_number ASC;",
            )
            .map_err(map_sqlite_err)?;
        let dep_keys = stmt
            .query_map(params![uuid_to_sql(feature_id.0)], |row| row.get::<_, String>(0))
            .map_err(map_sqlite_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_err)?;

        return Err(DomainError::ConversionBlocked(
            format_conversion_block_message(ItemKind::Feature, &reasons, &[], &dep_keys),
        ));
    }

    let old_parent_id = match eligibility {
        ConversionEligibility::Allowed {
            resulting_parent, ..
        } => resulting_parent,
        ConversionEligibility::Blocked(_) => None,
    };

    let feat_sql = uuid_to_sql(feature_id.0);
    // Prune stale references from already-trashed rows before flipping kind so trg_items_kind_guard_upd passes.
    tx.execute(
        "UPDATE items
         SET parent_feature_id = NULL
         WHERE parent_feature_id = ?1 AND deleted_at IS NOT NULL;",
        params![&feat_sql],
    )
    .map_err(map_sqlite_err)?;
    tx.execute(
        "DELETE FROM feature_work_items WHERE feature_id = ?1;",
        params![&feat_sql],
    )
    .map_err(map_sqlite_err)?;
    tx.execute(
        "DELETE FROM item_blockers WHERE blocker_feature_id = ?1;",
        params![&feat_sql],
    )
    .map_err(map_sqlite_err)?;

    let now = Utc::now();
    let now_sql = ts_to_sql(now);
    let new_wi_sort = append_work_item_sort_order(tx, item.project_id, &now_sql)?;

    tx.execute(
        "UPDATE items
         SET kind = 'work_item',
             parent_feature_id = NULL,
             sort_order = ?1,
             updated_at = ?2
         WHERE id = ?3;",
        params![&new_wi_sort, &now_sql, &feat_sql],
    )
    .map_err(map_sqlite_err)?;

    if let Some(parent_id) = old_parent_id {
        let fwi_sort = append_fwi_sort_order(tx, item.project_id, parent_id)?;
        tx.execute(
            "INSERT INTO feature_work_items (project_id, feature_id, work_item_id, sort_order, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5);",
            params![
                uuid_to_sql(item.project_id.0),
                uuid_to_sql(parent_id.0),
                &feat_sql,
                &fwi_sort,
                &now_sql,
            ],
        )
        .map_err(map_sqlite_err)?;
    }

    let converted = Item {
        kind: ItemKind::WorkItem,
        parent_feature_id: None,
        sort_order: new_wi_sort,
        updated_at: now,
        ..item
    };

    Ok((converted, old_parent_id))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashSubtreeResult {
    pub project_id: ProjectId,
    pub root: Item,
    pub trashed_feature_ids: Vec<ItemId>,
    pub trashed_work_item_ids: Vec<ItemId>,
    pub spared_shared_work_item_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreTrashResult {
    pub project_id: ProjectId,
    pub root: Item,
    pub restored_count: usize,
    pub promoted_to_root: bool,
    pub pruned_blocker_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeTrashResult {
    pub project_id: ProjectId,
    pub root_key: String,
    pub root_title: String,
    pub purged_item_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyTrashResult {
    pub project_id: ProjectId,
    pub purged_group_count: usize,
    pub purged_item_count: usize,
}

/// Soft-deletes a Work Item or an entire Feature subtree while sparing shared active Work Items (`DATA_MODEL.md` §4.7 / `INV-12`).
pub fn trash_item_subtree(
    tx: &Transaction<'_>,
    target_id: ItemId,
) -> Result<TrashSubtreeResult, DomainError> {
    let target = get_item(tx, target_id)?;
    if !target.is_active() {
        return Err(DomainError::Validation(
            "Item is already in Trash".to_string(),
        ));
    }

    let target_sql = uuid_to_sql(target_id.0);

    let (trashed_feature_ids, trashed_work_item_ids, spared_shared_work_item_count) =
        if target.kind == ItemKind::WorkItem {
            (Vec::new(), vec![target.id], 0usize)
        } else {
            let feature_ids = {
                let mut stmt = tx
                    .prepare(
                        "WITH RECURSIVE target_subtree(id, depth) AS (
                            SELECT id, 0 FROM items WHERE id = ?1 AND deleted_at IS NULL
                            UNION ALL
                            SELECT c.id, ts.depth + 1
                            FROM items c
                            JOIN target_subtree ts ON c.parent_feature_id = ts.id
                            WHERE c.kind = 'feature'
                              AND c.deleted_at IS NULL
                              AND ts.depth < 32
                        )
                        SELECT ts.id
                        FROM target_subtree ts
                        JOIN items i ON i.id = ts.id
                        ORDER BY ts.depth ASC, i.item_number ASC;",
                    )
                    .map_err(map_sqlite_err)?;
                let rows = stmt
                    .query_map(params![&target_sql], |row| {
                        Ok(ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?))
                    })
                    .map_err(map_sqlite_err)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)?
            };

            let exclusive_wi_ids = {
                let mut stmt = tx
                    .prepare(
                        "WITH RECURSIVE target_subtree(id, depth) AS (
                            SELECT id, 0 FROM items WHERE id = ?1 AND deleted_at IS NULL
                            UNION ALL
                            SELECT c.id, ts.depth + 1
                            FROM items c
                            JOIN target_subtree ts ON c.parent_feature_id = ts.id
                            WHERE c.kind = 'feature'
                              AND c.deleted_at IS NULL
                              AND ts.depth < 32
                        )
                        SELECT DISTINCT wi.id
                        FROM feature_work_items fwi
                        JOIN items wi ON wi.id = fwi.work_item_id
                        WHERE fwi.feature_id IN (SELECT id FROM target_subtree)
                          AND wi.deleted_at IS NULL
                          AND NOT EXISTS (
                              SELECT 1
                              FROM feature_work_items other_fwi
                              JOIN items other_f ON other_f.id = other_fwi.feature_id
                              WHERE other_fwi.work_item_id = wi.id
                                AND other_f.deleted_at IS NULL
                                AND other_f.id NOT IN (SELECT id FROM target_subtree)
                          )
                        ORDER BY wi.item_number ASC;",
                    )
                    .map_err(map_sqlite_err)?;
                let rows = stmt
                    .query_map(params![&target_sql], |row| {
                        Ok(ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?))
                    })
                    .map_err(map_sqlite_err)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)?
            };

            let spared_i64: i64 = tx
                .query_row(
                    "WITH RECURSIVE target_subtree(id, depth) AS (
                        SELECT id, 0 FROM items WHERE id = ?1 AND deleted_at IS NULL
                        UNION ALL
                        SELECT c.id, ts.depth + 1
                        FROM items c
                        JOIN target_subtree ts ON c.parent_feature_id = ts.id
                        WHERE c.kind = 'feature'
                          AND c.deleted_at IS NULL
                          AND ts.depth < 32
                    )
                    SELECT COUNT(DISTINCT wi.id)
                    FROM feature_work_items fwi
                    JOIN items wi ON wi.id = fwi.work_item_id
                    WHERE fwi.feature_id IN (SELECT id FROM target_subtree)
                      AND wi.deleted_at IS NULL
                      AND EXISTS (
                          SELECT 1
                          FROM feature_work_items other_fwi
                          JOIN items other_f ON other_f.id = other_fwi.feature_id
                          WHERE other_fwi.work_item_id = wi.id
                            AND other_f.deleted_at IS NULL
                            AND other_f.id NOT IN (SELECT id FROM target_subtree)
                      );",
                    params![&target_sql],
                    |row| row.get(0),
                )
                .map_err(map_sqlite_err)?;
            let spared = usize::try_from(spared_i64).unwrap_or(0);

            (feature_ids, exclusive_wi_ids, spared)
        };

    let now = Utc::now();
    let now_sql = ts_to_sql(now);

    tx.execute(
        "WITH RECURSIVE target_subtree(id, depth) AS (
            SELECT id, 0 FROM items WHERE id = ?1 AND deleted_at IS NULL
            UNION ALL
            SELECT c.id, ts.depth + 1
            FROM items c
            JOIN target_subtree ts ON c.parent_feature_id = ts.id
            WHERE c.kind = 'feature'
              AND c.deleted_at IS NULL
              AND ts.depth < 32
        ),
        exclusively_owned_wis AS (
            SELECT DISTINCT fwi.work_item_id AS id
            FROM feature_work_items fwi
            JOIN items wi ON wi.id = fwi.work_item_id
            WHERE fwi.feature_id IN (SELECT id FROM target_subtree)
              AND wi.deleted_at IS NULL
              AND NOT EXISTS (
                  SELECT 1
                  FROM feature_work_items other_fwi
                  JOIN items other_f ON other_f.id = other_fwi.feature_id
                  WHERE other_fwi.work_item_id = wi.id
                    AND other_f.deleted_at IS NULL
                    AND other_f.id NOT IN (SELECT id FROM target_subtree)
              )
        )
        UPDATE items
        SET deleted_at = ?2,
            trash_root_id = ?1,
            updated_at = ?2
        WHERE id IN (
            SELECT id FROM target_subtree
            UNION
            SELECT id FROM exclusively_owned_wis
        );",
        params![&target_sql, &now_sql],
    )
    .map_err(map_sqlite_err)?;

    let root = Item {
        deleted_at: Some(now),
        trash_root_id: Some(target.id),
        updated_at: now,
        ..target
    };

    Ok(TrashSubtreeResult {
        project_id: root.project_id,
        root,
        trashed_feature_ids,
        trashed_work_item_ids,
        spared_shared_work_item_count,
    })
}

/// Lists all grouped Trash roots for `project_id`, ordered newest deleted first.
pub fn list_trash_groups(
    conn: &Connection,
    project_id: ProjectId,
) -> Result<Vec<TrashGroup>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT r.id, r.project_id, r.item_number, r.item_key, r.kind, r.parent_feature_id,
                    r.title, r.description, r.status, r.priority, r.assignee_id, r.sort_order,
                    r.deleted_at, r.trash_root_id, r.created_at, r.updated_at, r.link_url, r.link_label,
                    COUNT(m.id) AS total_count,
                    SUM(CASE WHEN m.id != r.id AND m.kind = 'feature' THEN 1 ELSE 0 END) AS desc_feat_count,
                    SUM(CASE WHEN m.id != r.id AND m.kind = 'work_item' THEN 1 ELSE 0 END) AS desc_wi_count
             FROM items r
             JOIN items m
               ON m.project_id = r.project_id
              AND m.trash_root_id = r.id
              AND m.deleted_at IS NOT NULL
             WHERE r.project_id = ?1
               AND r.deleted_at IS NOT NULL
               AND r.trash_root_id = r.id
             GROUP BY r.id
             ORDER BY r.deleted_at DESC, r.item_number DESC;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(params![uuid_to_sql(project_id.0)], |row| {
            let root = row_to_item(row)?;
            let total_i64: i64 = row.get(18)?;
            let feat_i64: i64 = row.get(19)?;
            let wi_i64: i64 = row.get(20)?;
            let deleted_at = root.deleted_at.unwrap_or(root.updated_at);
            Ok(TrashGroup {
                root,
                deleted_at,
                descendant_feature_count: usize::try_from(feat_i64).unwrap_or(0),
                descendant_work_item_count: usize::try_from(wi_i64).unwrap_or(0),
                total_item_count: usize::try_from(total_i64).unwrap_or(1),
            })
        })
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

fn self_and_ancestors(
    start: ItemId,
    parent_of: &HashMap<ItemId, Option<ItemId>>,
) -> HashSet<ItemId> {
    let mut out = HashSet::new();
    let mut curr = Some(start);
    while let Some(id) = curr {
        if !out.insert(id) || out.len() > MAX_FEATURE_DEPTH + 1 {
            break;
        }
        curr = parent_of.get(&id).copied().flatten();
    }
    out
}

fn is_reachable_in_adj(start: ItemId, target: ItemId, adj: &HashMap<ItemId, Vec<ItemId>>) -> bool {
    let mut visited = HashSet::new();
    let mut stack = vec![start];
    while let Some(node) = stack.pop() {
        if node == target {
            return true;
        }
        if !visited.insert(node) {
            continue;
        }
        if let Some(nexts) = adj.get(&node) {
            for &next in nexts {
                if !visited.contains(&next) {
                    stack.push(next);
                }
            }
        }
    }
    false
}

/// Atomically restores an entire Trash group (`trash_root_id`), promoting root if its parent was
/// purged/trashed/converted and pruning any blocker edges that would violate `INV-05`.
pub fn restore_trash_group(
    tx: &Transaction<'_>,
    trash_root_id: ItemId,
) -> Result<RestoreTrashResult, DomainError> {
    let root = get_item(tx, trash_root_id)?;
    if root.is_active() || root.trash_root_id != Some(trash_root_id) {
        return Err(DomainError::Validation(
            "Selected item is not a Trash group root".to_string(),
        ));
    }

    let project_sql = uuid_to_sql(root.project_id.0);
    let root_sql = uuid_to_sql(trash_root_id.0);

    let members: Vec<Item> = {
        let mut stmt = tx
            .prepare(
                "SELECT id, project_id, item_number, item_key, kind, parent_feature_id,
                        title, description, status, priority, assignee_id, sort_order,
                        deleted_at, trash_root_id, created_at, updated_at, link_url, link_label
                 FROM items
                 WHERE project_id = ?1
                   AND trash_root_id = ?2
                   AND deleted_at IS NOT NULL
                 ORDER BY item_number ASC;",
            )
            .map_err(map_sqlite_err)?;
        let rows = stmt
            .query_map(params![&project_sql, &root_sql], row_to_item)
            .map_err(map_sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)?
    };

    if members.is_empty() {
        return Err(DomainError::ItemNotFound(trash_root_id));
    }

    let member_ids: HashSet<ItemId> = members.iter().map(|m| m.id).collect();

    let mut promoted_to_root = false;
    if root.kind == ItemKind::Feature
        && let Some(parent_id) = root.parent_feature_id
    {
        let parent_valid = get_item(tx, parent_id)
            .ok()
            .is_some_and(|p| p.project_id == root.project_id && p.is_active() && p.kind == ItemKind::Feature);
        if !parent_valid {
            promoted_to_root = true;
        } else {
            let mut children_by_parent: HashMap<ItemId, Vec<ItemId>> = HashMap::new();
            for m in &members {
                if m.kind == ItemKind::Feature
                    && m.id != root.id
                    && let Some(pid) = m.parent_feature_id
                {
                    children_by_parent.entry(pid).or_default().push(m.id);
                }
            }
            let mut max_offset = 0usize;
            let mut visited = HashSet::from([root.id]);
            let mut stack = vec![(root.id, 0usize)];
            while let Some((fid, offset)) = stack.pop() {
                max_offset = max_offset.max(offset);
                if let Some(children) = children_by_parent.get(&fid) {
                    for &cid in children {
                        if visited.insert(cid) {
                            stack.push((cid, offset + 1));
                        }
                    }
                }
            }
            let parent_depth = feature_depth(tx, parent_id)?;
            if parent_depth + 1 + max_offset >= MAX_FEATURE_DEPTH {
                promoted_to_root = true;
            }
        }
    }

    let mut orphaned_member_ids = Vec::new();
    for m in &members {
        if m.id != root.id
            && m.kind == ItemKind::Feature
            && let Some(pid) = m.parent_feature_id
        {
            let valid_in_group = member_ids.contains(&pid);
            let valid_active = !valid_in_group
                && get_item(tx, pid).ok().is_some_and(|p| {
                    p.project_id == root.project_id && p.is_active() && p.kind == ItemKind::Feature
                });
            if !valid_in_group && !valid_active {
                orphaned_member_ids.push(m.id);
            }
        }
    }

    let now = Utc::now();
    let now_sql = ts_to_sql(now);

    tx.execute(
        "UPDATE items
         SET deleted_at = NULL,
             trash_root_id = NULL,
             updated_at = ?1
         WHERE project_id = ?2
           AND trash_root_id = ?3
           AND deleted_at IS NOT NULL;",
        params![&now_sql, &project_sql, &root_sql],
    )
    .map_err(map_sqlite_err)?;

    if promoted_to_root {
        let new_sort_order =
            append_sort_order_for_parent(tx, root.project_id, None, Some(root.id), &now_sql)?;
        tx.execute(
            "UPDATE items
             SET parent_feature_id = NULL,
                 sort_order = ?1,
                 updated_at = ?2
             WHERE id = ?3;",
            params![&new_sort_order, &now_sql, &root_sql],
        )
        .map_err(map_sqlite_err)?;
    }

    for orphan_id in orphaned_member_ids {
        let new_sort_order =
            append_sort_order_for_parent(tx, root.project_id, None, Some(orphan_id), &now_sql)?;
        tx.execute(
            "UPDATE items
             SET parent_feature_id = NULL,
                 sort_order = ?1,
                 updated_at = ?2
             WHERE id = ?3;",
            params![&new_sort_order, &now_sql, uuid_to_sql(orphan_id.0)],
        )
        .map_err(map_sqlite_err)?;
    }

    // Re-validate INV-05 DAG acyclicity & ancestor/descendant exclusion for restored edges.
    let mut parent_of: HashMap<ItemId, Option<ItemId>> = HashMap::new();
    let mut adj: HashMap<ItemId, Vec<ItemId>> = HashMap::new();
    {
        let mut stmt = tx
            .prepare(
                "SELECT id, parent_feature_id
                 FROM items
                 WHERE project_id = ?1
                   AND kind = 'feature'
                   AND deleted_at IS NULL;",
            )
            .map_err(map_sqlite_err)?;
        let rows = stmt
            .query_map(params![&project_sql], |row| {
                let id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
                let parent_opt: Option<String> = row.get(1)?;
                let parent_id = parent_opt
                    .map(|raw| parse_uuid_col(1, raw).map(ItemId::from_uuid))
                    .transpose()?;
                Ok((id, parent_id))
            })
            .map_err(map_sqlite_err)?;
        for pair in rows {
            let (id, pid) = pair.map_err(map_sqlite_err)?;
            parent_of.insert(id, pid);
            if let Some(parent_id) = pid {
                adj.entry(id).or_default().push(parent_id);
            }
        }
    }

    #[derive(Clone, Copy)]
    struct ActiveBlockerEdge {
        blocked_id: ItemId,
        blocked_kind: ItemKind,
        blocked_num: i64,
        blocker_id: ItemId,
        blocker_num: i64,
    }

    let active_edges: Vec<ActiveBlockerEdge> = {
        let mut stmt = tx
            .prepare(
                "SELECT ib.blocked_item_id, b.kind, b.item_number, ib.blocker_feature_id, r.item_number
                 FROM item_blockers ib
                 JOIN items b ON b.id = ib.blocked_item_id
                 JOIN items r ON r.id = ib.blocker_feature_id
                 WHERE ib.project_id = ?1
                   AND b.deleted_at IS NULL
                   AND r.deleted_at IS NULL
                 ORDER BY b.item_number ASC, r.item_number ASC;",
            )
            .map_err(map_sqlite_err)?;
        let rows = stmt
            .query_map(params![&project_sql], |row| {
                let blocked_id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
                let kind_raw: String = row.get(1)?;
                let blocked_kind = parse_domain_col(1, ItemKind::from_db_str(&kind_raw))?;
                let blocked_num: i64 = row.get(2)?;
                let blocker_id = ItemId::from_uuid(parse_uuid_col(3, row.get(3)?)?);
                let blocker_num: i64 = row.get(4)?;
                Ok(ActiveBlockerEdge {
                    blocked_id,
                    blocked_kind,
                    blocked_num,
                    blocker_id,
                    blocker_num,
                })
            })
            .map_err(map_sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)?
    };

    let mut feature_candidates = Vec::new();
    let mut wi_edges = Vec::new();

    for edge in active_edges {
        if edge.blocked_kind == ItemKind::Feature {
            if member_ids.contains(&edge.blocked_id) || member_ids.contains(&edge.blocker_id) {
                feature_candidates.push(edge);
            } else {
                adj.entry(edge.blocked_id).or_default().push(edge.blocker_id);
            }
        } else {
            wi_edges.push(edge);
        }
    }

    feature_candidates.sort_by_key(|e| (e.blocked_num, e.blocker_num));
    let mut pruned_blocker_count = 0usize;

    for edge in feature_candidates {
        let b = edge.blocked_id;
        let r = edge.blocker_id;
        let b_ancestors = self_and_ancestors(b, &parent_of);
        let r_ancestors = self_and_ancestors(r, &parent_of);
        let invalid_exclusion = b == r || b_ancestors.contains(&r) || r_ancestors.contains(&b);
        let creates_cycle = !invalid_exclusion && is_reachable_in_adj(r, b, &adj);

        if invalid_exclusion || creates_cycle {
            tx.execute(
                "DELETE FROM item_blockers
                 WHERE blocked_item_id = ?1 AND blocker_feature_id = ?2;",
                params![uuid_to_sql(b.0), uuid_to_sql(r.0)],
            )
            .map_err(map_sqlite_err)?;
            pruned_blocker_count += 1;
        } else {
            adj.entry(b).or_default().push(r);
        }
    }

    if !wi_edges.is_empty() {
        let mut wi_features: HashMap<ItemId, Vec<ItemId>> = HashMap::new();
        {
            let mut stmt = tx
                .prepare(
                    "SELECT fwi.work_item_id, fwi.feature_id
                     FROM feature_work_items fwi
                     JOIN items f ON f.id = fwi.feature_id
                     JOIN items wi ON wi.id = fwi.work_item_id
                     WHERE fwi.project_id = ?1
                       AND f.deleted_at IS NULL
                       AND wi.deleted_at IS NULL;",
                )
                .map_err(map_sqlite_err)?;
            let rows = stmt
                .query_map(params![&project_sql], |row| {
                    let wi_id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
                    let fid = ItemId::from_uuid(parse_uuid_col(1, row.get(1)?)?);
                    Ok((wi_id, fid))
                })
                .map_err(map_sqlite_err)?;
            for pair in rows {
                let (wi_id, fid) = pair.map_err(map_sqlite_err)?;
                wi_features.entry(wi_id).or_default().push(fid);
            }
        }

        for edge in wi_edges {
            if let Some(attached) = wi_features.get(&edge.blocked_id) {
                let mut forbidden = HashSet::new();
                for &fid in attached {
                    forbidden.extend(self_and_ancestors(fid, &parent_of));
                }
                if forbidden.contains(&edge.blocker_id) {
                    tx.execute(
                        "DELETE FROM item_blockers
                         WHERE blocked_item_id = ?1 AND blocker_feature_id = ?2;",
                        params![
                            uuid_to_sql(edge.blocked_id.0),
                            uuid_to_sql(edge.blocker_id.0)
                        ],
                    )
                    .map_err(map_sqlite_err)?;
                    pruned_blocker_count += 1;
                }
            }
        }
    }

    let restored_root = get_item(tx, trash_root_id)?;
    Ok(RestoreTrashResult {
        project_id: restored_root.project_id,
        root: restored_root,
        restored_count: members.len(),
        promoted_to_root,
        pruned_blocker_count,
    })
}

/// Permanently deletes a Trash group (`trash_root_id`), cascading only to items inside that Trash
/// group and join rows while keeping shared active Work Items intact (`INV-12`).
pub fn purge_trash_group(
    tx: &Transaction<'_>,
    trash_root_id: ItemId,
) -> Result<PurgeTrashResult, DomainError> {
    let root = get_item(tx, trash_root_id)?;
    if root.is_active() || root.trash_root_id != Some(trash_root_id) {
        return Err(DomainError::Validation(
            "Selected item is not a Trash group root".to_string(),
        ));
    }

    let project_sql = uuid_to_sql(root.project_id.0);
    let root_sql = uuid_to_sql(trash_root_id.0);

    let count_i64: i64 = tx
        .query_row(
            "SELECT COUNT(*)
             FROM items
             WHERE project_id = ?1
               AND trash_root_id = ?2
               AND deleted_at IS NOT NULL;",
            params![&project_sql, &root_sql],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;
    let purged_item_count = usize::try_from(count_i64).unwrap_or(1);

    // Detach any separately trashed child groups so purging this group does not cascade-delete them.
    tx.execute(
        "UPDATE items
         SET parent_feature_id = NULL
         WHERE project_id = ?1
           AND deleted_at IS NOT NULL
           AND trash_root_id != ?2
           AND parent_feature_id IN (
               SELECT id FROM items WHERE project_id = ?1 AND trash_root_id = ?2
           );",
        params![&project_sql, &root_sql],
    )
    .map_err(map_sqlite_err)?;

    tx.execute("DELETE FROM items WHERE id = ?1;", params![&root_sql])
        .map_err(map_sqlite_err)?;

    Ok(PurgeTrashResult {
        project_id: root.project_id,
        root_key: root.item_key,
        root_title: root.title,
        purged_item_count,
    })
}

/// Permanently deletes all trashed items in `project_id` (`INV-01`, `INV-12`).
pub fn empty_project_trash(
    tx: &Transaction<'_>,
    project_id: ProjectId,
) -> Result<EmptyTrashResult, DomainError> {
    let _project = get_project(tx, project_id)?;
    let project_sql = uuid_to_sql(project_id.0);

    let (group_i64, item_i64): (i64, i64) = tx
        .query_row(
            "SELECT COUNT(DISTINCT trash_root_id), COUNT(*)
             FROM items
             WHERE project_id = ?1 AND deleted_at IS NOT NULL;",
            params![&project_sql],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(map_sqlite_err)?;

    let purged_group_count = usize::try_from(group_i64).unwrap_or(0);
    let purged_item_count = usize::try_from(item_i64).unwrap_or(0);

    if purged_item_count > 0 {
        tx.execute(
            "DELETE FROM items WHERE project_id = ?1 AND deleted_at IS NOT NULL;",
            params![&project_sql],
        )
        .map_err(map_sqlite_err)?;
    }

    Ok(EmptyTrashResult {
        project_id,
        purged_group_count,
        purged_item_count,
    })
}

fn resolve_reorder_bounds<'a>(
    ordered: &'a [(ItemId, String)],
    moving_id: ItemId,
    before_id: Option<ItemId>,
    after_id: Option<ItemId>,
    kind_label: &str,
) -> Result<(usize, Option<&'a str>, Option<&'a str>), DomainError> {
    let before_idx = match before_id {
        Some(bid) => {
            if bid == moving_id {
                return Err(DomainError::Validation(format!(
                    "before_id cannot equal {kind_label} id"
                )));
            }
            let pos = ordered.iter().position(|(id, _)| *id == bid).ok_or_else(|| {
                DomainError::Validation(format!("before_id is not a valid {kind_label} sibling"))
            })?;
            Some(pos)
        }
        None => None,
    };

    let after_idx = match after_id {
        Some(aid) => {
            if aid == moving_id {
                return Err(DomainError::Validation(format!(
                    "after_id cannot equal {kind_label} id"
                )));
            }
            let pos = ordered.iter().position(|(id, _)| *id == aid).ok_or_else(|| {
                DomainError::Validation(format!("after_id is not a valid {kind_label} sibling"))
            })?;
            Some(pos)
        }
        None => None,
    };

    if let (Some(b), Some(a)) = (before_idx, after_idx)
        && b >= a
    {
        return Err(DomainError::Validation(format!(
            "before_id must precede after_id among {kind_label} siblings"
        )));
    }

    let bounds = match (before_idx, after_idx) {
        (Some(b), Some(_a)) => (
            b + 1,
            Some(ordered[b].1.as_str()),
            ordered.get(b + 1).map(|(_, k)| k.as_str()),
        ),
        (Some(b), None) => (
            b + 1,
            Some(ordered[b].1.as_str()),
            ordered.get(b + 1).map(|(_, k)| k.as_str()),
        ),
        (None, Some(a)) => (
            a,
            a.checked_sub(1).map(|i| ordered[i].1.as_str()),
            Some(ordered[a].1.as_str()),
        ),
        (None, None) => (
            ordered.len(),
            ordered.last().map(|(_, k)| k.as_str()),
            None,
        ),
    };

    Ok(bounds)
}

/// Reorders `work_item_id` inside `feature_id`'s `feature_work_items` list using fractional indexing.
pub fn reorder_feature_work_items(
    tx: &Transaction<'_>,
    project_id: ProjectId,
    feature_id: ItemId,
    work_item_id: ItemId,
    before_work_item_id: Option<ItemId>,
    after_work_item_id: Option<ItemId>,
) -> Result<FeatureWorkItem, DomainError> {
    let _feature = validate_active_feature_in_project(tx, project_id, feature_id)?;
    let work_item = validate_active_work_item(tx, work_item_id)?;
    if work_item.project_id != project_id {
        return Err(DomainError::CrossProjectReference);
    }

    let feat_sql = uuid_to_sql(feature_id.0);
    let wi_sql = uuid_to_sql(work_item_id.0);

    let existing_link = tx
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
        .ok_or_else(|| {
            DomainError::Validation("Work Item is not attached to the target Feature".to_string())
        })?;

    let other_links: Vec<(ItemId, String)> = {
        let mut stmt = tx
            .prepare(
                "SELECT fwi.work_item_id, fwi.sort_order
                 FROM feature_work_items fwi
                 JOIN items wi ON wi.id = fwi.work_item_id
                 WHERE fwi.feature_id = ?1
                   AND wi.deleted_at IS NULL
                   AND wi.kind = 'work_item'
                   AND fwi.work_item_id != ?2
                 ORDER BY fwi.sort_order ASC, wi.item_number ASC;",
            )
            .map_err(map_sqlite_err)?;
        let rows = stmt
            .query_map(params![&feat_sql, &wi_sql], |row| {
                let id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
                let sort_order: String = row.get(1)?;
                Ok((id, sort_order))
            })
            .map_err(map_sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)?
    };

    let (insert_idx, lower_key, upper_key) = resolve_reorder_bounds(
        &other_links,
        work_item_id,
        before_work_item_id,
        after_work_item_id,
        "feature work item",
    )?;

    let now = Utc::now();
    let now_sql = ts_to_sql(now);

    let sort_order = match fractional_index::midpoint(lower_key, upper_key) {
        Ok(key) if !fractional_index::needs_rebalance(&key) => {
            tx.execute(
                "UPDATE feature_work_items
                 SET sort_order = ?1
                 WHERE feature_id = ?2 AND work_item_id = ?3;",
                params![&key, &feat_sql, &wi_sql],
            )
            .map_err(map_sqlite_err)?;
            key
        }
        _ => {
            let mut ordered_ids: Vec<ItemId> = other_links.iter().map(|(id, _)| *id).collect();
            ordered_ids.insert(insert_idx, work_item_id);
            let keys = fractional_index::rebalance_keys(ordered_ids.len());
            for (id, key) in ordered_ids.iter().zip(keys.iter()) {
                tx.execute(
                    "UPDATE feature_work_items
                     SET sort_order = ?1
                     WHERE feature_id = ?2 AND work_item_id = ?3;",
                    params![key, &feat_sql, uuid_to_sql(id.0)],
                )
                .map_err(map_sqlite_err)?;
            }
            keys[insert_idx].clone()
        }
    };

    tx.execute(
        "UPDATE items SET updated_at = ?1 WHERE id = ?2;",
        params![&now_sql, &wi_sql],
    )
    .map_err(map_sqlite_err)?;

    Ok(FeatureWorkItem {
        sort_order,
        ..existing_link
    })
}

/// Reorders an unallocated Work Item within the Project's Unallocated bucket (`items.sort_order`).
pub fn reorder_unallocated_work_items(
    tx: &Transaction<'_>,
    project_id: ProjectId,
    work_item_id: ItemId,
    before_work_item_id: Option<ItemId>,
    after_work_item_id: Option<ItemId>,
) -> Result<Item, DomainError> {
    let work_item = validate_active_work_item(tx, work_item_id)?;
    if work_item.project_id != project_id {
        return Err(DomainError::CrossProjectReference);
    }
    if !query_features_for_work_item(tx, work_item_id)?.is_empty() {
        return Err(DomainError::Validation(
            "Only Unallocated Work Items can be reordered in Unallocated View".to_string(),
        ));
    }

    let project_sql = uuid_to_sql(project_id.0);
    let wi_sql = uuid_to_sql(work_item_id.0);

    let other_unallocated: Vec<(ItemId, String)> = {
        let mut stmt = tx
            .prepare(
                "SELECT i.id, i.sort_order
                 FROM items i
                 WHERE i.project_id = ?1
                   AND i.kind = 'work_item'
                   AND i.deleted_at IS NULL
                   AND i.id != ?2
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
            .query_map(params![&project_sql, &wi_sql], |row| {
                let id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
                let sort_order: String = row.get(1)?;
                Ok((id, sort_order))
            })
            .map_err(map_sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)?
    };

    let (insert_idx, lower_key, upper_key) = resolve_reorder_bounds(
        &other_unallocated,
        work_item_id,
        before_work_item_id,
        after_work_item_id,
        "unallocated work item",
    )?;

    let now = Utc::now();
    let now_sql = ts_to_sql(now);

    let sort_order = match fractional_index::midpoint(lower_key, upper_key) {
        Ok(key) if !fractional_index::needs_rebalance(&key) => {
            tx.execute(
                "UPDATE items SET sort_order = ?1, updated_at = ?2 WHERE id = ?3;",
                params![&key, &now_sql, &wi_sql],
            )
            .map_err(map_sqlite_err)?;
            key
        }
        _ => {
            let mut ordered_ids: Vec<ItemId> =
                other_unallocated.iter().map(|(id, _)| *id).collect();
            ordered_ids.insert(insert_idx, work_item_id);
            let keys = fractional_index::rebalance_keys(ordered_ids.len());
            for (id, key) in ordered_ids.iter().zip(keys.iter()) {
                tx.execute(
                    "UPDATE items SET sort_order = ?1, updated_at = ?2 WHERE id = ?3;",
                    params![key, &now_sql, uuid_to_sql(id.0)],
                )
                .map_err(map_sqlite_err)?;
            }
            keys[insert_idx].clone()
        }
    };

    Ok(Item {
        sort_order,
        updated_at: now,
        ..work_item
    })
}

/// Atomically moves a Work Item from `from_feature_id` to `to_feature_id`.
/// Attaches to `to_feature_id` first so `INV-05` blocker violations abort before detaching `from_feature_id`.
pub fn move_work_item_between_features(
    tx: &Transaction<'_>,
    work_item_id: ItemId,
    from_feature_id: ItemId,
    to_feature_id: ItemId,
) -> Result<FeatureWorkItem, DomainError> {
    let work_item = validate_active_work_item(tx, work_item_id)?;
    let _from = validate_active_feature_in_project(tx, work_item.project_id, from_feature_id)?;
    let _to = validate_active_feature_in_project(tx, work_item.project_id, to_feature_id)?;

    let link = attach_work_item_to_feature(tx, work_item_id, to_feature_id)?;
    if from_feature_id != to_feature_id {
        let _ = detach_work_item_from_feature(tx, work_item_id, from_feature_id)?;
    }
    Ok(link)
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResultItem {
    pub id: ItemId,
    pub project_id: ProjectId,
    pub project_name: String,
    pub project_key_prefix: String,
    pub project_color: String,
    pub item_key: String,
    pub kind: ItemKind,
    pub title: String,
    pub status: ItemStatus,
    pub priority: Priority,
    pub link: Option<ItemLink>,
    pub rank: f64,
}

/// Sanitizes arbitrary user text into a safe FTS5 prefix-search query (`"tok1"* "tok2"*`).
pub fn build_fts_query(raw: &str) -> Option<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in raw.chars() {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    if tokens.is_empty() {
        return None;
    }

    Some(
        tokens
            .into_iter()
            .map(|tok| format!("\"{tok}\"*"))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Executes FTS5 + exact/prefix `item_key` search across `items_fts` (`DATA_MODEL.md` §4.8).
pub fn search_items_fts(
    conn: &Connection,
    raw_query: &str,
    project_id: Option<ProjectId>,
    limit: usize,
) -> Result<Vec<SearchResultItem>, DomainError> {
    let trimmed = raw_query.trim();
    let Some(fts_query) = build_fts_query(trimmed) else {
        return Ok(Vec::new());
    };

    let project_sql = project_id.map(|pid| uuid_to_sql(pid.0));
    let limit_i64 = i64::try_from(limit.clamp(1, 100)).unwrap_or(50);

    let mut stmt = conn
        .prepare(
            "SELECT
                i.id,
                i.project_id,
                p.name,
                p.key_prefix,
                p.color,
                i.item_key,
                i.kind,
                i.title,
                i.status,
                i.priority,
                i.link_url,
                i.link_label,
                bm25(items_fts, 10.0, 5.0, 1.0, 4.0) AS rank
             FROM items_fts
             JOIN items i ON i.rowid = items_fts.rowid
             JOIN projects p ON p.id = i.project_id
             WHERE items_fts MATCH ?1
               AND (?3 IS NULL OR i.project_id = ?3)
               AND i.deleted_at IS NULL
             ORDER BY
                 CASE
                     WHEN upper(i.item_key) = upper(?2) THEN 0
                     WHEN instr(upper(i.item_key), upper(?2)) = 1 THEN 1
                     ELSE 2
                 END ASC,
                 rank ASC,
                 i.item_number ASC
             LIMIT ?4;",
        )
        .map_err(map_sqlite_err)?;

    let rows = stmt
        .query_map(params![fts_query, trimmed, project_sql, limit_i64], |row| {
            let kind_raw: String = row.get(6)?;
            let kind = parse_domain_col(6, ItemKind::from_db_str(&kind_raw))?;
            let status_raw: String = row.get(8)?;
            let status = parse_domain_col(8, ItemStatus::from_db_str(&status_raw))?;
            let priority_raw: String = row.get(9)?;
            let priority = parse_domain_col(9, Priority::from_db_str(&priority_raw))?;
            let link_url_opt: Option<String> = row.get(10)?;
            let link_label_opt: Option<String> = row.get(11)?;
            let link = link_url_opt.and_then(|raw_url| {
                let trimmed_url = raw_url.trim();
                if trimmed_url.is_empty() {
                    return None;
                }
                let label = link_label_opt
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or_else(|| suggest_link_label(trimmed_url))
                    .unwrap_or_else(|| "Link".to_string());
                Some(ItemLink {
                    url: trimmed_url.to_string(),
                    label,
                })
            });

            Ok(SearchResultItem {
                id: ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                project_id: ProjectId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                project_name: row.get(2)?,
                project_key_prefix: row.get(3)?,
                project_color: row.get(4)?,
                item_key: row.get(5)?,
                kind,
                title: row.get(7)?,
                status,
                priority,
                link,
                rank: row.get(12)?,
            })
        })
        .map_err(map_sqlite_err)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}
