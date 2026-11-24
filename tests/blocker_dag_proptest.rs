use std::collections::{HashMap, HashSet};

use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use rusqlite::{Connection, TransactionBehavior};

use protrack4::db::blocker_repo::{add_blocker, remove_blocker};
use protrack4::db::item_repo::{
    CreateFeatureInput, CreateWorkItemInput, UpdateItemFieldsInput, attach_work_item_to_feature,
    create_feature, create_work_item, detach_work_item_from_feature, reparent_feature,
    update_item_fields,
};
use protrack4::db::project_repo::{CreateProjectInput, create_project};
use protrack4::db::{inspect_and_migrate, load_project_snapshot};
use protrack4::domain::{
    EffectiveBlockState, ItemId, ItemKind, ItemStatus, Priority, ProjectId, ProjectSnapshot,
    ProjectStatus,
};

#[derive(Debug, Clone)]
enum DagOp {
    CreateFeature { parent_pick: usize },
    CreateWorkItem { feature_pick: Option<usize> },
    ReparentFeature { feat_pick: usize, new_parent_pick: Option<usize> },
    AttachWorkItem { wi_pick: usize, feat_pick: usize },
    DetachWorkItem { wi_pick: usize, feat_pick: usize },
    AddBlocker { item_pick: usize, blocker_pick: usize },
    RemoveBlocker { item_pick: usize, blocker_pick: usize },
    SetStatus { item_pick: usize, status_idx: usize },
}

fn op_strategy() -> impl Strategy<Value = DagOp> {
    prop_oneof![
        3 => (0usize..32).prop_map(|parent_pick| DagOp::CreateFeature { parent_pick }),
        2 => proptest::option::of(0usize..32).prop_map(|feature_pick| DagOp::CreateWorkItem { feature_pick }),
        3 => (0usize..32, proptest::option::of(0usize..32))
            .prop_map(|(feat_pick, new_parent_pick)| DagOp::ReparentFeature { feat_pick, new_parent_pick }),
        3 => (0usize..32, 0usize..32).prop_map(|(wi_pick, feat_pick)| DagOp::AttachWorkItem { wi_pick, feat_pick }),
        2 => (0usize..32, 0usize..32).prop_map(|(wi_pick, feat_pick)| DagOp::DetachWorkItem { wi_pick, feat_pick }),
        5 => (0usize..32, 0usize..32).prop_map(|(item_pick, blocker_pick)| DagOp::AddBlocker { item_pick, blocker_pick }),
        2 => (0usize..32, 0usize..32).prop_map(|(item_pick, blocker_pick)| DagOp::RemoveBlocker { item_pick, blocker_pick }),
        3 => (0usize..32, 0usize..5).prop_map(|(item_pick, status_idx)| DagOp::SetStatus { item_pick, status_idx }),
    ]
}

