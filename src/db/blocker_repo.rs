use std::collections::{HashMap, HashSet};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use crate::db::item_repo::get_item;
use crate::db::{map_sqlite_err, parse_domain_col, parse_ts_col, parse_uuid_col, ts_to_sql, uuid_to_sql};
use crate::domain::{
    BlockerRef, BlockerWarning, DomainError, EffectiveBlockState, Item, ItemBlocker, ItemId,
    ItemKind, ItemStatus, ProjectId,
};

#[derive(Debug, Clone, Default)]
pub struct ProjectBlockSnapshot {
    pub item_blockers: Vec<ItemBlocker>,
    pub direct_blockers_by_item: HashMap<ItemId, Vec<BlockerRef>>,
    pub inherited_blockers_by_item: HashMap<ItemId, Vec<BlockerRef>>,
    pub effective_block_state: HashMap<ItemId, EffectiveBlockState>,
    pub blocker_warnings: HashMap<ItemId, Vec<BlockerWarning>>,
}

/// Validates `INV-01`, `INV-04`, and `INV-05` before inserting `(blocked_item_id, blocker_feature_id)`.
pub fn validate_add_blocker(
    conn: &Connection,
    blocked_item_id: ItemId,
    blocker_feature_id: ItemId,
) -> Result<(Item, Item), DomainError> {
    let blocked = get_item(conn, blocked_item_id)?;
    if !blocked.is_active() {
        return Err(DomainError::Validation(
            "Cannot add a blocker to an item in Trash".to_string(),
        ));
    }

    let blocker = get_item(conn, blocker_feature_id)?;
    if blocked.project_id != blocker.project_id {
        return Err(DomainError::CrossProjectReference);
    }
    if blocker.kind != ItemKind::Feature {
        return Err(DomainError::Validation(
            "Only Features can be blockers (INV-04)".to_string(),
        ));
    }
    if !blocker.is_active() {
        return Err(DomainError::Validation(
            "Cannot add a trashed Feature as a blocker".to_string(),
        ));
    }
    if blocked_item_id == blocker_feature_id {
        return Err(DomainError::DependencyCycle(
            "An item cannot be blocked on itself".to_string(),
        ));
    }

    let project_sql = uuid_to_sql(blocked.project_id.0);
    let blocked_sql = uuid_to_sql(blocked_item_id.0);
    let blocker_sql = uuid_to_sql(blocker_feature_id.0);

    if blocked.kind == ItemKind::WorkItem {
        let violates_exclusion: i64 = conn
            .query_row(
                "WITH RECURSIVE attached_ancestors(feature_id) AS (
                    SELECT fwi.feature_id
                    FROM feature_work_items fwi
                    JOIN items f ON f.id = fwi.feature_id
                    WHERE fwi.work_item_id = ?1
                      AND f.deleted_at IS NULL
                    UNION
                    SELECT p.parent_feature_id
                    FROM items p
                    JOIN attached_ancestors aa ON p.id = aa.feature_id
                    WHERE p.parent_feature_id IS NOT NULL
                      AND p.deleted_at IS NULL
                )
                SELECT EXISTS (
                    SELECT 1 FROM attached_ancestors WHERE feature_id = ?2
                );",
                params![&blocked_sql, &blocker_sql],
                |row| row.get(0),
            )
            .map_err(map_sqlite_err)?;

        if violates_exclusion != 0 {
            return Err(DomainError::DependencyCycle(format!(
                "Work Item {} cannot be directly blocked on attached Feature {} or its ancestor",
                blocked.item_key, blocker.item_key
            )));
        }

        return Ok((blocked, blocker));
    }

    // Blocked item is a Feature: check ancestor/descendant exclusion and combined graph reachability.
    let is_ancestor_or_descendant: i64 = conn
        .query_row(
            "WITH RECURSIVE ancestors(id) AS (
                SELECT ?1
                UNION
                SELECT i.parent_feature_id
                FROM items i
                JOIN ancestors a ON i.id = a.id
                WHERE i.parent_feature_id IS NOT NULL
                  AND i.deleted_at IS NULL
            ),
            descendants(id) AS (
                SELECT ?1
                UNION
                SELECT c.id
                FROM items c
                JOIN descendants d ON c.parent_feature_id = d.id
                WHERE c.kind = 'feature'
                  AND c.deleted_at IS NULL
            )
            SELECT (
                EXISTS (SELECT 1 FROM ancestors WHERE id = ?2)
                OR EXISTS (SELECT 1 FROM descendants WHERE id = ?2)
            );",
            params![&blocked_sql, &blocker_sql],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;

    if is_ancestor_or_descendant != 0 {
        return Err(DomainError::DependencyCycle(format!(
            "Feature {} cannot be directly blocked on its ancestor or descendant {}",
            blocked.item_key, blocker.item_key
        )));
    }

    let creates_cycle: i64 = conn
        .query_row(
            "WITH RECURSIVE combined_reach(node_id) AS (
                SELECT ?2
                UNION
                SELECT edges.next_id
                FROM (
                    SELECT i.id AS from_id, i.parent_feature_id AS next_id
                    FROM items i
                    JOIN items p ON p.id = i.parent_feature_id
                    WHERE i.project_id = ?1
                      AND i.kind = 'feature'
                      AND i.deleted_at IS NULL
                      AND p.kind = 'feature'
                      AND p.deleted_at IS NULL
                    UNION ALL
                    SELECT ib.blocked_item_id AS from_id, ib.blocker_feature_id AS next_id
                    FROM item_blockers ib
                    JOIN items b ON b.id = ib.blocked_item_id
                    JOIN items r ON r.id = ib.blocker_feature_id
                    WHERE ib.project_id = ?1
                      AND b.kind = 'feature'
                      AND b.deleted_at IS NULL
                      AND r.kind = 'feature'
                      AND r.deleted_at IS NULL
                ) edges
                JOIN combined_reach cr ON edges.from_id = cr.node_id
            )
            SELECT EXISTS (SELECT 1 FROM combined_reach WHERE node_id = ?3);",
            params![&project_sql, &blocker_sql, &blocked_sql],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;

    if creates_cycle != 0 {
        return Err(DomainError::DependencyCycle(format!(
            "Blocking {} on {} would create a dependency cycle",
            blocked.item_key, blocker.item_key
        )));
    }

    Ok((blocked, blocker))
}

