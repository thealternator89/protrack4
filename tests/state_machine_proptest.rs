use std::collections::{HashMap, HashSet};

use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use rusqlite::Connection;

use protrack4::commands::{
    AddBlocker, AttachWorkItemToFeature, ConvertFeatureToWorkItem, ConvertWorkItemToFeature,
    CreateFeature, CreateProject, CreateWorkItem, DetachWorkItemFromFeature, RemoveBlocker,
    ReparentFeature, RestoreTrashGroup, SetWorkItemFeatures, TrashItemSubtree, UndoManager,
    UpdateItemFields,
};
use protrack4::db::{inspect_and_migrate, load_project_snapshot};
use protrack4::domain::{
    ConversionEligibility, EffectiveBlockState, FeatureProgress, ItemId, ItemKind, ItemStatus,
    MAX_FEATURE_DEPTH, Priority, ProjectId, ProjectSnapshot, ProjectStatus,
};

#[derive(Debug, Clone)]
enum MachineOp {
    CreateFeature { project_idx: usize, parent_pick: usize },
    CreateWorkItem { project_idx: usize, feature_pick: Option<usize> },
    UpdateStatus { project_idx: usize, item_pick: usize, status_idx: usize },
    ReparentFeature { project_idx: usize, feat_pick: usize, new_parent_pick: Option<usize> },
    AttachWorkItem { project_idx: usize, wi_pick: usize, feat_pick: usize },
    DetachWorkItem { project_idx: usize, wi_pick: usize, feat_pick: usize },
    SetWorkItemFeatures { project_idx: usize, wi_pick: usize, feat_a: usize, feat_b: usize, count: usize },
    AddBlocker { project_idx: usize, item_pick: usize, blocker_pick: usize },
    RemoveBlocker { project_idx: usize, item_pick: usize, blocker_pick: usize },
    ConvertWiToFeature { project_idx: usize, wi_pick: usize },
    ConvertFeatureToWi { project_idx: usize, feat_pick: usize },
    TrashSubtree { project_idx: usize, item_pick: usize },
    RestoreTrash { project_idx: usize, group_pick: usize },
    Undo,
    Redo,
}

fn machine_op_strategy() -> impl Strategy<Value = MachineOp> {
    prop_oneof![
        3 => (0usize..2, 0usize..32).prop_map(|(project_idx, parent_pick)| MachineOp::CreateFeature { project_idx, parent_pick }),
        3 => (0usize..2, proptest::option::of(0usize..32)).prop_map(|(project_idx, feature_pick)| MachineOp::CreateWorkItem { project_idx, feature_pick }),
        2 => (0usize..2, 0usize..32, 0usize..5).prop_map(|(project_idx, item_pick, status_idx)| MachineOp::UpdateStatus { project_idx, item_pick, status_idx }),
        3 => (0usize..2, 0usize..32, proptest::option::of(0usize..32)).prop_map(|(project_idx, feat_pick, new_parent_pick)| MachineOp::ReparentFeature { project_idx, feat_pick, new_parent_pick }),
        3 => (0usize..2, 0usize..32, 0usize..32).prop_map(|(project_idx, wi_pick, feat_pick)| MachineOp::AttachWorkItem { project_idx, wi_pick, feat_pick }),
        2 => (0usize..2, 0usize..32, 0usize..32).prop_map(|(project_idx, wi_pick, feat_pick)| MachineOp::DetachWorkItem { project_idx, wi_pick, feat_pick }),
        2 => (0usize..2, 0usize..32, 0usize..32, 0usize..32, 0usize..3).prop_map(|(project_idx, wi_pick, feat_a, feat_b, count)| MachineOp::SetWorkItemFeatures { project_idx, wi_pick, feat_a, feat_b, count }),
        4 => (0usize..2, 0usize..32, 0usize..32).prop_map(|(project_idx, item_pick, blocker_pick)| MachineOp::AddBlocker { project_idx, item_pick, blocker_pick }),
        2 => (0usize..2, 0usize..32, 0usize..32).prop_map(|(project_idx, item_pick, blocker_pick)| MachineOp::RemoveBlocker { project_idx, item_pick, blocker_pick }),
        2 => (0usize..2, 0usize..32).prop_map(|(project_idx, wi_pick)| MachineOp::ConvertWiToFeature { project_idx, wi_pick }),
        2 => (0usize..2, 0usize..32).prop_map(|(project_idx, feat_pick)| MachineOp::ConvertFeatureToWi { project_idx, feat_pick }),
        2 => (0usize..2, 0usize..32).prop_map(|(project_idx, item_pick)| MachineOp::TrashSubtree { project_idx, item_pick }),
        2 => (0usize..2, 0usize..32).prop_map(|(project_idx, group_pick)| MachineOp::RestoreTrash { project_idx, group_pick }),
        4 => Just(MachineOp::Undo),
        2 => Just(MachineOp::Redo),
    ]
}

