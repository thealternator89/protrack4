pub mod blocker_commands;
pub mod conversion_commands;
pub mod item_commands;
pub mod meta_commands;
pub(crate) mod snapshot_helpers;
pub mod trash_commands;
pub mod undo_manager;

use rusqlite::Transaction;

use crate::domain::{AssigneeId, DomainError, DomainEvent, ItemId, ProjectId, TagId};

pub use blocker_commands::{AddBlocker, RemoveBlocker};
pub use conversion_commands::{ConvertFeatureToWorkItem, ConvertWorkItemToFeature};
pub use item_commands::{
    AttachWorkItemToFeature, CreateFeature, CreateWorkItem, DetachWorkItemFromFeature,
    MoveWorkItemBetweenFeatures, ReorderFeatureWorkItems, ReorderSiblingFeatures,
    ReorderUnallocatedWorkItems, ReparentFeature, SetItemTags, SetWorkItemFeatures,
    UpdateItemFields,
};
pub use meta_commands::{
    ArchiveAssignee, CreateProject, DeleteTag, UpdateProject, UpsertAssignee, UpsertTag,
};
pub use trash_commands::{EmptyTrash, PurgeTrashGroup, RestoreTrashGroup, TrashItemSubtree};
pub use undo_manager::{MAX_UNDO_DEPTH, UndoManager, UndoRedoStatus};

/// Result emitted by executing or undoing a command inside a SQLite transaction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandEffect {
    pub events: Vec<DomainEvent>,
    pub created_project_id: Option<ProjectId>,
    pub created_item_id: Option<ItemId>,
    pub created_assignee_id: Option<AssigneeId>,
    pub created_tag_id: Option<TagId>,
    pub selected_item_id: Option<ItemId>,
    pub toast: Option<String>,
    pub warning_toast: Option<String>,
}

/// Reversible database mutation executed inside a single `BEGIN IMMEDIATE` transaction.
pub trait UndoableCommand: Send + Sync {
    /// Human-readable label for menus, logs, and undo toasts.
    fn label(&self) -> String;

    /// Apply the forward mutation inside an active SQLite transaction.
    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError>;

    /// Revert the mutation inside an active SQLite transaction.
    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError>;

    /// Returns `true` for destructive commands (`PurgeTrashGroup`, `EmptyTrash`)
    /// that permanently delete rows and must reset the session Undo/Redo stacks.
    fn clears_undo_history(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, TransactionBehavior};

    use super::*;
    use crate::db::inspect_and_migrate;
    use crate::db::item_repo::{get_item, query_feature_tree};
    use crate::db::project_repo::get_project;
    use crate::domain::{ItemStatus, Priority, ProjectStatus};

    #[test]
    fn commands_execute_atomically_and_roll_back_on_error() {
        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "CmdTest").unwrap();