/// Validates `INV-05` before reparenting `feature_id` under `new_parent_id`.
pub fn validate_reparent_blockers(
    conn: &Connection,
    project_id: ProjectId,
    feature_id: ItemId,
    new_parent_id: ItemId,
) -> Result<(), DomainError> {
    let project_sql = uuid_to_sql(project_id.0);
    let feat_sql = uuid_to_sql(feature_id.0);
    let parent_sql = uuid_to_sql(new_parent_id.0);

    let creates_cycle: i64 = conn
        .query_row(
            "WITH RECURSIVE combined_reach(node_id) AS (
                SELECT ?2
                UNION
                SELECT edges.next_id
                FROM (
                    SELECT i.id AS from_id, i.parent_feature_id AS next_id
                    FROM items i
                    JOIN items p ON p.id = i.parent_feature_id
                    WHERE i.project_id = ?1
                      AND i.kind = 'feature'
                      AND i.deleted_at IS NULL
                      AND p.kind = 'feature'
                      AND p.deleted_at IS NULL
                    UNION ALL
                    SELECT ib.blocked_item_id AS from_id, ib.blocker_feature_id AS next_id
                    FROM item_blockers ib
                    JOIN items b ON b.id = ib.blocked_item_id
                    JOIN items r ON r.id = ib.blocker_feature_id
                    WHERE ib.project_id = ?1
                      AND b.kind = 'feature'
                      AND b.deleted_at IS NULL
                      AND r.kind = 'feature'
                      AND r.deleted_at IS NULL
                ) edges
                JOIN combined_reach cr ON edges.from_id = cr.node_id
            )
            SELECT EXISTS (SELECT 1 FROM combined_reach WHERE node_id = ?3);",
            params![&project_sql, &parent_sql, &feat_sql],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;

    if creates_cycle != 0 {
        return Err(DomainError::DependencyCycle(
            "Moving Feature under this parent would create a dependency cycle".to_string(),
        ));
    }

    let (subtree_blocked, ancestor_blocked, wi_blocked): (i64, i64, i64) = conn
        .query_row(
            "WITH RECURSIVE new_ancestors(id) AS (
                SELECT ?2
                UNION
                SELECT i.parent_feature_id
                FROM items i
                JOIN new_ancestors na ON i.id = na.id
                WHERE i.parent_feature_id IS NOT NULL
                  AND i.deleted_at IS NULL
            ),
            moving_subtree(id) AS (
                SELECT ?3
                UNION
                SELECT c.id
                FROM items c
                JOIN moving_subtree ms ON c.parent_feature_id = ms.id
                WHERE c.kind = 'feature'
                  AND c.deleted_at IS NULL
            )
            SELECT
                EXISTS (
                    SELECT 1 FROM item_blockers ib
                    WHERE ib.project_id = ?1
                      AND ib.blocked_item_id IN (SELECT id FROM moving_subtree)
                      AND ib.blocker_feature_id IN (SELECT id FROM new_ancestors)
                ),
                EXISTS (
                    SELECT 1 FROM item_blockers ib
                    WHERE ib.project_id = ?1
                      AND ib.blocked_item_id IN (SELECT id FROM new_ancestors)
                      AND ib.blocker_feature_id IN (SELECT id FROM moving_subtree)
                ),
                EXISTS (
                    SELECT 1
                    FROM feature_work_items fwi
                    JOIN items wi ON wi.id = fwi.work_item_id
                    JOIN item_blockers ib ON ib.blocked_item_id = wi.id
                    WHERE fwi.project_id = ?1
                      AND wi.deleted_at IS NULL
                      AND fwi.feature_id IN (SELECT id FROM moving_subtree)
                      AND ib.blocker_feature_id IN (SELECT id FROM new_ancestors)
                );",
            params![&project_sql, &parent_sql, &feat_sql],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(map_sqlite_err)?;

    if subtree_blocked != 0 || ancestor_blocked != 0 {
        return Err(DomainError::DependencyCycle(
            "Moving Feature here violates ancestor/descendant blocker exclusion (INV-05)"
                .to_string(),
        ));
    }
    if wi_blocked != 0 {
        return Err(DomainError::DependencyCycle(
            "A Work Item in this Feature subtree is directly blocked on the target ancestor (INV-05)"
                .to_string(),
        ));
    }

    Ok(())
}

