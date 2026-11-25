use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use crate::db::{map_sqlite_err, parse_ts_col, parse_uuid_col, ts_to_sql, uuid_to_sql};
use crate::domain::{
    Assignee, DomainError, FeatureWorkItem, Item, ItemBlocker, ItemId, Project, ProjectId, Tag,
    TagId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ItemSortDiff {
    pub id: ItemId,
    pub old_sort: String,
    pub old_updated: DateTime<Utc>,
    pub new_sort: String,
    pub new_updated: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectSortDiff {
    pub id: ProjectId,
    pub old_sort: String,
    pub old_updated: DateTime<Utc>,
    pub new_sort: String,
    pub new_updated: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FwiSortDiff {
    pub feature_id: ItemId,
    pub work_item_id: ItemId,
    pub old_sort: String,
    pub new_sort: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ItemTagRow {
    pub project_id: ProjectId,
    pub item_id: ItemId,
    pub tag_id: TagId,
    pub created_at: DateTime<Utc>,
}

pub(crate) fn load_item_sort_map(
    conn: &Connection,
    project_id: ProjectId,
) -> Result<HashMap<ItemId, (String, DateTime<Utc>)>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, sort_order, updated_at
             FROM items
             WHERE project_id = ?1;",
        )
        .map_err(map_sqlite_err)?;
    let rows = stmt
        .query_map(params![uuid_to_sql(project_id.0)], |row| {
            let id = ItemId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
            let sort_order: String = row.get(1)?;
            let updated_at = parse_ts_col(2, row.get(2)?)?;
            Ok((id, (sort_order, updated_at)))
        })
        .map_err(map_sqlite_err)?;

    let mut out = HashMap::new();
    for pair in rows {
        let (id, val) = pair.map_err(map_sqlite_err)?;
        out.insert(id, val);
    }
    Ok(out)
}

pub(crate) fn diff_item_sorts(
    before: &HashMap<ItemId, (String, DateTime<Utc>)>,
    after: &HashMap<ItemId, (String, DateTime<Utc>)>,
    exclude_ids: &[ItemId],
) -> Vec<ItemSortDiff> {
    let mut diffs = Vec::new();
    for (id, (old_sort, old_updated)) in before {
        if exclude_ids.contains(id) {
            continue;
        }
        if let Some((new_sort, new_updated)) = after.get(id)
            && (old_sort != new_sort || old_updated != new_updated)
        {
            diffs.push(ItemSortDiff {
                id: *id,
                old_sort: old_sort.clone(),
                old_updated: *old_updated,
                new_sort: new_sort.clone(),
                new_updated: *new_updated,
            });
        }
    }
    diffs.sort_by_key(|d| d.id);
    diffs
}

pub(crate) fn apply_item_sort_diffs(
    tx: &Transaction<'_>,
    diffs: &[ItemSortDiff],
    use_old: bool,
) -> Result<(), DomainError> {
    for d in diffs {
        let (sort_order, updated_at) = if use_old {
            (&d.old_sort, d.old_updated)
        } else {
            (&d.new_sort, d.new_updated)
        };
        tx.execute(
            "UPDATE items SET sort_order = ?1, updated_at = ?2 WHERE id = ?3;",
            params![sort_order, ts_to_sql(updated_at), uuid_to_sql(d.id.0)],
        )
        .map_err(map_sqlite_err)?;
    }
    Ok(())
}

pub(crate) fn load_project_sort_map(
    conn: &Connection,
) -> Result<HashMap<ProjectId, (String, DateTime<Utc>)>, DomainError> {
    let mut stmt = conn
        .prepare("SELECT id, sort_order, updated_at FROM projects;")
        .map_err(map_sqlite_err)?;
    let rows = stmt
        .query_map([], |row| {
            let id = ProjectId::from_uuid(parse_uuid_col(0, row.get(0)?)?);
            let sort_order: String = row.get(1)?;
            let updated_at = parse_ts_col(2, row.get(2)?)?;
            Ok((id, (sort_order, updated_at)))
        })
        .map_err(map_sqlite_err)?;
    let mut out = HashMap::new();
    for pair in rows {
        let (id, val) = pair.map_err(map_sqlite_err)?;
        out.insert(id, val);
    }
    Ok(out)
}

pub(crate) fn diff_project_sorts(
    before: &HashMap<ProjectId, (String, DateTime<Utc>)>,
    after: &HashMap<ProjectId, (String, DateTime<Utc>)>,
    exclude_id: Option<ProjectId>,
) -> Vec<ProjectSortDiff> {
    let mut diffs = Vec::new();
    for (id, (old_sort, old_updated)) in before {
        if exclude_id == Some(*id) {
            continue;
        }
        if let Some((new_sort, new_updated)) = after.get(id)
            && (old_sort != new_sort || old_updated != new_updated)
        {
            diffs.push(ProjectSortDiff {
                id: *id,
                old_sort: old_sort.clone(),
                old_updated: *old_updated,
                new_sort: new_sort.clone(),
                new_updated: *new_updated,
            });
        }
    }
    diffs.sort_by_key(|d| d.id);
    diffs
}

pub(crate) fn apply_project_sort_diffs(
    tx: &Transaction<'_>,
    diffs: &[ProjectSortDiff],
    use_old: bool,
) -> Result<(), DomainError> {
    for d in diffs {
        let (sort_order, updated_at) = if use_old {
            (&d.old_sort, d.old_updated)
        } else {
            (&d.new_sort, d.new_updated)
        };
        tx.execute(
            "UPDATE projects SET sort_order = ?1, updated_at = ?2 WHERE id = ?3;",
            params![sort_order, ts_to_sql(updated_at), uuid_to_sql(d.id.0)],
        )
        .map_err(map_sqlite_err)?;
    }
    Ok(())
}

pub(crate) fn load_fwi_rows_for_project(
    conn: &Connection,
    project_id: ProjectId,
) -> Result<Vec<FeatureWorkItem>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT project_id, feature_id, work_item_id, sort_order, created_at
             FROM feature_work_items
             WHERE project_id = ?1
             ORDER BY feature_id ASC, work_item_id ASC;",
        )
        .map_err(map_sqlite_err)?;
    let rows = stmt
        .query_map(params![uuid_to_sql(project_id.0)], |row| {
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

pub(crate) fn load_fwi_rows_for_work_item(
    conn: &Connection,
    work_item_id: ItemId,
) -> Result<Vec<FeatureWorkItem>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT project_id, feature_id, work_item_id, sort_order, created_at
             FROM feature_work_items
             WHERE work_item_id = ?1
             ORDER BY feature_id ASC;",
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

pub(crate) fn load_fwi_rows_for_feature(
    conn: &Connection,
    feature_id: ItemId,
) -> Result<Vec<FeatureWorkItem>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT project_id, feature_id, work_item_id, sort_order, created_at
             FROM feature_work_items
             WHERE feature_id = ?1
             ORDER BY work_item_id ASC;",
        )
        .map_err(map_sqlite_err)?;
    let rows = stmt
        .query_map(params![uuid_to_sql(feature_id.0)], |row| {
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

pub(crate) fn diff_fwi_sorts(
    before: &[FeatureWorkItem],
    after: &[FeatureWorkItem],
) -> Vec<FwiSortDiff> {
    let before_map: HashMap<(ItemId, ItemId), &str> = before
        .iter()
        .map(|r| ((r.feature_id, r.work_item_id), r.sort_order.as_str()))
        .collect();
    let mut diffs = Vec::new();
    for a in after {
        if let Some(&old_sort) = before_map.get(&(a.feature_id, a.work_item_id))
            && old_sort != a.sort_order
        {
            diffs.push(FwiSortDiff {
                feature_id: a.feature_id,
                work_item_id: a.work_item_id,
                old_sort: old_sort.to_string(),
                new_sort: a.sort_order.clone(),
            });
        }
    }
    diffs.sort_by_key(|d| (d.feature_id, d.work_item_id));
    diffs
}

pub(crate) fn apply_fwi_sort_diffs(
    tx: &Transaction<'_>,
    diffs: &[FwiSortDiff],
    use_old: bool,
) -> Result<(), DomainError> {
    for d in diffs {
        let sort = if use_old { &d.old_sort } else { &d.new_sort };
        tx.execute(
            "UPDATE feature_work_items SET sort_order = ?1 WHERE feature_id = ?2 AND work_item_id = ?3;",
            params![
                sort,
                uuid_to_sql(d.feature_id.0),
                uuid_to_sql(d.work_item_id.0)
            ],
        )
        .map_err(map_sqlite_err)?;
    }
    Ok(())
}

pub(crate) fn insert_fwi_row(
    tx: &Transaction<'_>,
    row: &FeatureWorkItem,
) -> Result<(), DomainError> {
    tx.execute(
        "INSERT INTO feature_work_items (project_id, feature_id, work_item_id, sort_order, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5);",
        params![
            uuid_to_sql(row.project_id.0),
            uuid_to_sql(row.feature_id.0),
            uuid_to_sql(row.work_item_id.0),
            &row.sort_order,
            ts_to_sql(row.created_at),
        ],
    )
    .map_err(map_sqlite_err)?;
    Ok(())
}

pub(crate) fn delete_fwi_row(
    tx: &Transaction<'_>,
    feature_id: ItemId,
    work_item_id: ItemId,
) -> Result<(), DomainError> {
    tx.execute(
        "DELETE FROM feature_work_items WHERE feature_id = ?1 AND work_item_id = ?2;",
        params![uuid_to_sql(feature_id.0), uuid_to_sql(work_item_id.0)],
    )
    .map_err(map_sqlite_err)?;
    Ok(())
}

pub(crate) fn load_item_tags_for_item(
    conn: &Connection,
    item_id: ItemId,
) -> Result<Vec<ItemTagRow>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT project_id, item_id, tag_id, created_at
             FROM item_tags
             WHERE item_id = ?1
             ORDER BY created_at ASC, tag_id ASC;",
        )
        .map_err(map_sqlite_err)?;
    let rows = stmt
        .query_map(params![uuid_to_sql(item_id.0)], |row| {
            Ok(ItemTagRow {
                project_id: ProjectId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                item_id: ItemId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                tag_id: TagId::from_uuid(parse_uuid_col(2, row.get(2)?)?),
                created_at: parse_ts_col(3, row.get(3)?)?,
            })
        })
        .map_err(map_sqlite_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

pub(crate) fn load_item_tags_for_tag(
    conn: &Connection,
    tag_id: TagId,
) -> Result<Vec<ItemTagRow>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT project_id, item_id, tag_id, created_at
             FROM item_tags
             WHERE tag_id = ?1
             ORDER BY created_at ASC, item_id ASC;",
        )
        .map_err(map_sqlite_err)?;
    let rows = stmt
        .query_map(params![uuid_to_sql(tag_id.0)], |row| {
            Ok(ItemTagRow {
                project_id: ProjectId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                item_id: ItemId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                tag_id: TagId::from_uuid(parse_uuid_col(2, row.get(2)?)?),
                created_at: parse_ts_col(3, row.get(3)?)?,
            })
        })
        .map_err(map_sqlite_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

pub(crate) fn insert_item_tag_row(
    tx: &Transaction<'_>,
    row: &ItemTagRow,
) -> Result<(), DomainError> {
    tx.execute(
        "INSERT INTO item_tags (project_id, item_id, tag_id, created_at)
         VALUES (?1, ?2, ?3, ?4);",
        params![
            uuid_to_sql(row.project_id.0),
            uuid_to_sql(row.item_id.0),
            uuid_to_sql(row.tag_id.0),
            ts_to_sql(row.created_at),
        ],
    )
    .map_err(map_sqlite_err)?;
    Ok(())
}

pub(crate) fn load_blocker_row(
    conn: &Connection,
    blocked_item_id: ItemId,
    blocker_feature_id: ItemId,
) -> Result<Option<ItemBlocker>, DomainError> {
    conn.query_row(
        "SELECT project_id, blocked_item_id, blocker_feature_id, created_at
         FROM item_blockers
         WHERE blocked_item_id = ?1 AND blocker_feature_id = ?2;",
        params![
            uuid_to_sql(blocked_item_id.0),
            uuid_to_sql(blocker_feature_id.0)
        ],
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
    .map_err(map_sqlite_err)
}

pub(crate) fn load_blockers_for_blocker_feature(
    conn: &Connection,
    blocker_feature_id: ItemId,
) -> Result<Vec<ItemBlocker>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT project_id, blocked_item_id, blocker_feature_id, created_at
             FROM item_blockers
             WHERE blocker_feature_id = ?1
             ORDER BY blocked_item_id ASC;",
        )
        .map_err(map_sqlite_err)?;
    let rows = stmt
        .query_map(params![uuid_to_sql(blocker_feature_id.0)], |row| {
            Ok(ItemBlocker {
                project_id: ProjectId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                blocked_item_id: ItemId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                blocker_feature_id: ItemId::from_uuid(parse_uuid_col(2, row.get(2)?)?),
                created_at: parse_ts_col(3, row.get(3)?)?,
            })
        })
        .map_err(map_sqlite_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

pub(crate) fn load_blockers_for_project(
    conn: &Connection,
    project_id: ProjectId,
) -> Result<Vec<ItemBlocker>, DomainError> {
    let mut stmt = conn
        .prepare(
            "SELECT project_id, blocked_item_id, blocker_feature_id, created_at
             FROM item_blockers
             WHERE project_id = ?1
             ORDER BY blocked_item_id ASC, blocker_feature_id ASC;",
        )
        .map_err(map_sqlite_err)?;
    let rows = stmt
        .query_map(params![uuid_to_sql(project_id.0)], |row| {
            Ok(ItemBlocker {
                project_id: ProjectId::from_uuid(parse_uuid_col(0, row.get(0)?)?),
                blocked_item_id: ItemId::from_uuid(parse_uuid_col(1, row.get(1)?)?),
                blocker_feature_id: ItemId::from_uuid(parse_uuid_col(2, row.get(2)?)?),
                created_at: parse_ts_col(3, row.get(3)?)?,
            })
        })
        .map_err(map_sqlite_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite_err)
}

pub(crate) fn insert_blocker_row(
    tx: &Transaction<'_>,
    row: &ItemBlocker,
) -> Result<(), DomainError> {
    tx.execute(
        "INSERT INTO item_blockers (project_id, blocked_item_id, blocker_feature_id, created_at)
         VALUES (?1, ?2, ?3, ?4);",
        params![
            uuid_to_sql(row.project_id.0),
            uuid_to_sql(row.blocked_item_id.0),
            uuid_to_sql(row.blocker_feature_id.0),
            ts_to_sql(row.created_at),
        ],
    )
    .map_err(map_sqlite_err)?;
    Ok(())
}

pub(crate) fn insert_exact_item_row(tx: &Transaction<'_>, item: &Item) -> Result<(), DomainError> {
    let (link_url, link_label) = match &item.link {
        Some(link) => (Some(link.url.as_str()), Some(link.label.as_str())),
        None => (None, None),
    };
    tx.execute(
        "INSERT INTO items (
            id, project_id, item_number, item_key, kind, parent_feature_id,
            title, description, status, priority, assignee_id, sort_order,
            deleted_at, trash_root_id, created_at, updated_at, link_url, link_label
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18);",
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
            item.deleted_at.map(ts_to_sql),
            item.trash_root_id.map(|id| uuid_to_sql(id.0)),
            ts_to_sql(item.created_at),
            ts_to_sql(item.updated_at),
            link_url,
            link_label,
        ],
    )
    .map_err(map_sqlite_err)?;
    Ok(())
}

pub(crate) fn insert_exact_project_row(
    tx: &Transaction<'_>,
    project: &Project,
) -> Result<(), DomainError> {
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
            ts_to_sql(project.created_at),
            ts_to_sql(project.updated_at),
        ],
    )
    .map_err(map_sqlite_err)?;
    Ok(())
}

pub(crate) fn insert_exact_assignee_row(
    tx: &Transaction<'_>,
    assignee: &Assignee,
) -> Result<(), DomainError> {
    tx.execute(
        "INSERT INTO assignees (
            id, name, handle, color, role, notes, archived_at, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9);",
        params![
            uuid_to_sql(assignee.id.0),
            &assignee.name,
            &assignee.handle,
            &assignee.color,
            &assignee.role,
            &assignee.notes,
            assignee.archived_at.map(ts_to_sql),
            ts_to_sql(assignee.created_at),
            ts_to_sql(assignee.updated_at),
        ],
    )
    .map_err(map_sqlite_err)?;
    Ok(())
}

pub(crate) fn insert_exact_tag_row(tx: &Transaction<'_>, tag: &Tag) -> Result<(), DomainError> {
    tx.execute(
        "INSERT INTO tags (id, project_id, name, color, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5);",
        params![
            uuid_to_sql(tag.id.0),
            uuid_to_sql(tag.project_id.0),
            &tag.name,
            &tag.color,
            ts_to_sql(tag.created_at),
        ],
    )
    .map_err(map_sqlite_err)?;
    Ok(())
}

pub(crate) fn set_item_updated_at(
    tx: &Transaction<'_>,
    item_id: ItemId,
    updated_at: DateTime<Utc>,
) -> Result<(), DomainError> {
    tx.execute(
        "UPDATE items SET updated_at = ?1 WHERE id = ?2;",
        params![ts_to_sql(updated_at), uuid_to_sql(item_id.0)],
    )
    .map_err(map_sqlite_err)?;
    Ok(())
}