fn feature_ancestors(snap: &ProjectSnapshot, feature_id: ItemId) -> HashSet<ItemId> {
    let mut set = HashSet::new();
    let mut cur = snap.item(feature_id).and_then(|f| f.parent_feature_id);
    while let Some(pid) = cur {
        if !set.insert(pid) {
            break;
        }
        cur = snap.item(pid).and_then(|f| f.parent_feature_id);
    }
    set
}

fn reference_effective_block_state(snap: &ProjectSnapshot, item_id: ItemId) -> EffectiveBlockState {
    let item = snap.item(item_id).unwrap();
    let active_direct: Vec<ItemId> = snap
        .item_blockers
        .iter()
        .filter(|e| e.blocked_item_id == item_id)
        .filter_map(|e| snap.item(e.blocker_feature_id))
        .filter(|b| b.is_active() && !b.status.is_closed())
        .map(|b| b.id)
        .collect();

    if !active_direct.is_empty() {
        return EffectiveBlockState::DirectlyBlocked {
            active_count: active_direct.len(),
        };
    }

    let feature_is_blocked = |fid: ItemId| -> HashSet<ItemId> {
        let mut blockers = HashSet::new();
        let mut chain = vec![fid];
        chain.extend(feature_ancestors(snap, fid));
        for anc in chain {
            for e in &snap.item_blockers {
                if e.blocked_item_id == anc
                    && let Some(b) = snap.item(e.blocker_feature_id)
                    && b.is_active()
                    && !b.status.is_closed()
                {
                    blockers.insert(b.id);
                }
            }
        }
        blockers
    };

    match item.kind {
        ItemKind::Feature => {
            let mut anc_blockers = HashSet::new();
            for anc in feature_ancestors(snap, item_id) {
                for e in &snap.item_blockers {
                    if e.blocked_item_id == anc
                        && let Some(b) = snap.item(e.blocker_feature_id)
                        && b.is_active()
                        && !b.status.is_closed()
                    {
                        anc_blockers.insert(b.id);
                    }
                }
            }
            if anc_blockers.is_empty() {
                EffectiveBlockState::Unblocked
            } else {
                EffectiveBlockState::InheritedBlocked {
                    ancestor_blocker_count: anc_blockers.len(),
                }
            }
        }
        ItemKind::WorkItem => {
            let attached = snap.features_for_work_item(item_id);
            if attached.is_empty() {
                return EffectiveBlockState::Unblocked;
            }
            let mut blocked_features = 0usize;
            let mut distinct_blockers = HashSet::new();
            for f in &attached {
                let fb = feature_is_blocked(f.id);
                if !fb.is_empty() {
                    blocked_features += 1;
                    distinct_blockers.extend(fb);
                }
            }
            if blocked_features == 0 {
                EffectiveBlockState::Unblocked
            } else if blocked_features == attached.len() {
                EffectiveBlockState::InheritedBlocked {
                    ancestor_blocker_count: distinct_blockers.len().max(1),
                }
            } else {
                EffectiveBlockState::PartiallyBlocked {
                    blocked_features,
                    total_features: attached.len(),
                }
            }
        }
    }
}