/// Validates `INV-05` before attaching `work_item_id` to `feature_id`.
pub fn validate_attach_work_item_blockers(
    conn: &Connection,
    work_item_id: ItemId,
    feature_id: ItemId,
) -> Result<(), DomainError> {
    let violates: i64 = conn
        .query_row(
            "WITH RECURSIVE target_ancestors(id) AS (
                SELECT ?2
                UNION
                SELECT i.parent_feature_id
                FROM items i
                JOIN target_ancestors ta ON i.id = ta.id
                WHERE i.parent_feature_id IS NOT NULL
                  AND i.deleted_at IS NULL
            )
            SELECT EXISTS (
                SELECT 1
                FROM item_blockers ib
                WHERE ib.blocked_item_id = ?1
                  AND ib.blocker_feature_id IN (SELECT id FROM target_ancestors)
            );",
            params![uuid_to_sql(work_item_id.0), uuid_to_sql(feature_id.0)],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;

    if violates != 0 {
        return Err(DomainError::DependencyCycle(
            "Work Item cannot attach to a Feature that blocks it directly or via an ancestor (INV-05)"
                .to_string(),
        ));
    }

    Ok(())
}

pub fn add_blocker(
    tx: &Transaction<'_>,
    blocked_item_id: ItemId,
    blocker_feature_id: ItemId,
) -> Result<(ItemBlocker, bool), DomainError> {
    let (blocked, _blocker) = validate_add_blocker(tx, blocked_item_id, blocker_feature_id)?;
    let project_sql = uuid_to_sql(blocked.project_id.0);
    let blocked_sql = uuid_to_sql(blocked_item_id.0);
    let blocker_sql = uuid_to_sql(blocker_feature_id.0);

    if let Some(existing) = tx
        .query_row(
            "SELECT project_id, blocked_item_id, blocker_feature_id, created_at
             FROM item_blockers
             WHERE blocked_item_id = ?1 AND blocker_feature_id = ?2;",
            params![&blocked_sql, &blocker_sql],
            |row| {
                Ok(ItemBlocker {
                    project_id: ProjectId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                    blocked_item_id: ItemId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                    blocker_feature_id: ItemId::from_uuid(parse_uuid_col(2, row.get(2)?)?),
                    created_at: parse_ts_col(3, row.get(3)?)?,
                })
            },
        )
        .optional()
        .map_err(map_sqlite_err)?
    {
        return Ok((existing, false));
    }

    let now = Utc::now();
    let now_sql = ts_to_sql(now);

    tx.execute(
        "INSERT INTO item_blockers (project_id, blocked_item_id, blocker_feature_id, created_at)
         VALUES (?1, ?2, ?3, ?4);",
        params![&project_sql, &blocked_sql, &blocker_sql, &now_sql],
    )
    .map_err(map_sqlite_err)?;

    tx.execute(
        "UPDATE items SET updated_at = ?1 WHERE id = ?2;",
        params![&now_sql, &blocked_sql],
    )
    .map_err(map_sqlite_err)?;

    Ok((
        ItemBlocker {
            project_id: blocked.project_id,
            blocked_item_id,
            blocker_feature_id,
            created_at: now,
        },
        true,
    ))
}

pub fn remove_blocker(
    tx: &Transaction<'_>,
    blocked_item_id: ItemId,
    blocker_feature_id: ItemId,
) -> Result<(ProjectId, bool), DomainError> {
    let blocked = get_item(tx, blocked_item_id)?;
    if !blocked.is_active() {
        return Err(DomainError::Validation(
            "Cannot modify blockers of an item in Trash".to_string(),
        ));
    }

    if let Some(blocker_pid_raw) = tx
        .query_row(
            "SELECT project_id FROM items WHERE id = ?1;",
            params![uuid_to_sql(blocker_feature_id.0)],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_sqlite_err)?
    {
        let blocker_pid =
            ProjectId::from_uuid(parse_uuid_col(0, blocker_pid_raw).map_err(map_sqlite_err)?);
        if blocker_pid != blocked.project_id {
            return Err(DomainError::CrossProjectReference);
        }
    }

    let blocked_sql = uuid_to_sql(blocked_item_id.0);
    let deleted = tx
        .execute(
            "DELETE FROM item_blockers
             WHERE blocked_item_id = ?1 AND blocker_feature_id = ?2;",
            params![&blocked_sql, uuid_to_sql(blocker_feature_id.0)],
        )
        .map_err(map_sqlite_err)?;

    if deleted > 0 {
        let now_sql = ts_to_sql(Utc::now());
        tx.execute(
            "UPDATE items SET updated_at = ?1 WHERE id = ?2;",
            params![&now_sql, &blocked_sql],
        )
        .map_err(map_sqlite_err)?;
        Ok((blocked.project_id, true))
    } else {
        Ok((blocked.project_id, false))
    }
}

