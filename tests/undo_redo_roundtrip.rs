use rusqlite::Connection;

use protrack4::commands::{
    AddBlocker, ArchiveAssignee, AttachWorkItemToFeature, ConvertFeatureToWorkItem,
    ConvertWorkItemToFeature, CreateFeature, CreateProject, CreateWorkItem, DeleteTag,
    DetachWorkItemFromFeature, EmptyTrash, MAX_UNDO_DEPTH, MoveWorkItemBetweenFeatures,
    PurgeTrashGroup, RemoveBlocker, ReorderFeatureWorkItems, ReorderSiblingFeatures,
    ReorderUnallocatedWorkItems, ReparentFeature, RestoreTrashGroup, SetItemTags,
    SetWorkItemFeatures, TrashItemSubtree, UndoManager, UndoableCommand, UpdateItemFields,
    UpdateProject, UpsertAssignee, UpsertTag,
};
use protrack4::db::inspect_and_migrate;
use protrack4::db::item_repo::{get_item, trash_item_subtree};
use protrack4::domain::{ItemId, ItemStatus, Priority, ProjectId, ProjectStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
struct DbSnapshotState {
    projects: Vec<Vec<String>>,
    assignees: Vec<Vec<String>>,
    tags: Vec<Vec<String>>,
    items: Vec<Vec<String>>,
    feature_work_items: Vec<Vec<String>>,
    item_tags: Vec<Vec<String>>,
    item_blockers: Vec<Vec<String>>,
}

fn query_rows(conn: &Connection, sql: &str, col_count: usize) -> Vec<Vec<String>> {
    let mut stmt = conn.prepare(sql).unwrap();
    let rows = stmt
        .query_map([], |row| {
            let mut cols = Vec::with_capacity(col_count);
            for ix in 0..col_count {
                let val: rusqlite::types::Value = row.get(ix)?;
                let formatted = match val {
                    rusqlite::types::Value::Null => "<NULL>".to_string(),
                    rusqlite::types::Value::Integer(n) => format!("I:{n}"),
                    rusqlite::types::Value::Real(r) => format!("R:{r}"),
                    rusqlite::types::Value::Text(t) => format!("T:{t}"),
                    rusqlite::types::Value::Blob(b) => format!("B:{}", b.len()),
                };
                cols.push(formatted);
            }
            Ok(cols)
        })
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

fn capture_db_state(conn: &Connection) -> DbSnapshotState {
    conn.execute(
        "INSERT INTO items_fts(items_fts) VALUES ('integrity-check');",
        [],
    )
    .expect("FTS5 integrity check must pass");

    DbSnapshotState {
        projects: query_rows(
            conn,
            "SELECT id, name, key_prefix, next_item_seq, description, color, status, sort_order, created_at, updated_at
             FROM projects ORDER BY id ASC;",
            10,
        ),
        assignees: query_rows(
            conn,
            "SELECT id, name, handle, color, role, notes, archived_at, created_at, updated_at
             FROM assignees ORDER BY id ASC;",
            9,
        ),
        tags: query_rows(
            conn,
            "SELECT id, project_id, name, color, created_at
             FROM tags ORDER BY id ASC;",
            5,
        ),
        items: query_rows(
            conn,
            "SELECT id, project_id, item_number, item_key, kind, parent_feature_id,
                    title, description, status, priority, assignee_id, sort_order,
                    deleted_at, trash_root_id, created_at, updated_at
             FROM items ORDER BY id ASC;",
            16,
        ),
        feature_work_items: query_rows(
            conn,
            "SELECT project_id, feature_id, work_item_id, sort_order, created_at
             FROM feature_work_items ORDER BY feature_id ASC, work_item_id ASC;",
            5,
        ),
        item_tags: query_rows(
            conn,
            "SELECT project_id, item_id, tag_id, created_at
             FROM item_tags ORDER BY item_id ASC, tag_id ASC;",
            4,
        ),
        item_blockers: query_rows(
            conn,
            "SELECT project_id, blocked_item_id, blocker_feature_id, created_at
             FROM item_blockers ORDER BY blocked_item_id ASC, blocker_feature_id ASC;",
            4,
        ),
    }
}

fn assert_round_trip(
    conn: &mut Connection,
    mgr: &mut UndoManager,
    name: &str,
    cmd: Box<dyn UndoableCommand>,
) {
    let s0 = capture_db_state(conn);
    mgr.execute(conn, cmd)
        .unwrap_or_else(|e| panic!("{name}: execute failed: {e}"));
    let s1 = capture_db_state(conn);

    let undo_eff = mgr
        .undo(conn)
        .unwrap_or_else(|e| panic!("{name}: undo failed: {e}"))
        .expect("undo must yield effect");
    assert!(
        undo_eff
            .toast
            .as_deref()
            .is_some_and(|t| t.starts_with("Undid: ")),
        "{name}: unexpected undo toast: {:?}",
        undo_eff.toast
    );
    let s_after_undo = capture_db_state(conn);
    assert_eq!(s_after_undo, s0, "{name}: S0 != S after undo");

    let redo_eff = mgr
        .redo(conn)
        .unwrap_or_else(|e| panic!("{name}: redo failed: {e}"))
        .expect("redo must yield effect");
    assert!(
        redo_eff
            .toast
            .as_deref()
            .is_some_and(|t| t.starts_with("Redid: ")),
        "{name}: unexpected redo toast: {:?}",
        redo_eff.toast
    );
    let s_after_redo = capture_db_state(conn);
    assert_eq!(s_after_redo, s1, "{name}: S1 != S after redo");
}

#[test]
fn layer2_all_commands_round_trip_undo_and_redo_with_exact_equality() {
    let mut conn = Connection::open_in_memory().unwrap();
    inspect_and_migrate(&mut conn, None, "RoundTrip").unwrap();
    let mut mgr = UndoManager::new();

    // 1. CreateProject
    let pid: ProjectId = {
        let s0 = capture_db_state(&conn);
        let eff = mgr
            .execute(
                &mut conn,
                Box::new(CreateProject::new(
                    "Alpha",
                    "ALP",
                    "Primary",
                    "#6366F1",
                    ProjectStatus::Active,
                )),
            )
            .unwrap();
        let pid = eff.created_project_id.unwrap();
        let s1 = capture_db_state(&conn);
        mgr.undo(&mut conn).unwrap();
        assert_eq!(capture_db_state(&conn), s0, "CreateProject undo mismatch");
        mgr.redo(&mut conn).unwrap();
        assert_eq!(capture_db_state(&conn), s1, "CreateProject redo mismatch");
        pid
    };

    // Second project to also cover rebalance/multi-project states
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "CreateProject(Beta)",
        Box::new(CreateProject::new(
            "Beta",
            "BET",
            "",
            "#10B981",
            ProjectStatus::Active,
        )),
    );

    // 2. UpsertAssignee (create & update) + ArchiveAssignee
    let assignee_id = {
        let s0 = capture_db_state(&conn);
        let eff = mgr
            .execute(
                &mut conn,
                Box::new(UpsertAssignee::create(
                    "Mark Benson",
                    "MB",
                    "#6366F1",
                    Some("Founder".to_string()),
                    "Owner",
                )),
            )
            .unwrap();
        let aid = eff.created_assignee_id.unwrap();
        let s1 = capture_db_state(&conn);
        mgr.undo(&mut conn).unwrap();
        assert_eq!(capture_db_state(&conn), s0);
        mgr.redo(&mut conn).unwrap();
        assert_eq!(capture_db_state(&conn), s1);
        aid
    };
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "UpsertAssignee(update)",
        Box::new(UpsertAssignee::update(
            assignee_id,
            "Mark B.",
            "MB1",
            "#0EA5E9",
            Some("Lead".to_string()),
            "Updated notes",
        )),
    );
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "ArchiveAssignee(true)",
        Box::new(ArchiveAssignee::new(assignee_id, true)),
    );
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "ArchiveAssignee(false)",
        Box::new(ArchiveAssignee::new(assignee_id, false)),
    );

    // 3. UpsertTag (create & update)
    let tag1_id = {
        let s0 = capture_db_state(&conn);
        let eff = mgr
            .execute(
                &mut conn,
                Box::new(UpsertTag::create(pid, "core", "#8B5CF6")),
            )
            .unwrap();
        let tid = eff.created_tag_id.unwrap();
        let s1 = capture_db_state(&conn);
        mgr.undo(&mut conn).unwrap();
        assert_eq!(capture_db_state(&conn), s0);
        mgr.redo(&mut conn).unwrap();
        assert_eq!(capture_db_state(&conn), s1);
        tid
    };
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "UpsertTag(update)",
        Box::new(UpsertTag::update(tag1_id, pid, "platform", "#EC4899")),
    );
    let tag2_id = mgr
        .execute(&mut conn, Box::new(UpsertTag::create(pid, "ui", "#0EA5E9")))
        .unwrap()
        .created_tag_id
        .unwrap();

    // 4. CreateFeature & CreateWorkItem
    let f1 = {
        let s0 = capture_db_state(&conn);
        let eff = mgr
            .execute(
                &mut conn,
                Box::new(
                    CreateFeature::new(pid, None, "Feature 1", "Body", ItemStatus::Todo, Priority::High)
                        .with_assignee(Some(assignee_id))
                        .with_tags(vec![tag1_id]),
                ),
            )
            .unwrap();
        let fid = eff.created_item_id.unwrap();
        let s1 = capture_db_state(&conn);
        mgr.undo(&mut conn).unwrap();
        assert_eq!(capture_db_state(&conn), s0);
        mgr.redo(&mut conn).unwrap();
        assert_eq!(capture_db_state(&conn), s1);
        fid
    };
    let f2 = mgr
        .execute(
            &mut conn,
            Box::new(CreateFeature::new(
                pid,
                None,
                "Feature 2",
                "",
                ItemStatus::Todo,
                Priority::Medium,
            )),
        )
        .unwrap()
        .created_item_id
        .unwrap();
    let f3 = mgr
        .execute(
            &mut conn,
            Box::new(CreateFeature::new(
                pid,
                Some(f1),
                "Sub Feature 3",
                "",
                ItemStatus::Todo,
                Priority::Low,
            )),
        )
        .unwrap()
        .created_item_id
        .unwrap();

    let wi1 = {
        let s0 = capture_db_state(&conn);
        let eff = mgr
            .execute(
                &mut conn,
                Box::new(
                    CreateWorkItem::new(
                        pid,
                        "Work Item 1",
                        "Details",
                        ItemStatus::Todo,
                        Priority::High,
                        vec![f1],
                    )
                    .with_assignee(Some(assignee_id))
                    .with_tags(vec![tag1_id, tag2_id]),
                ),
            )
            .unwrap();
        let wid = eff.created_item_id.unwrap();
        let s1 = capture_db_state(&conn);
        mgr.undo(&mut conn).unwrap();
        assert_eq!(capture_db_state(&conn), s0);
        mgr.redo(&mut conn).unwrap();
        assert_eq!(capture_db_state(&conn), s1);
        wid
    };
    let wi2 = mgr
        .execute(
            &mut conn,
            Box::new(CreateWorkItem::new(
                pid,
                "Work Item 2",
                "",
                ItemStatus::Backlog,
                Priority::None,
                vec![f1],
            )),
        )
        .unwrap()
        .created_item_id
        .unwrap();
    let u1 = mgr
        .execute(
            &mut conn,
            Box::new(CreateWorkItem::new(
                pid,
                "Unallocated 1",
                "",
                ItemStatus::Todo,
                Priority::Low,
                vec![],
            )),
        )
        .unwrap()
        .created_item_id
        .unwrap();
    let u2 = mgr
        .execute(
            &mut conn,
            Box::new(CreateWorkItem::new(
                pid,
                "Unallocated 2",
                "",
                ItemStatus::Todo,
                Priority::Medium,
                vec![],
            )),
        )
        .unwrap()
        .created_item_id
        .unwrap();

    // 5. UpdateItemFields & SetItemTags
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "UpdateItemFields",
        Box::new(
            UpdateItemFields::new(wi1)
                .with_title("Work Item 1 Renamed")
                .with_description("## Updated")
                .with_status(ItemStatus::InProgress)
                .with_priority(Priority::Urgent)
                .with_assignee_id(None),
        ),
    );
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "SetItemTags",
        Box::new(SetItemTags::new(wi1, vec![tag2_id])),
    );

    // 6. DeleteTag (with item_tags restoration!)
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "DeleteTag",
        Box::new(DeleteTag::new(tag1_id)),
    );

    // 7. ReorderSiblingFeatures, ReparentFeature, ReorderFeatureWorkItems, ReorderUnallocatedWorkItems
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "ReorderSiblingFeatures",
        Box::new(ReorderSiblingFeatures::new(pid, f2, None, Some(f1))),
    );
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "ReparentFeature",
        Box::new(ReparentFeature::new(f3, Some(f2))),
    );
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "ReorderFeatureWorkItems",
        Box::new(ReorderFeatureWorkItems::new(pid, f1, wi2, None, Some(wi1))),
    );
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "ReorderUnallocatedWorkItems",
        Box::new(ReorderUnallocatedWorkItems::new(pid, u2, None, Some(u1))),
    );

    // 8. AttachWorkItemToFeature, DetachWorkItemFromFeature, SetWorkItemFeatures, MoveWorkItemBetweenFeatures
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "AttachWorkItemToFeature",
        Box::new(AttachWorkItemToFeature::new(wi1, f2)),
    );
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "DetachWorkItemFromFeature",
        Box::new(DetachWorkItemFromFeature::new(wi1, f2)),
    );
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "SetWorkItemFeatures",
        Box::new(SetWorkItemFeatures::new(wi1, vec![f1, f2])),
    );
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "MoveWorkItemBetweenFeatures",
        Box::new(MoveWorkItemBetweenFeatures::new(wi2, f1, f2)),
    );

    // 9. AddBlocker & RemoveBlocker
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "AddBlocker",
        Box::new(AddBlocker::new(f1, f2)),
    );
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "RemoveBlocker",
        Box::new(RemoveBlocker::new(f1, f2)),
    );

    // 10. UpdateProject (with atomic key_prefix rewrite!)
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "UpdateProject(key_prefix rewrite)",
        Box::new(UpdateProject::new(
            pid,
            "Alpha v2",
            "CORE",
            "Rewritten keys",
            "#F59E0B",
            ProjectStatus::Paused,
        )),
    );

    // 11. ConvertWorkItemToFeature & ConvertFeatureToWorkItem (both unallocated & child, plus stale trashed links!)
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "ConvertWorkItemToFeature(unallocated)",
        Box::new(ConvertWorkItemToFeature::new(u1)),
    );
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "ConvertFeatureToWorkItem(root)",
        Box::new(ConvertFeatureToWorkItem::new(u1)),
    );

    // Create a stale trashed child under f_leaf, trash it, then convert f_leaf -> WI -> undo!
    let f_leaf = mgr
        .execute(
            &mut conn,
            Box::new(CreateFeature::new(
                pid,
                Some(f2),
                "Leaf For Conversion",
                "",
                ItemStatus::Todo,
                Priority::None,
            )),
        )
        .unwrap()
        .created_item_id
        .unwrap();
    let stale_child = mgr
        .execute(
            &mut conn,
            Box::new(CreateFeature::new(
                pid,
                Some(f_leaf),
                "Stale Trashed Child",
                "",
                ItemStatus::Todo,
                Priority::None,
            )),
        )
        .unwrap()
        .created_item_id
        .unwrap();
    mgr.execute(&mut conn, Box::new(TrashItemSubtree::new(stale_child)))
        .unwrap();

    assert_round_trip(
        &mut conn,
        &mut mgr,
        "ConvertFeatureToWorkItem(with stale trashed child)",
        Box::new(ConvertFeatureToWorkItem::new(f_leaf)),
    );
    assert_eq!(get_item(&conn, f_leaf).unwrap().parent_feature_id, None);
    // Convert u2 attached to f2 -> Feature -> undo
    mgr.execute(&mut conn, Box::new(AttachWorkItemToFeature::new(u2, f2)))
        .unwrap();
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "ConvertWorkItemToFeature(attached)",
        Box::new(ConvertWorkItemToFeature::new(u2)),
    );

    // 12. TrashItemSubtree & RestoreTrashGroup (including shared Work Items & pruned blocker restoration!)
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "TrashItemSubtree(with shared WI)",
        Box::new(TrashItemSubtree::new(f1)),
    );
    // f1 is now trashed; also add a blocker while f1 was active? Let's test RestoreTrashGroup with root promotion + blocker pruning:
    // Create blocker relationship f2 -> f_promo_parent? No, test restoring f1 directly:
    assert_round_trip(
        &mut conn,
        &mut mgr,
        "RestoreTrashGroup",
        Box::new(RestoreTrashGroup::new(f1)),
    );

    // Test chained multi-step undo -> redo across dependent commands
    let depth_before = mgr.status().undo_len;
    assert!(depth_before > 10);
    let state_final = capture_db_state(&conn);
    let mut undone = 0usize;
    while mgr.undo(&mut conn).unwrap().is_some() {
        undone += 1;
    }
    assert_eq!(undone, depth_before);
    assert_eq!(
        capture_db_state(&conn),
        DbSnapshotState {
            projects: vec![],
            assignees: vec![],
            tags: vec![],
            items: vec![],
            feature_work_items: vec![],
            item_tags: vec![],
            item_blockers: vec![],
        },
        "Undoing the entire session must return DB to completely empty state"
    );
    while mgr.redo(&mut conn).unwrap().is_some() {}
    assert_eq!(
        capture_db_state(&conn),
        state_final,
        "Redoing the entire session must reconstruct exact final DB state"
    );

    // 13. PurgeTrashGroup & EmptyTrash clear Undo/Redo stacks
    mgr.execute(&mut conn, Box::new(TrashItemSubtree::new(f1)))
        .unwrap();
    assert!(mgr.status().can_undo);
    mgr.execute(&mut conn, Box::new(PurgeTrashGroup::new(f1)))
        .unwrap();
    assert!(!mgr.status().can_undo);
    assert!(!mgr.status().can_redo);

    mgr.execute(&mut conn, Box::new(TrashItemSubtree::new(f2)))
        .unwrap();
    assert!(mgr.status().can_undo);
    mgr.execute(&mut conn, Box::new(EmptyTrash::new(pid)))
        .unwrap();
    assert!(!mgr.status().can_undo);
    assert!(!mgr.status().can_redo);

    // Confirm trash_item_subtree helper import stays valid for direct stale-link scenarios
    let _ = trash_item_subtree;
}

