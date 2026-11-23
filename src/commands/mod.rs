pub mod item_commands;
pub mod meta_commands;

use rusqlite::Transaction;

use crate::domain::{DomainError, DomainEvent, ItemId, ProjectId};

pub use item_commands::{
    AttachWorkItemToFeature, CreateFeature, CreateWorkItem, DetachWorkItemFromFeature,
    ReparentFeature, ReorderSiblingFeatures, SetWorkItemFeatures, UpdateItemFields,
};
pub use meta_commands::{CreateProject, UpdateProject};

/// Result emitted by executing or undoing a command inside a SQLite transaction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandEffect {
    pub events: Vec<DomainEvent>,
    pub created_project_id: Option<ProjectId>,
    pub created_item_id: Option<ItemId>,
    pub selected_item_id: Option<ItemId>,
    pub toast: Option<String>,
}

/// Reversible database mutation executed inside a single `BEGIN IMMEDIATE` transaction.
pub trait UndoableCommand: Send + Sync {
    /// Human-readable label for menus, logs, and undo toasts.
    fn label(&self) -> String;

    /// Apply the forward mutation inside an active SQLite transaction.
    fn execute(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError>;

    /// Revert the mutation inside an active SQLite transaction (enabled in Phase 12).
    fn undo(&mut self, tx: &Transaction<'_>) -> Result<CommandEffect, DomainError>;
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
        let mut update_fields = UpdateItemFields {
            item_id: f2_id,
            title: Some("Child Feature Updated".to_string()),
            description: Some("## Details".to_string()),
            status: Some(ItemStatus::InProgress),
            priority: Some(Priority::Urgent),
        };
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
        let mut update_proj = UpdateProject {
            project_id: pid,
            name: "Core App v2".to_string(),
            key_prefix: "APP".to_string(),
            description: "Renamed".to_string(),
            color: "#10B981".to_string(),
            status: ProjectStatus::Active,
        };
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
        let mut bad_update = UpdateItemFields {
            item_id: f3_id,
            title: Some("   ".to_string()),
            description: None,
            status: None,
            priority: None,
        };
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
}