        let mut create_proj = CreateProject::new(
            "Core App",
            "CORE",
            "Main project",
            "#6366F1",
            ProjectStatus::Active,
        );
        let proj_effect = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let eff = create_proj.execute(&tx).unwrap();
            tx.commit().unwrap();
            eff
        };
        let pid = proj_effect.created_project_id.unwrap();
        assert_eq!(get_project(&conn, pid).unwrap().key_prefix, "CORE");

        let mut create_f1 = CreateFeature::new(
            pid,
            None,
            "Root Feature",
            "Markdown body",
            ItemStatus::Todo,
            Priority::Medium,
        );
        let f1_id = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let eff = create_f1.execute(&tx).unwrap();
            tx.commit().unwrap();
            eff.created_item_id.unwrap()
        };

        let mut create_f2 = CreateFeature::new(
            pid,
            Some(f1_id),
            "Child Feature",
            "",
            ItemStatus::Backlog,
            Priority::Low,
        );
        let f2_id = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let eff = create_f2.execute(&tx).unwrap();
            tx.commit().unwrap();
            eff.created_item_id.unwrap()
        };

        let mut create_f3 = CreateFeature::new(
            pid,
            None,
            "Second Root",
            "",
            ItemStatus::Todo,
            Priority::None,
        );
        let f3_id = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let eff = create_f3.execute(&tx).unwrap();
            tx.commit().unwrap();
            eff.created_item_id.unwrap()
        };

        // Update fields on f2
        let mut update_fields = UpdateItemFields::new(f2_id)
            .with_title("Child Feature Updated")
            .with_description("## Details")
            .with_status(ItemStatus::InProgress)
            .with_priority(Priority::Urgent);
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            update_fields.execute(&tx).unwrap();
            tx.commit().unwrap();
        }
        let f2 = get_item(&conn, f2_id).unwrap();
        assert_eq!(f2.title, "Child Feature Updated");
        assert_eq!(f2.status, ItemStatus::InProgress);
        assert_eq!(f2.priority, Priority::Urgent);

        // Reparent f2 to root, then reorder before f1
        let mut reparent = ReparentFeature::new(f2_id, None);
        let mut reorder = ReorderSiblingFeatures::new(pid, f2_id, None, Some(f1_id));
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            reparent.execute(&tx).unwrap();
            reorder.execute(&tx).unwrap();
            tx.commit().unwrap();
        }

        let tree = query_feature_tree(&conn, pid).unwrap();
        let keys: Vec<_> = tree.iter().map(|n| n.item_key.as_str()).collect();
        assert_eq!(keys, vec!["CORE-2", "CORE-1", "CORE-3"]);

        // Rename key prefix via UpdateProject
        let mut update_proj = UpdateProject::new(
            pid,
            "Core App v2",
            "APP",
            "Renamed",
            "#10B981",
            ProjectStatus::Active,
        );
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            update_proj.execute(&tx).unwrap();
            tx.commit().unwrap();
        }

        let tree_after = query_feature_tree(&conn, pid).unwrap();
        let keys_after: Vec<_> = tree_after.iter().map(|n| n.item_key.as_str()).collect();
        assert_eq!(keys_after, vec!["APP-2", "APP-1", "APP-3"]);

        // Invalid command fails and leaves DB untouched on drop/rollback
        let mut bad_update = UpdateItemFields::new(f3_id).with_title("   ");
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert!(bad_update.execute(&tx).is_err());
        }
        assert_eq!(get_item(&conn, f3_id).unwrap().title, "Second Root");
    }

    #[test]
    fn work_item_commands_create_attach_detach_and_set_features() {
        use crate::db::load_project_snapshot;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "WiCmdTest").unwrap();

        let mut create_proj = CreateProject::new(
            "Triage",
            "TRI",
            "Triage project",
            "#6366F1",
            ProjectStatus::Active,
        );
        let pid = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let eff = create_proj.execute(&tx).unwrap();
            tx.commit().unwrap();
            eff.created_project_id.unwrap()
        };

        let mut cf1 = CreateFeature::new(pid, None, "Auth", "", ItemStatus::Todo, Priority::High);
        let mut cf2 = CreateFeature::new(pid, None, "CLI", "", ItemStatus::Todo, Priority::Medium);
        let (f1, f2) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let id1 = cf1.execute(&tx).unwrap().created_item_id.unwrap();
            let id2 = cf2.execute(&tx).unwrap().created_item_id.unwrap();
            tx.commit().unwrap();
            (id1, id2)
        };

        let mut create_wi = CreateWorkItem::new(
            pid,
            "Brain dump task",
            "",
            ItemStatus::Todo,
            Priority::None,
            Vec::new(),
        );
        let wi_eff = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let eff = create_wi.execute(&tx).unwrap();
            tx.commit().unwrap();
            eff
        };
        let wi_id = wi_eff.created_item_id.unwrap();
        assert_eq!(wi_eff.selected_item_id, Some(wi_id));
        assert!(wi_eff.toast.as_deref().unwrap().contains("TRI-3"));

        let snap = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(snap.unallocated_work_items().len(), 1);

        let mut attach1 = AttachWorkItemToFeature::new(wi_id, f1);
        let mut attach2 = AttachWorkItemToFeature::new(wi_id, f2);
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            attach1.execute(&tx).unwrap();
            attach2.execute(&tx).unwrap();
            tx.commit().unwrap();
        }
        let snap2 = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(snap2.features_for_work_item(wi_id).len(), 2);
        assert!(snap2.unallocated_work_items().is_empty());

        let mut detach1 = DetachWorkItemFromFeature::new(wi_id, f1);
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let eff = detach1.execute(&tx).unwrap();
            tx.commit().unwrap();
            assert!(eff.toast.as_deref().unwrap().contains("Detached"));
        }
        let snap3 = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(snap3.features_for_work_item(wi_id).len(), 1);

        let mut unallocate = SetWorkItemFeatures::new(wi_id, Vec::new());
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let eff = unallocate.execute(&tx).unwrap();
            tx.commit().unwrap();
            assert_eq!(eff.toast.as_deref(), Some("Moved TRI-3 to Unallocated"));
        }
        let snap4 = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(snap4.unallocated_work_items().len(), 1);
    }

    #[test]
    fn assignee_and_tag_commands_execute_atomically_and_enforce_isolation() {
        use crate::db::load_project_snapshot;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "MetaCmdTest").unwrap();

        let (pid_a, pid_b) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut ca = CreateProject::new("Alpha", "ALP", "", "#6366F1", ProjectStatus::Active);
            let mut cb = CreateProject::new("Beta", "BET", "", "#10B981", ProjectStatus::Active);
            let pa = ca.execute(&tx).unwrap().created_project_id.unwrap();
            let pb = cb.execute(&tx).unwrap().created_project_id.unwrap();
            tx.commit().unwrap();
            (pa, pb)
        };

        let (wi_a, wi_b) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut cwa =
                CreateWorkItem::new(pid_a, "Alpha task", "", ItemStatus::Todo, Priority::High, vec![]);
            let mut cwb =
                CreateWorkItem::new(pid_b, "Beta task", "", ItemStatus::Todo, Priority::Low, vec![]);
            let ia = cwa.execute(&tx).unwrap().created_item_id.unwrap();
            let ib = cwb.execute(&tx).unwrap().created_item_id.unwrap();
            tx.commit().unwrap();
            (ia, ib)
        };

        // Create global Assignee and edit it
        let mut create_assignee = UpsertAssignee::create(
            "Mark Benson",
            "MB",
            "#6366F1",
            Some("Founder".to_string()),
            "Owner",
        );
        let assignee_eff = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let eff = create_assignee.execute(&tx).unwrap();
            tx.commit().unwrap();
            eff
        };
        let mb_id = assignee_eff.created_assignee_id.unwrap();
        assert_eq!(assignee_eff.events, vec![DomainEvent::AssigneesChanged]);

        // Assign MB to both Alpha and Beta items
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut ua = UpdateItemFields::new(wi_a).with_assignee_id(Some(mb_id));
            let mut ub = UpdateItemFields::new(wi_b).with_assignee_id(Some(mb_id));
            ua.execute(&tx).unwrap();
            ub.execute(&tx).unwrap();
            tx.commit().unwrap();
        }

        // Archive MB -> preserved on items, rejected on unassign/re-assign to new item
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut arch = ArchiveAssignee::new(mb_id, true);
            let eff = arch.execute(&tx).unwrap();
            assert_eq!(eff.events, vec![DomainEvent::AssigneesChanged]);
            tx.commit().unwrap();
        }

        // Create Project A tag, attach to wi_a, verify Project B cannot attach it
        let mut create_tag = UpsertTag::create(pid_a, "backend", "#8B5CF6");
        let tag_id = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let eff = create_tag.execute(&tx).unwrap();
            assert_eq!(eff.events, vec![DomainEvent::TagsChanged { project_id: pid_a }]);
            tx.commit().unwrap();
            eff.created_tag_id.unwrap()
        };

        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut set_ok = SetItemTags::new(wi_a, vec![tag_id]);
            set_ok.execute(&tx).unwrap();
            tx.commit().unwrap();
        }
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut set_cross = SetItemTags::new(wi_b, vec![tag_id]);
            assert_eq!(
                set_cross.execute(&tx).unwrap_err(),
                DomainError::CrossProjectReference
            );
        }

        let snap_a = load_project_snapshot(&conn, pid_a).unwrap();
        assert_eq!(snap_a.item(wi_a).unwrap().assignee_id, Some(mb_id));
        assert_eq!(snap_a.tags_for_item(wi_a).len(), 1);

        // Delete tag
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut del = DeleteTag::new(tag_id);
            let eff = del.execute(&tx).unwrap();
            assert_eq!(
                eff.events,
                vec![
                    DomainEvent::TagsChanged { project_id: pid_a },
                    DomainEvent::ItemsChanged { project_id: pid_a },
                ]
            );
            tx.commit().unwrap();
        }
        let snap_a_after = load_project_snapshot(&conn, pid_a).unwrap();
        assert!(snap_a_after.tags.is_empty());
        assert!(snap_a_after.tags_for_item(wi_a).is_empty());
    }

    #[test]
    fn blocker_commands_and_soft_warning_on_in_progress() {
        use crate::db::load_project_snapshot;
        use crate::domain::EffectiveBlockState;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "BlockerCmdTest").unwrap();

        let pid = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut cp = CreateProject::new("Deps", "DEP", "", "#6366F1", ProjectStatus::Active);
            let pid = cp.execute(&tx).unwrap().created_project_id.unwrap();
            tx.commit().unwrap();
            pid
        };

        let (f_a, f_b, wi_a) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut ca =
                CreateFeature::new(pid, None, "Feature A", "", ItemStatus::Todo, Priority::High);
            let mut cb = CreateFeature::new(
                pid,
                None,
                "Blocker B",
                "",
                ItemStatus::Todo,
                Priority::Medium,
            );
            let a = ca.execute(&tx).unwrap().created_item_id.unwrap();
            let b = cb.execute(&tx).unwrap().created_item_id.unwrap();
            let mut cwi =
                CreateWorkItem::new(pid, "Child WI", "", ItemStatus::Todo, Priority::Low, vec![a]);
            let wi = cwi.execute(&tx).unwrap().created_item_id.unwrap();
            tx.commit().unwrap();
            (a, b, wi)
        };

        // AddBlocker(A, B)
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut add = AddBlocker::new(f_a, f_b);
            let eff = add.execute(&tx).unwrap();
            assert_eq!(
                eff.events,
                vec![DomainEvent::ItemsChanged { project_id: pid }]
            );
            assert!(eff.toast.as_deref().unwrap().contains("DEP-1"));
            assert!(eff.toast.as_deref().unwrap().contains("DEP-2"));
            tx.commit().unwrap();
        }

        // Moving inherited-blocked Work Item to InProgress succeeds (soft blocking) and emits warning_toast
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut start_wi = UpdateItemFields::new(wi_a).with_status(ItemStatus::InProgress);
            let eff = start_wi.execute(&tx).unwrap();
            assert_eq!(
                eff.warning_toast.as_deref(),
                Some("Warning: DEP-3 is currently blocked by DEP-2")
            );
            tx.commit().unwrap();
        }
        assert_eq!(get_item(&conn, wi_a).unwrap().status, ItemStatus::InProgress);

        // Moving unblocked Feature B to InProgress emits no warning_toast
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut start_b = UpdateItemFields::new(f_b).with_status(ItemStatus::InProgress);
            let eff = start_b.execute(&tx).unwrap();
            assert_eq!(eff.warning_toast, None);
            tx.commit().unwrap();
        }

        // RemoveBlocker(A, B) clears block
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut rem = RemoveBlocker::new(f_a, f_b);
            let eff = rem.execute(&tx).unwrap();
            assert!(eff.toast.as_deref().unwrap().contains("Removed blocker"));
            tx.commit().unwrap();
        }
        let snap = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(
            snap.effective_block_state(f_a),
            EffectiveBlockState::Unblocked
        );
        assert_eq!(
            snap.effective_block_state(wi_a),
            EffectiveBlockState::Unblocked
        );
    }

    #[test]
    fn conversion_commands_preserve_key_and_reject_blocked_cases() {
        use crate::db::load_project_snapshot;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "ConvCmdTest").unwrap();

        let pid = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut cp = CreateProject::new("Convert", "CNV", "", "#6366F1", ProjectStatus::Active);
            let pid = cp.execute(&tx).unwrap().created_project_id.unwrap();
            tx.commit().unwrap();
            pid
        };

        let (f_parent, wi_id) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut cf =
                CreateFeature::new(pid, None, "Parent", "", ItemStatus::Todo, Priority::High);
            let fid = cf.execute(&tx).unwrap().created_item_id.unwrap();
            let mut cwi = CreateWorkItem::new(
                pid,
                "Convertible",
                "Body",
                ItemStatus::Todo,
                Priority::Medium,
                vec![fid],
            );
            let wid = cwi.execute(&tx).unwrap().created_item_id.unwrap();
            tx.commit().unwrap();
            (fid, wid)
        };

        // Parent with attached WI cannot convert
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut bad = ConvertFeatureToWorkItem::new(f_parent);
            assert!(matches!(
                bad.execute(&tx),
                Err(DomainError::ConversionBlocked(_))
            ));
        }

        // Convertible WI -> Child Feature under f_parent -> back to Work Item attached to [f_parent]
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut to_feat = ConvertWorkItemToFeature::new(wi_id);
            let eff = to_feat.execute(&tx).unwrap();
            assert_eq!(eff.selected_item_id, Some(wi_id));
            assert_eq!(eff.toast.as_deref(), Some("Converted CNV-2 to Feature"));
            tx.commit().unwrap();
        }
        let item_as_feat = get_item(&conn, wi_id).unwrap();
        assert_eq!(item_as_feat.item_key, "CNV-2");
        assert_eq!(item_as_feat.parent_feature_id, Some(f_parent));

        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut to_wi = ConvertFeatureToWorkItem::new(wi_id);
            let eff = to_wi.execute(&tx).unwrap();
            assert_eq!(eff.selected_item_id, Some(wi_id));
            assert_eq!(eff.toast.as_deref(), Some("Converted CNV-2 to Work Item"));
            tx.commit().unwrap();
        }
        let snap = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(snap.item(wi_id).unwrap().item_key, "CNV-2");
        assert_eq!(snap.features_for_work_item(wi_id).len(), 1);
    }

    #[test]
    fn trash_commands_execute_atomically_and_emit_events_and_toasts() {
        use crate::db::load_project_snapshot;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "TrashCmdTest").unwrap();

        let pid = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut cp = CreateProject::new("TrashCmd", "TRSH", "", "#6366F1", ProjectStatus::Active);
            let pid = cp.execute(&tx).unwrap().created_project_id.unwrap();
            tx.commit().unwrap();
            pid
        };

        let (f1_id, f3_id) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut cf1 = CreateFeature::new(pid, None, "Epic", "", ItemStatus::Todo, Priority::High);
            let f1 = cf1.execute(&tx).unwrap().created_item_id.unwrap();
            let mut cf2 = CreateFeature::new(pid, Some(f1), "Sub", "", ItemStatus::Todo, Priority::Medium);
            let f2 = cf2.execute(&tx).unwrap().created_item_id.unwrap();
            let mut cf3 = CreateFeature::new(pid, None, "Other", "", ItemStatus::Todo, Priority::Low);
            let f3 = cf3.execute(&tx).unwrap().created_item_id.unwrap();
            let mut c_excl = CreateWorkItem::new(pid, "Exclusive", "", ItemStatus::Todo, Priority::None, vec![f2]);
            c_excl.execute(&tx).unwrap();
            let mut c_shared = CreateWorkItem::new(pid, "Shared", "", ItemStatus::Todo, Priority::None, vec![f2, f3]);
            c_shared.execute(&tx).unwrap();
            tx.commit().unwrap();
            (f1, f3)
        };

        // TrashItemSubtree
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut cmd = TrashItemSubtree::new(f1_id);
            let eff = cmd.execute(&tx).unwrap();
            assert_eq!(
                eff.events,
                vec![
                    DomainEvent::ItemsChanged { project_id: pid },
                    DomainEvent::TrashChanged { project_id: pid },
                ]
            );
            let toast = eff.toast.unwrap();
            assert!(toast.contains("TRSH-1"));
            assert!(toast.contains("+1 sub-feature"));
            assert!(toast.contains("+1 work item"));
            assert!(toast.contains("1 shared work item kept active"));
            tx.commit().unwrap();
        }
        assert_eq!(load_project_snapshot(&conn, pid).unwrap().trash_count, 1);

        // RestoreTrashGroup
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut cmd = RestoreTrashGroup::new(f1_id);
            let eff = cmd.execute(&tx).unwrap();
            assert_eq!(eff.selected_item_id, Some(f1_id));
            assert!(eff.toast.unwrap().contains("Restored TRSH-1 (3 items)"));
            tx.commit().unwrap();
        }
        assert_eq!(load_project_snapshot(&conn, pid).unwrap().trash_count, 0);

        // Trash again, PurgeTrashGroup, then Trash F3 + EmptyTrash
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            TrashItemSubtree::new(f1_id).execute(&tx).unwrap();
            let mut purge = PurgeTrashGroup::new(f1_id);
            let purge_eff = purge.execute(&tx).unwrap();
            assert!(purge_eff.toast.unwrap().contains("Permanently deleted TRSH-1 (3 items)"));
            assert!(purge.undo(&tx).is_err());

            TrashItemSubtree::new(f3_id).execute(&tx).unwrap();
            let mut empty = EmptyTrash::new(pid);
            let empty_eff = empty.execute(&tx).unwrap();
            assert!(empty_eff.toast.unwrap().contains("Emptied Trash (2 items permanently deleted)"));
            assert!(empty.undo(&tx).is_err());
            tx.commit().unwrap();
        }
        assert!(load_project_snapshot(&conn, pid).unwrap().active_items().is_empty());
    }

    #[test]
    fn dnd_commands_and_quick_add_assignee_tags_execute_atomically() {
        use crate::db::load_project_snapshot;

        let mut conn = Connection::open_in_memory().unwrap();
        inspect_and_migrate(&mut conn, None, "Phase11CmdTest").unwrap();

        let (pid, assignee_id, tag_id) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let mut cp = CreateProject::new("Quick", "QCK", "", "#6366F1", ProjectStatus::Active);
            let pid = cp.execute(&tx).unwrap().created_project_id.unwrap();
            let mut ca = UpsertAssignee::create("Mark", "MB", "#6366F1", None, "");
            let aid = ca.execute(&tx).unwrap().created_assignee_id.unwrap();
            let mut ct = UpsertTag::create(pid, "urgent-fix", "#EF4444");
            let tid = ct.execute(&tx).unwrap().created_tag_id.unwrap();
            tx.commit().unwrap();
            (pid, aid, tid)
        };

        let (f1, f2, wi1, wi2, u1, u2) = {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let f1 = CreateFeature::new(pid, None, "Feat 1", "", ItemStatus::Todo, Priority::High)
                .with_assignee(Some(assignee_id))
                .with_tags(vec![tag_id])
                .execute(&tx)
                .unwrap()
                .created_item_id
                .unwrap();
            let f2 = CreateFeature::new(pid, None, "Feat 2", "", ItemStatus::Todo, Priority::Low)
                .execute(&tx)
                .unwrap()
                .created_item_id
                .unwrap();
            let wi1 = CreateWorkItem::new(
                pid,
                "WI 1",
                "Body",
                ItemStatus::InProgress,
                Priority::Urgent,
                vec![f1],
            )
            .with_assignee(Some(assignee_id))
            .with_tags(vec![tag_id])
            .execute(&tx)
            .unwrap()
            .created_item_id
            .unwrap();
            let wi2 = CreateWorkItem::new(pid, "WI 2", "", ItemStatus::Todo, Priority::None, vec![f1])
                .execute(&tx)
                .unwrap()
                .created_item_id
                .unwrap();
            let u1 = CreateWorkItem::new(pid, "U1", "", ItemStatus::Todo, Priority::None, vec![])
                .execute(&tx)
                .unwrap()
                .created_item_id
                .unwrap();
            let u2 = CreateWorkItem::new(pid, "U2", "", ItemStatus::Todo, Priority::None, vec![])
                .execute(&tx)
                .unwrap()
                .created_item_id
                .unwrap();
            tx.commit().unwrap();
            (f1, f2, wi1, wi2, u1, u2)
        };

        let snap = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(snap.item(f1).unwrap().assignee_id, Some(assignee_id));
        assert_eq!(snap.tags_for_item(f1).len(), 1);
        assert_eq!(snap.item(wi1).unwrap().assignee_id, Some(assignee_id));
        assert_eq!(snap.tags_for_item(wi1).len(), 1);

        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            ReorderFeatureWorkItems::new(pid, f1, wi2, None, Some(wi1))
                .execute(&tx)
                .unwrap();
            ReorderUnallocatedWorkItems::new(pid, u2, None, Some(u1))
                .execute(&tx)
                .unwrap();
            let mv_eff = MoveWorkItemBetweenFeatures::new(wi1, f1, f2)
                .execute(&tx)
                .unwrap();
            assert!(mv_eff.toast.unwrap().contains("Moved QCK-3 from QCK-1 to QCK-2"));
            tx.commit().unwrap();
        }

        let snap2 = load_project_snapshot(&conn, pid).unwrap();
        assert_eq!(
            snap2.work_items_for_feature(f1).into_iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![wi2]
        );
        assert_eq!(
            snap2.work_items_for_feature(f2).into_iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![wi1]
        );
        assert_eq!(
            snap2.unallocated_work_items().into_iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![u2, u1]
        );
    }
}