/// Loads all blocker edges, direct/inherited refs, effective states, and warnings (`DATA_MODEL.md` §4.4).
pub fn load_project_block_snapshot(
    conn: &Connection,
    project_id: ProjectId,
) -> Result<ProjectBlockSnapshot, DomainError> {
    let project_sql = uuid_to_sql(project_id.0);

    let item_blockers = {
        let mut stmt = conn
            .prepare(
                "SELECT ib.project_id, ib.blocked_item_id, ib.blocker_feature_id, ib.created_at
                 FROM item_blockers ib
                 JOIN items i ON i.id = ib.blocked_item_id
                 WHERE ib.project_id = ?1
                   AND i.deleted_at IS NULL
                 ORDER BY ib.created_at ASC, ib.blocker_feature_id ASC;",
            )
            .map_err(map_sqlite_err)?;
        let rows = stmt
            .query_map(params![&project_sql], |row| {
                Ok(ItemBlocker {
                    project_id: ProjectId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                    blocked_item_id: ItemId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                    blocker_feature_id: ItemId::from_uuid(parse_uuid_col(2, row.get(2)?)?),
                    created_at: parse_ts_col(3, row.get(3)?)?,
                })
            })
            .map_err(map_sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)?
    };

    let mut direct_blockers_by_item: HashMap<ItemId, Vec<BlockerRef>> = HashMap::new();
    let mut blocker_warnings: HashMap<ItemId, Vec<BlockerWarning>> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT ib.blocked_item_id, b.id, b.item_key, b.title, b.status,
                        (b.deleted_at IS NOT NULL) AS in_trash
                 FROM item_blockers ib
                 JOIN items i ON i.id = ib.blocked_item_id
                 JOIN items b ON b.id = ib.blocker_feature_id
                 WHERE ib.project_id = ?1
                   AND i.deleted_at IS NULL
                 ORDER BY b.item_number ASC;",
            )
            .map_err(map_sqlite_err)?;
        let rows = stmt
            .query_map(params![&project_sql], |row| {
                let item_id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
                let blocker_id = ItemId::from_uuid(parse_uuid_col(1, row.get(1)?)?);
                let blocker_key: String = row.get(2)?;
                let blocker_title: String = row.get(3)?;
                let status_raw: String = row.get(4)?;
                let blocker_status = parse_domain_col(4, ItemStatus::from_db_str(&status_raw))?;
                let in_trash_i64: i64 = row.get(5)?;
                Ok((
                    item_id,
                    BlockerRef {
                        blocker_id,
                        blocker_key,
                        blocker_title,
                        blocker_status,
                        blocker_in_trash: in_trash_i64 != 0,
                        via_ancestor_id: None,
                        via_ancestor_key: None,
                        via_ancestor_title: None,
                    },
                ))
            })
            .map_err(map_sqlite_err)?;

        for pair in rows {
            let (item_id, bref) = pair.map_err(map_sqlite_err)?;
            if bref.blocker_in_trash {
                blocker_warnings
                    .entry(item_id)
                    .or_default()
                    .push(BlockerWarning::InTrash {
                        blocker_id: bref.blocker_id,
                        blocker_key: bref.blocker_key.clone(),
                        title: bref.blocker_title.clone(),
                    });
            } else if bref.is_cancelled() {
                blocker_warnings
                    .entry(item_id)
                    .or_default()
                    .push(BlockerWarning::Cancelled {
                        blocker_id: bref.blocker_id,
                        blocker_key: bref.blocker_key.clone(),
                        title: bref.blocker_title.clone(),
                    });
            }
            direct_blockers_by_item
                .entry(item_id)
                .or_default()
                .push(bref);
        }
    }

    // Inherited active blockers with `via_ancestor` attribution.
    let mut inherited_blockers_by_item: HashMap<ItemId, Vec<BlockerRef>> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "WITH RECURSIVE feature_ancestors(feature_id, ancestor_id, hop) AS (
                    SELECT id, parent_feature_id, 1
                    FROM items
                    WHERE project_id = ?1
                      AND kind = 'feature'
                      AND parent_feature_id IS NOT NULL
                      AND deleted_at IS NULL
                    UNION ALL
                    SELECT fa.feature_id, p.parent_feature_id, fa.hop + 1
                    FROM feature_ancestors fa
                    JOIN items p ON p.id = fa.ancestor_id
                    WHERE p.parent_feature_id IS NOT NULL
                      AND p.deleted_at IS NULL
                      AND fa.hop < 32
                ),
                active_direct AS (
                    SELECT ib.blocked_item_id AS item_id,
                           b.id AS blocker_id,
                           b.item_key AS blocker_key,
                           b.title AS blocker_title,
                           b.status AS blocker_status,
                           b.item_number AS blocker_number
                    FROM item_blockers ib
                    JOIN items b ON b.id = ib.blocker_feature_id
                    WHERE ib.project_id = ?1
                      AND b.deleted_at IS NULL
                      AND b.status NOT IN ('done', 'cancelled')
                ),
                inherited_raw AS (
                    -- Inherited blockers for Features via strict ancestors
                    SELECT fa.feature_id AS item_id,
                           ad.blocker_id,
                           ad.blocker_key,
                           ad.blocker_title,
                           ad.blocker_status,
                           anc.id AS via_id,
                           anc.item_key AS via_key,
                           anc.title AS via_title,
                           fa.hop AS sort_hop,
                           ad.blocker_number
                    FROM feature_ancestors fa
                    JOIN active_direct ad ON ad.item_id = fa.ancestor_id
                    JOIN items anc ON anc.id = fa.ancestor_id
                    WHERE anc.deleted_at IS NULL

                    UNION ALL

                    -- Inherited blockers for Work Items via attached active Features
                    SELECT wi.id AS item_id,
                           ad.blocker_id,
                           ad.blocker_key,
                           ad.blocker_title,
                           ad.blocker_status,
                           f.id AS via_id,
                           f.item_key AS via_key,
                           f.title AS via_title,
                           0 AS sort_hop,
                           ad.blocker_number
                    FROM items wi
                    JOIN feature_work_items fwi ON fwi.work_item_id = wi.id
                    JOIN items f ON f.id = fwi.feature_id
                    JOIN active_direct ad ON ad.item_id = f.id
                    WHERE wi.project_id = ?1
                      AND wi.kind = 'work_item'
                      AND wi.deleted_at IS NULL
                      AND f.deleted_at IS NULL

                    UNION ALL

                    -- Inherited blockers for Work Items via ancestors of attached active Features
                    SELECT wi.id AS item_id,
                           ad.blocker_id,
                           ad.blocker_key,
                           ad.blocker_title,
                           ad.blocker_status,
                           anc.id AS via_id,
                           anc.item_key AS via_key,
                           anc.title AS via_title,
                           fa.hop AS sort_hop,
                           ad.blocker_number
                    FROM items wi
                    JOIN feature_work_items fwi ON fwi.work_item_id = wi.id
                    JOIN items f ON f.id = fwi.feature_id
                    JOIN feature_ancestors fa ON fa.feature_id = f.id
                    JOIN active_direct ad ON ad.item_id = fa.ancestor_id
                    JOIN items anc ON anc.id = fa.ancestor_id
                    WHERE wi.project_id = ?1
                      AND wi.kind = 'work_item'
                      AND wi.deleted_at IS NULL
                      AND f.deleted_at IS NULL
                      AND anc.deleted_at IS NULL
                )
                SELECT item_id, blocker_id, blocker_key, blocker_title, blocker_status,
                       via_id, via_key, via_title
                FROM inherited_raw
                ORDER BY item_id ASC, sort_hop ASC, blocker_number ASC;",
            )
            .map_err(map_sqlite_err)?;

        let rows = stmt
            .query_map(params![&project_sql], |row| {
                let item_id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
                let blocker_id = ItemId::from_uuid(parse_uuid_col(1, row.get(1)?)?);
                let blocker_key: String = row.get(2)?;
                let blocker_title: String = row.get(3)?;
                let status_raw: String = row.get(4)?;
                let blocker_status = parse_domain_col(4, ItemStatus::from_db_str(&status_raw))?;
                let via_id = ItemId::from_uuid(parse_uuid_col(5, row.get(5)?)?);
                let via_key: String = row.get(6)?;
                let via_title: String = row.get(7)?;
                Ok((
                    item_id,
                    BlockerRef {
                        blocker_id,
                        blocker_key,
                        blocker_title,
                        blocker_status,
                        blocker_in_trash: false,
                        via_ancestor_id: Some(via_id),
                        via_ancestor_key: Some(via_key),
                        via_ancestor_title: Some(via_title),
                    },
                ))
            })
            .map_err(map_sqlite_err)?;

        let mut seen_pairs: HashSet<(ItemId, ItemId, ItemId)> = HashSet::new();
        for pair in rows {
            let (item_id, bref) = pair.map_err(map_sqlite_err)?;
            let via_id = bref.via_ancestor_id.unwrap_or(bref.blocker_id);
            if seen_pairs.insert((item_id, bref.blocker_id, via_id)) {
                inherited_blockers_by_item
                    .entry(item_id)
                    .or_default()
                    .push(bref);
            }
        }
    }

    // Effective blocking state CTE (`DATA_MODEL.md` §4.4).
    let mut effective_block_state: HashMap<ItemId, EffectiveBlockState> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "WITH RECURSIVE feature_ancestors(feature_id, ancestor_id, hop) AS (
                    SELECT id, parent_feature_id, 1
                    FROM items
                    WHERE project_id = ?1
                      AND kind = 'feature'
                      AND parent_feature_id IS NOT NULL
                      AND deleted_at IS NULL
                    UNION ALL
                    SELECT fa.feature_id, p.parent_feature_id, fa.hop + 1
                    FROM feature_ancestors fa
                    JOIN items p ON p.id = fa.ancestor_id
                    WHERE p.parent_feature_id IS NOT NULL
                      AND p.deleted_at IS NULL
                      AND fa.hop < 32
                ),
                direct_blockers AS (
                    SELECT
                        ib.blocked_item_id AS item_id,
                        b.id AS blocker_id,
                        (b.deleted_at IS NULL AND b.status NOT IN ('done', 'cancelled')) AS is_active_block
                    FROM item_blockers ib
                    JOIN items b ON b.id = ib.blocker_feature_id
                    WHERE ib.project_id = ?1
                ),
                feature_effective AS (
                    SELECT
                        f.id AS feature_id,
                        EXISTS (
                            SELECT 1 FROM direct_blockers db
                            WHERE db.item_id = f.id AND db.is_active_block = 1
                        ) AS has_direct_block,
                        (
                            SELECT COUNT(DISTINCT db.blocker_id)
                            FROM feature_ancestors fa
                            JOIN direct_blockers db ON db.item_id = fa.ancestor_id
                            WHERE fa.feature_id = f.id AND db.is_active_block = 1
                        ) AS ancestor_blocker_count
                    FROM items f
                    WHERE f.project_id = ?1
                      AND f.kind = 'feature'
                      AND f.deleted_at IS NULL
                ),
                work_item_feature_stats AS (
                    SELECT
                        wi.id AS work_item_id,
                        COUNT(fe.feature_id) AS attached_feature_count,
                        COALESCE(SUM(CASE WHEN fe.has_direct_block OR fe.ancestor_blocker_count > 0 THEN 1 ELSE 0 END), 0) AS blocked_feature_count
                    FROM items wi
                    LEFT JOIN feature_work_items fwi ON fwi.work_item_id = wi.id
                    LEFT JOIN feature_effective fe ON fe.feature_id = fwi.feature_id
                    WHERE wi.project_id = ?1
                      AND wi.kind = 'work_item'
                      AND wi.deleted_at IS NULL
                    GROUP BY wi.id
                )
                SELECT
                    i.id,
                    i.kind,
                    (SELECT COUNT(*) FROM direct_blockers db WHERE db.item_id = i.id AND db.is_active_block = 1) AS active_direct_count,
                    COALESCE(fe.ancestor_blocker_count, 0) AS ancestor_blocker_count,
                    COALESCE(wfs.blocked_feature_count, 0) AS blocked_feature_count,
                    COALESCE(wfs.attached_feature_count, 0) AS attached_feature_count
                FROM items i
                LEFT JOIN feature_effective fe ON fe.feature_id = i.id
                LEFT JOIN work_item_feature_stats wfs ON wfs.work_item_id = i.id
                WHERE i.project_id = ?1
                  AND i.deleted_at IS NULL;",
            )
            .map_err(map_sqlite_err)?;

        let rows = stmt
            .query_map(params![&project_sql], |row| {
                let item_id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
                let kind_raw: String = row.get(1)?;
                let kind = parse_domain_col(1, ItemKind::from_db_str(&kind_raw))?;
                let active_direct = usize::try_from(row.get::<_, i64>(2)?).unwrap_or(0);
                let ancestor_blockers = usize::try_from(row.get::<_, i64>(3)?).unwrap_or(0);
                let blocked_features = usize::try_from(row.get::<_, i64>(4)?).unwrap_or(0);
                let attached_features = usize::try_from(row.get::<_, i64>(5)?).unwrap_or(0);
                Ok((
                    item_id,
                    kind,
                    active_direct,
                    ancestor_blockers,
                    blocked_features,
                    attached_features,
                ))
            })
            .map_err(map_sqlite_err)?;

        for row in rows {
            let (
                item_id,
                kind,
                active_direct,
                ancestor_blockers,
                blocked_features,
                attached_features,
            ) = row.map_err(map_sqlite_err)?;

            let state = if active_direct > 0 {
                EffectiveBlockState::DirectlyBlocked {
                    active_count: active_direct,
                }
            } else if kind == ItemKind::Feature && ancestor_blockers > 0 {
                EffectiveBlockState::InheritedBlocked {
                    ancestor_blocker_count: ancestor_blockers,
                }
            } else if kind == ItemKind::WorkItem
                && attached_features > 0
                && blocked_features == attached_features
            {
                let distinct_inherited = inherited_blockers_by_item
                    .get(&item_id)
                    .map(|refs| {
                        refs.iter()
                            .map(|r| r.blocker_id)
                            .collect::<HashSet<_>>()
                            .len()
                    })
                    .unwrap_or(0);
                EffectiveBlockState::InheritedBlocked {
                    ancestor_blocker_count: distinct_inherited.max(1),
                }
            } else if kind == ItemKind::WorkItem
                && blocked_features > 0
                && blocked_features < attached_features
            {
                EffectiveBlockState::PartiallyBlocked {
                    blocked_features,
                    total_features: attached_features,
                }
            } else {
                EffectiveBlockState::Unblocked
            };

            effective_block_state.insert(item_id, state);
        }
    }

    Ok(ProjectBlockSnapshot {
        item_blockers,
        direct_blockers_by_item,
        inherited_blockers_by_item,
        effective_block_state,
        blocker_warnings,
    })
}