fn assert_all_invariants(conn: &Connection, project_ids: &[ProjectId]) {
    conn.execute(
        "INSERT INTO items_fts(items_fts) VALUES ('integrity-check');",
        [],
    )
    .expect("FTS5 integrity check failed");

    // Cross-project relational check directly on SQLite tables (INV-01)
    let cross_parent: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM items c JOIN items p ON p.id = c.parent_feature_id
             WHERE c.project_id != p.project_id;",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cross_parent, 0, "INV-01 cross-project parent detected");

    let cross_fwi: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM feature_work_items fwi
             JOIN items f ON f.id = fwi.feature_id
             JOIN items w ON w.id = fwi.work_item_id
             WHERE fwi.project_id != f.project_id OR fwi.project_id != w.project_id;",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cross_fwi, 0, "INV-01 cross-project fwi detected");

    let cross_blocker: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM item_blockers ib
             JOIN items b ON b.id = ib.blocked_item_id
             JOIN items r ON r.id = ib.blocker_feature_id
             WHERE ib.project_id != b.project_id OR ib.project_id != r.project_id;",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cross_blocker, 0, "INV-01 cross-project blocker detected");

    for &pid in project_ids {
        let snap = load_project_snapshot(conn, pid).unwrap();
        let active_features: Vec<_> = snap
            .active_items()
            .into_iter()
            .filter(|i| i.kind == ItemKind::Feature)
            .collect();
        let feature_ids: HashSet<ItemId> = active_features.iter().map(|f| f.id).collect();

        // INV-02: Feature depth < MAX_FEATURE_DEPTH
        for f in &active_features {
            assert!(
                snap.feature_depth(f.id) < MAX_FEATURE_DEPTH,
                "INV-02 depth exceeded for {}",
                f.item_key
            );
        }

        // INV-05: Acyclicity of G = hierarchy U feature_blockers
        let mut adj: HashMap<ItemId, Vec<ItemId>> = HashMap::new();
        for &fid in &feature_ids {
            adj.entry(fid).or_default();
        }
        for f in &active_features {
            if let Some(parent_id) = f.parent_feature_id {
                adj.entry(f.id).or_default().push(parent_id);
            }
        }
        for edge in &snap.item_blockers {
            if feature_ids.contains(&edge.blocked_item_id)
                && feature_ids.contains(&edge.blocker_feature_id)
            {
                adj.entry(edge.blocked_item_id)
                    .or_default()
                    .push(edge.blocker_feature_id);
            }
        }
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Color {
            White,
            Gray,
            Black,
        }
        let mut color: HashMap<ItemId, Color> =
            feature_ids.iter().map(|&id| (id, Color::White)).collect();
        fn has_cycle(
            node: ItemId,
            adj: &HashMap<ItemId, Vec<ItemId>>,
            color: &mut HashMap<ItemId, Color>,
        ) -> bool {
            color.insert(node, Color::Gray);
            if let Some(nexts) = adj.get(&node) {
                for &next in nexts {
                    match color.get(&next).copied().unwrap_or(Color::White) {
                        Color::Gray => return true,
                        Color::White => {
                            if has_cycle(next, adj, color) {
                                return true;
                            }
                        }
                        Color::Black => {}
                    }
                }
            }
            color.insert(node, Color::Black);
            false
        }
        for &fid in &feature_ids {
            if color.get(&fid) == Some(&Color::White) {
                assert!(!has_cycle(fid, &adj, &mut color), "INV-05 cycle detected");
            }
        }

        // INV-04 / INV-05 exclusions for active blockers
        for edge in &snap.item_blockers {
            let blocked = snap.item(edge.blocked_item_id).unwrap();
            if let Some(blocker) = snap.item(edge.blocker_feature_id) {
                assert_eq!(blocker.kind, ItemKind::Feature, "INV-04 violation");
                if blocked.kind == ItemKind::Feature {
                    assert!(
                        !feature_ancestors(&snap, blocked.id).contains(&blocker.id),
                        "INV-05 ancestor blocker"
                    );
                    assert!(
                        !snap.descendant_feature_ids(blocked.id).contains(&blocker.id),
                        "INV-05 descendant blocker"
                    );
                } else {
                    for f in snap.features_for_work_item(blocked.id) {
                        assert_ne!(f.id, blocker.id, "INV-05 WI attached blocker");
                        assert!(
                            !feature_ancestors(&snap, f.id).contains(&blocker.id),
                            "INV-05 WI ancestor blocker"
                        );
                    }
                }
            }
        }

        // INV-06, INV-08, INV-09, INV-11 snapshot vs SQLite consistency
        for item in snap.active_items() {
            assert_eq!(
                snap.effective_block_state(item.id),
                reference_effective_block_state(&snap, item.id),
                "INV-06 mismatch for {}",
                item.item_key
            );

            if item.kind == ItemKind::Feature {
                let expected_prog: FeatureProgress = snap.compute_feature_progress(item.id);
                assert_eq!(
                    snap.feature_progress_detail(item.id),
                    expected_prog,
                    "INV-11 rollup mismatch for {}",
                    item.item_key
                );
                let live_elig =
                    protrack4::db::item_repo::check_feature_to_work_item_eligibility(conn, item.id)
                        .unwrap();
                let snap_elig = snap.conversion_eligibility(item.id).unwrap();
                assert_eq!(
                    matches!(snap_elig, ConversionEligibility::Allowed { .. }),
                    matches!(live_elig, ConversionEligibility::Allowed { .. }),
                    "INV-09 eligibility mismatch for {}",
                    item.item_key
                );
            } else {
                let live_elig =
                    protrack4::db::item_repo::check_work_item_to_feature_eligibility(conn, item.id)
                        .unwrap();
                let snap_elig = snap.conversion_eligibility(item.id).unwrap();
                assert_eq!(
                    matches!(snap_elig, ConversionEligibility::Allowed { .. }),
                    matches!(live_elig, ConversionEligibility::Allowed { .. }),
                    "INV-08 eligibility mismatch for {}",
                    item.item_key
                );
            }
        }
    }
}