#[test]
fn undo_manager_enforces_max_depth_and_clears_redo_on_new_command() {
    let mut conn = Connection::open_in_memory().unwrap();
    inspect_and_migrate(&mut conn, None, "Capacity").unwrap();
    let mut mgr = UndoManager::new();
    assert_eq!(MAX_UNDO_DEPTH, 100);

    let pid = mgr
        .execute(
            &mut conn,
            Box::new(CreateProject::new(
                "Cap",
                "CAP",
                "",
                "#6366F1",
                ProjectStatus::Active,
            )),
        )
        .unwrap()
        .created_project_id
        .unwrap();

    for i in 0..110 {
        mgr.execute(
            &mut conn,
            Box::new(CreateWorkItem::new(
                pid,
                format!("Item {i}"),
                "",
                ItemStatus::Todo,
                Priority::None,
                vec![],
            )),
        )
        .unwrap();
    }

    assert_eq!(mgr.status().undo_len, MAX_UNDO_DEPTH);

    mgr.undo(&mut conn).unwrap().unwrap();
    assert!(mgr.status().can_redo);

    // Executing a new command clears redo stack
    mgr.execute(
        &mut conn,
        Box::new(CreateWorkItem::new(
            pid,
            "Fresh branch",
            "",
            ItemStatus::Todo,
            Priority::High,
            vec![],
        )),
    )
    .unwrap();
    assert!(!mgr.status().can_redo);
    assert_eq!(mgr.status().undo_len, MAX_UNDO_DEPTH);
}