/// Returns `(effective_block_state, active_blocker_keys)` for `item_id` (used for soft-block toast warnings).
pub fn query_active_blocker_keys_for_item(
    conn: &Connection,
    project_id: ProjectId,
    item_id: ItemId,
) -> Result<(EffectiveBlockState, Vec<String>), DomainError> {
    let block_snap = load_project_block_snapshot(conn, project_id)?;
    let state = block_snap
        .effective_block_state
        .get(&item_id)
        .copied()
        .unwrap_or(EffectiveBlockState::Unblocked);

    let mut seen = HashSet::new();
    let mut keys = Vec::new();
    if let Some(direct) = block_snap.direct_blockers_by_item.get(&item_id) {
        for r in direct {
            if r.is_active_block() && seen.insert(r.blocker_key.clone()) {
                keys.push(r.blocker_key.clone());
            }
        }
    }
    if let Some(inherited) = block_snap.inherited_blockers_by_item.get(&item_id) {
        for r in inherited {
            if r.is_active_block() && seen.insert(r.blocker_key.clone()) {
                keys.push(r.blocker_key.clone());
            }
        }
    }
    Ok((state, keys))
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, TransactionBehavior};

    use super::*;
    use crate::db::inspect_and_migrate;
    use crate::db::item_repo::{
        CreateFeatureInput, CreateWorkItemInput, UpdateItemFieldsInput, attach_work_item_to_feature,
        create_feature, create_work_item, reparent_feature, set_work_item_features,
        update_item_fields,
    };
    use crate::db::load_project_snapshot;
    use crate::db::project_repo::{CreateProjectInput, create_project};
    use crate::domain::{BlockerCandidateRejection, Priority, ProjectStatus};

    fn setup_two_projects() -> (Connection, ProjectId, ProjectId) {
        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "BlockerTest").unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let p_a = create_project(
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
        let p_b = create_project(
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
        (conn, p_a.id, p_b.id)
    }

    fn mk_feature(
        conn: &mut Connection,
        pid: ProjectId,
        parent: Option<ItemId>,
        title: &str,
    ) -> Item {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let item = create_feature(
            &tx,
            CreateFeatureInput {
                project_id: pid,
                parent_feature_id: parent,
                title: title.to_string(),
                description: String::new(),
                status: ItemStatus::Todo,
                priority: Priority::None,
            },
        )
        .unwrap();
        tx.commit().unwrap();
        item
    }

    fn mk_wi(conn: &mut Connection, pid: ProjectId, features: Vec<ItemId>, title: &str) -> Item {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let item = create_work_item(
            &tx,
            CreateWorkItemInput {
                project_id: pid,
                title: title.to_string(),
                description: String::new(),
                status: ItemStatus::Todo,
                priority: Priority::None,
                feature_ids: features,
            },
        )
        .unwrap();
        tx.commit().unwrap();
        item
    }

    #[test]
    fn inv04_inv05_rejects_work_item_blockers_cross_project_and_cycles() {
        let (mut conn, pid_a, pid_b) = setup_two_projects();

        // Hierarchy in A: F1 -> F2; sibling F3; outside project B: FB
        let f1 = mk_feature(&mut conn, pid_a, None, "F1");
        let f2 = mk_feature(&mut conn, pid_a, Some(f1.id), "F2");
        let f3 = mk_feature(&mut conn, pid_a, None, "F3");
        let wi_on_f2 = mk_wi(&mut conn, pid_a, vec![f2.id], "WI on F2");
        let wi_unalloc = mk_wi(&mut conn, pid_a, vec![], "Unallocated WI");
        let fb = mk_feature(&mut conn, pid_b, None, "FB");

        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            // INV-04: Work Item cannot be blocker
            assert!(matches!(
                add_blocker(&tx, f3.id, wi_unalloc.id),
                Err(DomainError::Validation(_))
            ));
            // INV-01: Cross-project blocker rejected
            assert_eq!(
                add_blocker(&tx, f3.id, fb.id),
                Err(DomainError::CrossProjectReference)
            );
            // Self-block rejected
            assert!(matches!(
                add_blocker(&tx, f1.id, f1.id),
                Err(DomainError::DependencyCycle(_))
            ));
            // Direct ancestor / descendant exclusion for Feature
            assert!(matches!(
                add_blocker(&tx, f2.id, f1.id),
                Err(DomainError::DependencyCycle(_))
            ));
            assert!(matches!(
                add_blocker(&tx, f1.id, f2.id),
                Err(DomainError::DependencyCycle(_))
            ));
            // Work Item attached/ancestor exclusion
            assert!(matches!(
                add_blocker(&tx, wi_on_f2.id, f2.id),
                Err(DomainError::DependencyCycle(_))
            ));
            assert!(matches!(
                add_blocker(&tx, wi_on_f2.id, f1.id),
                Err(DomainError::DependencyCycle(_))
            ));

            // Mixed hierarchy + blocker cycle: F2 -> F1 (hierarchy), add F1 blocked by F3, then F3 blocked by F2 must fail!
            assert!(add_blocker(&tx, f1.id, f3.id).unwrap().1);
            assert!(matches!(
                add_blocker(&tx, f3.id, f2.id),
                Err(DomainError::DependencyCycle(_))
            ));
            tx.commit().unwrap();
        }

        // Snapshot pre-filter agrees with SQLite rejections
        let snap = load_project_snapshot(&conn, pid_a).unwrap();
        assert_eq!(
            snap.blocker_candidate_rejection(f1.id, f1.id),
            Some(BlockerCandidateRejection::SelfReference)
        );
        assert_eq!(
            snap.blocker_candidate_rejection(f1.id, f3.id),
            Some(BlockerCandidateRejection::AlreadyBlockedBy)
        );
        assert_eq!(
            snap.blocker_candidate_rejection(f2.id, f1.id),
            Some(BlockerCandidateRejection::AncestorOfItem)
        );
        assert_eq!(
            snap.blocker_candidate_rejection(f1.id, f2.id),
            Some(BlockerCandidateRejection::DescendantOfItem)
        );
        assert_eq!(
            snap.blocker_candidate_rejection(f3.id, f2.id),
            Some(BlockerCandidateRejection::WouldCreateCycle)
        );
        assert_eq!(
            snap.blocker_candidate_rejection(wi_on_f2.id, f2.id),
            Some(BlockerCandidateRejection::AttachedFeatureOfWorkItem)
        );
        assert_eq!(
            snap.blocker_candidate_rejection(wi_on_f2.id, f1.id),
            Some(BlockerCandidateRejection::AncestorOfAttachedFeature)
        );

        // Reparenting F3 under F2 would close F2 -> F1 -> F3 -> F2 cycle
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert!(matches!(
                reparent_feature(&tx, f3.id, Some(f2.id)),
                Err(DomainError::DependencyCycle(_))
            ));
        }
        assert!(snap.reparent_blocker_conflict(f3.id, f2.id).is_some());

        // Work Item blocked on F3 cannot attach to F3 or F3's descendants; reparenting F2 under F3 must fail if wi_on_f2 is blocked on F3
        let wi_blocked_on_f3 = mk_wi(&mut conn, pid_a, vec![], "Blocked on F3");
        let f3_child = mk_feature(&mut conn, pid_a, Some(f3.id), "F3 Child");
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            add_blocker(&tx, wi_blocked_on_f3.id, f3.id).unwrap();
            assert!(matches!(
                attach_work_item_to_feature(&tx, wi_blocked_on_f3.id, f3.id),
                Err(DomainError::DependencyCycle(_))
            ));
            assert!(matches!(
                attach_work_item_to_feature(&tx, wi_blocked_on_f3.id, f3_child.id),
                Err(DomainError::DependencyCycle(_))
            ));
            assert!(matches!(
                set_work_item_features(&tx, wi_blocked_on_f3.id, &[f3_child.id]),
                Err(DomainError::DependencyCycle(_))
            ));

            // Also block wi_on_f2 on a fresh root feature F4, then reparenting F2 under F4 must fail
            let f4 = create_feature(
                &tx,
                CreateFeatureInput {
                    project_id: pid_a,
                    parent_feature_id: None,
                    title: "F4".to_string(),
                    description: String::new(),
                    status: ItemStatus::Todo,
                    priority: Priority::None,
                },
            )
            .unwrap();
            add_blocker(&tx, wi_on_f2.id, f4.id).unwrap();
            assert!(matches!(
                reparent_feature(&tx, f2.id, Some(f4.id)),
                Err(DomainError::DependencyCycle(_))
            ));
        }
    }

    #[test]
    fn inv06_inv07_effective_states_partial_resolution_cancelled_and_trash_warnings() {
        let (mut conn, pid, _) = setup_two_projects();

        // A -> A1; B (blocker); C (unblocked)
        let f_a = mk_feature(&mut conn, pid, None, "Feature A");
        let f_a1 = mk_feature(&mut conn, pid, Some(f_a.id), "Child A1");
        let f_b = mk_feature(&mut conn, pid, None, "Feature B");
        let f_c = mk_feature(&mut conn, pid, None, "Feature C");

        let wi_excl = mk_wi(&mut conn, pid, vec![f_a1.id], "Exclusive WI");
        let wi_shared = mk_wi(&mut conn, pid, vec![f_a1.id, f_c.id], "Shared WI");
        let wi_all_blocked = mk_wi(&mut conn, pid, vec![f_a.id, f_a1.id], "All-blocked WI");
        let wi_unalloc = mk_wi(&mut conn, pid, vec![], "Unallocated WI");

        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            add_blocker(&tx, f_a.id, f_b.id).unwrap();
            tx.commit().unwrap();
        }

        let snap = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(
            snap.effective_block_state(f_a.id),
            EffectiveBlockState::DirectlyBlocked { active_count: 1 }
        );
        assert_eq!(
            snap.effective_block_state(f_a1.id),
            EffectiveBlockState::InheritedBlocked {
                ancestor_blocker_count: 1
            }
        );
        assert_eq!(
            snap.effective_block_state(wi_excl.id),
            EffectiveBlockState::InheritedBlocked {
                ancestor_blocker_count: 1
            }
        );
        assert_eq!(
            snap.effective_block_state(wi_all_blocked.id),
            EffectiveBlockState::InheritedBlocked {
                ancestor_blocker_count: 1
            }
        );
        assert_eq!(
            snap.effective_block_state(wi_shared.id),
            EffectiveBlockState::PartiallyBlocked {
                blocked_features: 1,
                total_features: 2,
            }
        );
        assert_eq!(
            snap.effective_block_state(wi_unalloc.id),
            EffectiveBlockState::Unblocked
        );
        assert!(!snap.inherited_blockers(f_a1.id).is_empty());
        assert_eq!(snap.dependents_of_feature(f_b.id).len(), 1);

        // Cancelling B clears active blocks and surfaces Cancelled warning without locking status
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            update_item_fields(
                &tx,
                UpdateItemFieldsInput {
                    item_id: f_b.id,
                    status: Some(ItemStatus::Cancelled),
                    ..Default::default()
                },
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let snap_cancelled = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(
            snap_cancelled.effective_block_state(f_a.id),
            EffectiveBlockState::Unblocked
        );
        assert_eq!(
            snap_cancelled.effective_block_state(wi_excl.id),
            EffectiveBlockState::Unblocked
        );
        assert_eq!(
            snap_cancelled.blocker_warnings(f_a.id),
            &[BlockerWarning::Cancelled {
                blocker_id: f_b.id,
                blocker_key: f_b.item_key.clone(),
                title: f_b.title.clone(),
            }]
        );

        // Reopening B reactivates the historical blocker; completing B (Done) resolves cleanly with no warning
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            update_item_fields(
                &tx,
                UpdateItemFieldsInput {
                    item_id: f_b.id,
                    status: Some(ItemStatus::InProgress),
                    ..Default::default()
                },
            )
            .unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            load_project_snapshot(&conn, pid)
                .unwrap()
                .effective_block_state(f_a.id),
            EffectiveBlockState::DirectlyBlocked { active_count: 1 }
        );

        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            update_item_fields(
                &tx,
                UpdateItemFieldsInput {
                    item_id: f_b.id,
                    status: Some(ItemStatus::Done),
                    ..Default::default()
                },
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let snap_done = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(
            snap_done.effective_block_state(f_a.id),
            EffectiveBlockState::Unblocked
        );
        assert!(snap_done.blocker_warnings(f_a.id).is_empty());

        // Moving blocker B to Trash clears active blocks and emits InTrash warning
        conn.execute(
            "UPDATE items SET status = 'todo', deleted_at = '2026-11-24T00:00:00Z', trash_root_id = id WHERE id = ?1;",
            [uuid_to_sql(f_b.id.0)],
        )
        .unwrap();
        let snap_trash = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(
            snap_trash.effective_block_state(f_a.id),
            EffectiveBlockState::Unblocked
        );
        assert_eq!(
            snap_trash.blocker_warnings(f_a.id),
            &[BlockerWarning::InTrash {
                blocker_id: f_b.id,
                blocker_key: f_b.item_key,
                title: f_b.title,
            }]
        );
    }
}