fn assert_combined_graph_acyclic_and_exclusions_hold(snap: &ProjectSnapshot) {
    let active_features: Vec<_> = snap
        .active_items()
        .into_iter()
        .filter(|i| i.kind == ItemKind::Feature)
        .collect();
    let feature_ids: HashSet<ItemId> = active_features.iter().map(|f| f.id).collect();

    let mut adj: HashMap<ItemId, Vec<ItemId>> = HashMap::new();
    for &fid in &feature_ids {
        adj.entry(fid).or_default();
    }
    for f in &active_features {
        if let Some(pid) = f.parent_feature_id {
            adj.entry(f.id).or_default().push(pid);
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

    // 3-color DFS cycle check on G = hierarchy ∪ feature blockers
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Gray,
        Black,
    }
    let mut color: HashMap<ItemId, Color> = feature_ids.iter().map(|&id| (id, Color::White)).collect();

    fn dfs(
        node: ItemId,
        adj: &HashMap<ItemId, Vec<ItemId>>,
        color: &mut HashMap<ItemId, Color>,
    ) -> bool {
        color.insert(node, Color::Gray);
        if let Some(neighbors) = adj.get(&node) {
            for &next in neighbors {
                match color.get(&next).copied().unwrap_or(Color::White) {
                    Color::Gray => return true,
                    Color::White => {
                        if dfs(next, adj, color) {
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
            assert!(!dfs(fid, &adj, &mut color), "INV-05 cycle detected in G!");
        }
    }

    // Ancestor/descendant exclusion for Features & attached/ancestor exclusion for Work Items
    for edge in &snap.item_blockers {
        let blocked = snap.item(edge.blocked_item_id).unwrap();
        let blocker = snap.item(edge.blocker_feature_id).unwrap();
        assert_eq!(blocker.kind, ItemKind::Feature, "INV-04 violation");

        if blocked.kind == ItemKind::Feature {
            let ancestors = feature_ancestors(snap, blocked.id);
            let descendants = snap.descendant_feature_ids(blocked.id);
            assert!(
                !ancestors.contains(&blocker.id),
                "INV-05 violation: feature blocked on ancestor"
            );
            assert!(
                !descendants.contains(&blocker.id),
                "INV-05 violation: feature blocked on descendant"
            );
        } else {
            for f in snap.features_for_work_item(blocked.id) {
                assert_ne!(
                    f.id, blocker.id,
                    "INV-05 violation: WI blocked on attached feature"
                );
                let anc = feature_ancestors(snap, f.id);
                assert!(
                    !anc.contains(&blocker.id),
                    "INV-05 violation: WI blocked on ancestor of attached feature"
                );
            }
        }
    }

    // Reference evaluator check for EffectiveBlockState (INV-06)
    for item in snap.active_items() {
        let expected = reference_effective_block_state(snap, item.id);
        assert_eq!(
            snap.effective_block_state(item.id),
            expected,
            "INV-06 mismatch for {}",
            item.item_key
        );
    }
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

fn setup_seed_conn() -> (Connection, ProjectId) {
    let mut conn = Connection::open_in_memory().unwrap();
    inspect_and_migrate(&mut conn, None, "PropTest").unwrap();
    let pid = {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let p = create_project(
            &tx,
            CreateProjectInput {
                name: "Fuzz".to_string(),
                key_prefix: "FUZ".to_string(),
                description: String::new(),
                color: "#6366F1".to_string(),
                status: ProjectStatus::Active,
            },
        )
        .unwrap();
        // Seed 3 initial Features and 2 Work Items so operations immediately have rich targets.
        let f1 = create_feature(
            &tx,
            CreateFeatureInput {
                project_id: p.id,
                parent_feature_id: None,
                title: "Seed F1".to_string(),
                description: String::new(),
                status: ItemStatus::Todo,
                priority: Priority::None,
            },
        )
        .unwrap();
        let f2 = create_feature(
            &tx,
            CreateFeatureInput {
                project_id: p.id,
                parent_feature_id: Some(f1.id),
                title: "Seed F2".to_string(),
                description: String::new(),
                status: ItemStatus::Todo,
                priority: Priority::None,
            },
        )
        .unwrap();
        let _f3 = create_feature(
            &tx,
            CreateFeatureInput {
                project_id: p.id,
                parent_feature_id: None,
                title: "Seed F3".to_string(),
                description: String::new(),
                status: ItemStatus::Todo,
                priority: Priority::None,
            },
        )
        .unwrap();
        let _w1 = create_work_item(
            &tx,
            CreateWorkItemInput {
                project_id: p.id,
                title: "Seed W1".to_string(),
                description: String::new(),
                status: ItemStatus::Todo,
                priority: Priority::None,
                feature_ids: vec![f2.id],
            },
        )
        .unwrap();
        tx.commit().unwrap();
        p.id
    };
    (conn, pid)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn prop_combined_dag_stays_acyclic_and_effective_blocking_matches_reference(
        ops in proptest::collection::vec(op_strategy(), 25..45)
    ) {
        let (mut conn, pid) = setup_seed_conn();
        let statuses = [
            ItemStatus::Backlog,
            ItemStatus::Todo,
            ItemStatus::InProgress,
            ItemStatus::Done,
            ItemStatus::Cancelled,
        ];

        for (step, op) in ops.into_iter().enumerate() {
            let snap_before = load_project_snapshot(&conn, pid).unwrap();
            let features: Vec<ItemId> = snap_before
                .active_items()
                .into_iter()
                .filter(|i| i.kind == ItemKind::Feature)
                .map(|i| i.id)
                .collect();
            let work_items: Vec<ItemId> = snap_before
                .active_items()
                .into_iter()
                .filter(|i| i.kind == ItemKind::WorkItem)
                .map(|i| i.id)
                .collect();
            let all_items: Vec<ItemId> = snap_before.active_items().into_iter().map(|i| i.id).collect();

            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();

            let res: Result<(), _> = match op {
                DagOp::CreateFeature { parent_pick } => {
                    let parent = if features.is_empty() || parent_pick % 4 == 0 {
                        None
                    } else {
                        Some(features[parent_pick % features.len()])
                    };
                    create_feature(
                        &tx,
                        CreateFeatureInput {
                            project_id: pid,
                            parent_feature_id: parent,
                            title: format!("Feat {step}"),
                            description: String::new(),
                            status: ItemStatus::Todo,
                            priority: Priority::None,
                        },
                    )
                    .map(|_| ())
                }
                DagOp::CreateWorkItem { feature_pick } => {
                    let feats = match (feature_pick, features.is_empty()) {
                        (Some(ix), false) => vec![features[ix % features.len()]],
                        _ => Vec::new(),
                    };
                    create_work_item(
                        &tx,
                        CreateWorkItemInput {
                            project_id: pid,
                            title: format!("WI {step}"),
                            description: String::new(),
                            status: ItemStatus::Todo,
                            priority: Priority::None,
                            feature_ids: feats,
                        },
                    )
                    .map(|_| ())
                }
                DagOp::ReparentFeature { feat_pick, new_parent_pick } => {
                    if features.is_empty() {
                        Ok(())
                    } else {
                        let fid = features[feat_pick % features.len()];
                        let new_parent = new_parent_pick.map(|ix| features[ix % features.len()]);
                        reparent_feature(&tx, fid, new_parent).map(|_| ())
                    }
                }
                DagOp::AttachWorkItem { wi_pick, feat_pick } => {
                    if work_items.is_empty() || features.is_empty() {
                        Ok(())
                    } else {
                        attach_work_item_to_feature(
                            &tx,
                            work_items[wi_pick % work_items.len()],
                            features[feat_pick % features.len()],
                        )
                        .map(|_| ())
                    }
                }
                DagOp::DetachWorkItem { wi_pick, feat_pick } => {
                    if work_items.is_empty() || features.is_empty() {
                        Ok(())
                    } else {
                        detach_work_item_from_feature(
                            &tx,
                            work_items[wi_pick % work_items.len()],
                            features[feat_pick % features.len()],
                        )
                        .map(|_| ())
                    }
                }
                DagOp::AddBlocker { item_pick, blocker_pick } => {
                    if all_items.is_empty() || features.is_empty() {
                        Ok(())
                    } else {
                        add_blocker(
                            &tx,
                            all_items[item_pick % all_items.len()],
                            features[blocker_pick % features.len()],
                        )
                        .map(|_| ())
                    }
                }
                DagOp::RemoveBlocker { item_pick, blocker_pick } => {
                    if all_items.is_empty() || features.is_empty() {
                        Ok(())
                    } else {
                        remove_blocker(
                            &tx,
                            all_items[item_pick % all_items.len()],
                            features[blocker_pick % features.len()],
                        )
                        .map(|_| ())
                    }
                }
                DagOp::SetStatus { item_pick, status_idx } => {
                    if all_items.is_empty() {
                        Ok(())
                    } else {
                        update_item_fields(
                            &tx,
                            UpdateItemFieldsInput {
                                item_id: all_items[item_pick % all_items.len()],
                                status: Some(statuses[status_idx % statuses.len()]),
                                ..Default::default()
                            },
                        )
                        .map(|_| ())
                    }
                }
            };

            if res.is_ok() {
                tx.commit().unwrap();
            } else {
                drop(tx);
            }

            let snap_after = load_project_snapshot(&conn, pid).unwrap();
            assert_combined_graph_acyclic_and_exclusions_hold(&snap_after);
        }
    }
}