fn setup_two_project_db() -> (Connection, UndoManager, [ProjectId; 2]) {
    let mut conn = Connection::open_in_memory().unwrap();
    inspect_and_migrate(&mut conn, None, "StateMachineFuzz").unwrap();
    let mut mgr = UndoManager::new();

    let pa = mgr
        .execute(
            &mut conn,
            Box::new(CreateProject::new(
                "Alpha",
                "ALP",
                "",
                "#6366F1",
                ProjectStatus::Active,
            )),
        )
        .unwrap()
        .created_project_id
        .unwrap();
    let pb = mgr
        .execute(
            &mut conn,
            Box::new(CreateProject::new(
                "Beta",
                "BET",
                "",
                "#10B981",
                ProjectStatus::Active,
            )),
        )
        .unwrap()
        .created_project_id
        .unwrap();

    for pid in [pa, pb] {
        let f1 = mgr
            .execute(
                &mut conn,
                Box::new(CreateFeature::new(
                    pid,
                    None,
                    "Seed F1",
                    "",
                    ItemStatus::Todo,
                    Priority::High,
                )),
            )
            .unwrap()
            .created_item_id
            .unwrap();
        let f2 = mgr
            .execute(
                &mut conn,
                Box::new(CreateFeature::new(
                    pid,
                    Some(f1),
                    "Seed F2",
                    "",
                    ItemStatus::Todo,
                    Priority::Medium,
                )),
            )
            .unwrap()
            .created_item_id
            .unwrap();
        mgr.execute(
            &mut conn,
            Box::new(CreateWorkItem::new(
                pid,
                "Seed W1",
                "",
                ItemStatus::Todo,
                Priority::Low,
                vec![f2],
            )),
        )
        .unwrap();
    }

    (conn, mgr, [pa, pb])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(14))]
    #[test]
    fn prop_randomized_state_machine_operations_and_undo_redo_preserve_all_invariants(
        ops in proptest::collection::vec(machine_op_strategy(), 40..50)
    ) {
        let (mut conn, mut mgr, pids) = setup_two_project_db();
        let statuses = [
            ItemStatus::Backlog,
            ItemStatus::Todo,
            ItemStatus::InProgress,
            ItemStatus::Done,
            ItemStatus::Cancelled,
        ];

        for (step, op) in ops.into_iter().enumerate() {
            let pick_pid = |idx: usize| pids[idx % pids.len()];
            match op {
                MachineOp::Undo => {
                    let _ = mgr.undo(&mut conn);
                }
                MachineOp::Redo => {
                    let _ = mgr.redo(&mut conn);
                }
                MachineOp::CreateFeature { project_idx, parent_pick } => {
                    let pid = pick_pid(project_idx);
                    let snap = load_project_snapshot(&conn, pid).unwrap();
                    let features: Vec<_> = snap.active_items().into_iter().filter(|i| i.kind == ItemKind::Feature).map(|i| i.id).collect();
                    let parent = if features.is_empty() || parent_pick % 4 == 0 {
                        None
                    } else {
                        Some(features[parent_pick % features.len()])
                    };
                    let _ = mgr.execute(
                        &mut conn,
                        Box::new(CreateFeature::new(
                            pid,
                            parent,
                            format!("F-{step}"),
                            "",
                            ItemStatus::Todo,
                            Priority::Medium,
                        )),
                    );
                }
                MachineOp::CreateWorkItem { project_idx, feature_pick } => {
                    let pid = pick_pid(project_idx);
                    let snap = load_project_snapshot(&conn, pid).unwrap();
                    let features: Vec<_> = snap.active_items().into_iter().filter(|i| i.kind == ItemKind::Feature).map(|i| i.id).collect();
                    let fids = match (feature_pick, features.is_empty()) {
                        (Some(ix), false) => vec![features[ix % features.len()]],
                        _ => Vec::new(),
                    };
                    let _ = mgr.execute(
                        &mut conn,
                        Box::new(CreateWorkItem::new(
                            pid,
                            format!("W-{step}"),
                            "",
                            ItemStatus::Todo,
                            Priority::None,
                            fids,
                        )),
                    );
                }
                MachineOp::UpdateStatus { project_idx, item_pick, status_idx } => {
                    let pid = pick_pid(project_idx);
                    let snap = load_project_snapshot(&conn, pid).unwrap();
                    let items = snap.active_items();
                    if !items.is_empty() {
                        let id = items[item_pick % items.len()].id;
                        let _ = mgr.execute(
                            &mut conn,
                            Box::new(UpdateItemFields::new(id).with_status(statuses[status_idx % statuses.len()])),
                        );
                    }
                }
                MachineOp::ReparentFeature { project_idx, feat_pick, new_parent_pick } => {
                    let pid = pick_pid(project_idx);
                    let snap = load_project_snapshot(&conn, pid).unwrap();
                    let features: Vec<_> = snap.active_items().into_iter().filter(|i| i.kind == ItemKind::Feature).map(|i| i.id).collect();
                    if !features.is_empty() {
                        let fid = features[feat_pick % features.len()];
                        let new_parent = new_parent_pick.map(|ix| features[ix % features.len()]);
                        let _ = mgr.execute(&mut conn, Box::new(ReparentFeature::new(fid, new_parent)));
                    }
                }
                MachineOp::AttachWorkItem { project_idx, wi_pick, feat_pick } => {
                    let pid = pick_pid(project_idx);
                    let snap = load_project_snapshot(&conn, pid).unwrap();
                    let wis: Vec<_> = snap.active_items().into_iter().filter(|i| i.kind == ItemKind::WorkItem).map(|i| i.id).collect();
                    let features: Vec<_> = snap.active_items().into_iter().filter(|i| i.kind == ItemKind::Feature).map(|i| i.id).collect();
                    if !wis.is_empty() && !features.is_empty() {
                        let _ = mgr.execute(
                            &mut conn,
                            Box::new(AttachWorkItemToFeature::new(
                                wis[wi_pick % wis.len()],
                                features[feat_pick % features.len()],
                            )),
                        );
                    }
                }
                MachineOp::DetachWorkItem { project_idx, wi_pick, feat_pick } => {
                    let pid = pick_pid(project_idx);
                    let snap = load_project_snapshot(&conn, pid).unwrap();
                    let wis: Vec<_> = snap.active_items().into_iter().filter(|i| i.kind == ItemKind::WorkItem).map(|i| i.id).collect();
                    let features: Vec<_> = snap.active_items().into_iter().filter(|i| i.kind == ItemKind::Feature).map(|i| i.id).collect();
                    if !wis.is_empty() && !features.is_empty() {
                        let _ = mgr.execute(
                            &mut conn,
                            Box::new(DetachWorkItemFromFeature::new(
                                wis[wi_pick % wis.len()],
                                features[feat_pick % features.len()],
                            )),
                        );
                    }
                }
                MachineOp::SetWorkItemFeatures { project_idx, wi_pick, feat_a, feat_b, count } => {
                    let pid = pick_pid(project_idx);
                    let snap = load_project_snapshot(&conn, pid).unwrap();
                    let wis: Vec<_> = snap.active_items().into_iter().filter(|i| i.kind == ItemKind::WorkItem).map(|i| i.id).collect();
                    let features: Vec<_> = snap.active_items().into_iter().filter(|i| i.kind == ItemKind::Feature).map(|i| i.id).collect();
                    if !wis.is_empty() {
                        let wi_id = wis[wi_pick % wis.len()];
                        let fids = if features.is_empty() || count == 0 {
                            vec![]
                        } else if count == 1 {
                            vec![features[feat_a % features.len()]]
                        } else {
                            vec![features[feat_a % features.len()], features[feat_b % features.len()]]
                        };
                        let _ = mgr.execute(&mut conn, Box::new(SetWorkItemFeatures::new(wi_id, fids)));
                    }
                }
                MachineOp::AddBlocker { project_idx, item_pick, blocker_pick } => {
                    let pid = pick_pid(project_idx);
                    let snap = load_project_snapshot(&conn, pid).unwrap();
                    let items = snap.active_items();
                    let features: Vec<_> = items.iter().filter(|i| i.kind == ItemKind::Feature).map(|i| i.id).collect();
                    if !items.is_empty() && !features.is_empty() {
                        let _ = mgr.execute(
                            &mut conn,
                            Box::new(AddBlocker::new(
                                items[item_pick % items.len()].id,
                                features[blocker_pick % features.len()],
                            )),
                        );
                    }
                }
                MachineOp::RemoveBlocker { project_idx, item_pick, blocker_pick } => {
                    let pid = pick_pid(project_idx);
                    let snap = load_project_snapshot(&conn, pid).unwrap();
                    let items = snap.active_items();
                    let features: Vec<_> = items.iter().filter(|i| i.kind == ItemKind::Feature).map(|i| i.id).collect();
                    if !items.is_empty() && !features.is_empty() {
                        let _ = mgr.execute(
                            &mut conn,
                            Box::new(RemoveBlocker::new(
                                items[item_pick % items.len()].id,
                                features[blocker_pick % features.len()],
                            )),
                        );
                    }
                }
                MachineOp::ConvertWiToFeature { project_idx, wi_pick } => {
                    let pid = pick_pid(project_idx);
                    let snap = load_project_snapshot(&conn, pid).unwrap();
                    let wis: Vec<_> = snap.active_items().into_iter().filter(|i| i.kind == ItemKind::WorkItem).map(|i| i.id).collect();
                    if !wis.is_empty() {
                        let _ = mgr.execute(
                            &mut conn,
                            Box::new(ConvertWorkItemToFeature::new(wis[wi_pick % wis.len()])),
                        );
                    }
                }
                MachineOp::ConvertFeatureToWi { project_idx, feat_pick } => {
                    let pid = pick_pid(project_idx);
                    let snap = load_project_snapshot(&conn, pid).unwrap();
                    let features: Vec<_> = snap.active_items().into_iter().filter(|i| i.kind == ItemKind::Feature).map(|i| i.id).collect();
                    if !features.is_empty() {
                        let _ = mgr.execute(
                            &mut conn,
                            Box::new(ConvertFeatureToWorkItem::new(features[feat_pick % features.len()])),
                        );
                    }
                }
                MachineOp::TrashSubtree { project_idx, item_pick } => {
                    let pid = pick_pid(project_idx);
                    let snap = load_project_snapshot(&conn, pid).unwrap();
                    let items = snap.active_items();
                    if !items.is_empty() {
                        let _ = mgr.execute(
                            &mut conn,
                            Box::new(TrashItemSubtree::new(items[item_pick % items.len()].id)),
                        );
                    }
                }
                MachineOp::RestoreTrash { project_idx, group_pick } => {
                    let pid = pick_pid(project_idx);
                    let snap = load_project_snapshot(&conn, pid).unwrap();
                    if !snap.trash_groups.is_empty() {
                        let root_id = snap.trash_groups[group_pick % snap.trash_groups.len()].root.id;
                        let _ = mgr.execute(&mut conn, Box::new(RestoreTrashGroup::new(root_id)));
                    }
                }
            }

            assert_all_invariants(&conn, &pids);
        }
    }
}