#[test]
fn restore_trash_group_undo_restores_pruned_blockers_and_parent() {
    let mut conn = Connection::open_in_memory().unwrap();
    inspect_and_migrate(&mut conn, None, "RestoreUndo").unwrap();
    let mut mgr = UndoManager::new();

    let pid = mgr
        .execute(
            &mut conn,
            Box::new(CreateProject::new(
                "RestoreUndo",
                "RST",
                "",
                "#6366F1",
                ProjectStatus::Active,
            )),
        )
        .unwrap()
        .created_project_id
        .unwrap();

    let f_parent = mgr
        .execute(
            &mut conn,
            Box::new(CreateFeature::new(
                pid,
                None,
                "Parent",
                "",
                ItemStatus::Todo,
                Priority::High,
            )),
        )
        .unwrap()
        .created_item_id
        .unwrap();
    let f_child = mgr
        .execute(
            &mut conn,
            Box::new(CreateFeature::new(
                pid,
                Some(f_parent),
                "Child",
                "",
                ItemStatus::Todo,
                Priority::Medium,
            )),
        )
        .unwrap()
        .created_item_id
        .unwrap();

    // Trash child, then while child is trashed add blocker f_parent -> f_child? No: f_parent cannot block on trashed item,
    // but while child was active: block f_other on f_child, trash f_child, reparent f_other under f_parent...
    // Actually: block f_child on f_other while active; trash f_child; reparent f_other under f_parent (or f_parent blocked on f_other);
    // simpler: block f_parent on f_other, trash f_child, then restore f_child after parent changed:
    let f_other = mgr
        .execute(
            &mut conn,
            Box::new(CreateFeature::new(
                pid,
                None,
                "Other",
                "",
                ItemStatus::Todo,
                Priority::Low,
            )),
        )
        .unwrap()
        .created_item_id
        .unwrap();

    // f_child blocked on f_other while active
    mgr.execute(&mut conn, Box::new(AddBlocker::new(f_child, f_other)))
        .unwrap();
    // Trash f_child
    mgr.execute(&mut conn, Box::new(TrashItemSubtree::new(f_child)))
        .unwrap();
    // Now move f_other under f_parent (while f_child is trashed) and convert/trash f_parent?
    // Move f_other under f_parent? Wait: if f_other is child of f_parent and f_child is child of f_parent, both are siblings, valid.
    // Instead: block f_other on f_parent while f_child is trashed -> wait: f_child has parent f_parent.
    // If we reparent f_parent under f_other while f_child is trashed, restoring f_child would make f_child descendant of f_other while blocked on f_other -> pruned!
    mgr.execute(&mut conn, Box::new(ReparentFeature::new(f_parent, Some(f_other))))
        .unwrap();

    let s0 = capture_db_state(&conn);
    let eff = mgr
        .execute(&mut conn, Box::new(RestoreTrashGroup::new(f_child)))
        .unwrap();
    assert!(
        eff.warning_toast
            .as_deref()
            .is_some_and(|w| w.contains("Pruned 1 conflicting blocker link"))
    );
    let s1 = capture_db_state(&conn);
    mgr.undo(&mut conn).unwrap();
    assert_eq!(capture_db_state(&conn), s0);
    mgr.redo(&mut conn).unwrap();
    assert_eq!(capture_db_state(&conn), s1);

    let _: ItemId = f_child;
}
