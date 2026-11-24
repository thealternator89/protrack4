use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{ItemId, ItemStatus, ProjectId};

/// Persisted `item_blockers` edge (`blocked_item_id` is blocked by `blocker_feature_id`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemBlocker {
    pub project_id: ProjectId,
    pub blocked_item_id: ItemId,
    pub blocker_feature_id: ItemId,
    pub created_at: DateTime<Utc>,
}

/// Effective blocking state for an active Feature or Work Item (`INV-06`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectiveBlockState {
    #[default]
    Unblocked,
    DirectlyBlocked {
        active_count: usize,
    },
    InheritedBlocked {
        ancestor_blocker_count: usize,
    },
    PartiallyBlocked {
        blocked_features: usize,
        total_features: usize,
    },
}

impl EffectiveBlockState {
    /// Returns `true` when the item has a hard/full soft-block (`DirectlyBlocked` or `InheritedBlocked`).
    pub const fn is_blocked(&self) -> bool {
        matches!(
            self,
            Self::DirectlyBlocked { .. } | Self::InheritedBlocked { .. }
        )
    }

    /// Returns `true` when the item has any active direct, inherited, or partial block.
    pub const fn has_any_block(&self) -> bool {
        !matches!(self, Self::Unblocked)
    }

    /// Returns `true` under `INV-06` Ready-to-Work semantics (`Unblocked` or `PartiallyBlocked`).
    pub const fn is_ready_to_work(&self) -> bool {
        matches!(self, Self::Unblocked | Self::PartiallyBlocked { .. })
    }

    /// Formats a concise badge label for Outline and Inspector views.
    pub fn badge_label(&self, primary_blocker_key: Option<&str>) -> Option<String> {
        match *self {
            Self::Unblocked => None,
            Self::DirectlyBlocked { active_count: 1 } => Some(match primary_blocker_key {
                Some(key) => format!("Blocked: {key}"),
                None => "Blocked by 1".to_string(),
            }),
            Self::DirectlyBlocked { active_count } => Some(format!("Blocked by {active_count}")),
            Self::InheritedBlocked { .. } => Some("Inherited block".to_string()),
            Self::PartiallyBlocked {
                blocked_features,
                total_features,
            } => Some(format!("Partial ({blocked_features}/{total_features})")),
        }
    }
}

/// Non-blocking warning surfaced when a blocker Feature is `cancelled` or in Trash (`INV-07`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockerWarning {
    Cancelled {
        blocker_id: ItemId,
        blocker_key: String,
        title: String,
    },
    InTrash {
        blocker_id: ItemId,
        blocker_key: String,
        title: String,
    },
}

impl BlockerWarning {
    pub const fn blocker_id(&self) -> ItemId {
        match self {
            Self::Cancelled { blocker_id, .. } | Self::InTrash { blocker_id, .. } => *blocker_id,
        }
    }

    pub fn blocker_key(&self) -> &str {
        match self {
            Self::Cancelled { blocker_key, .. } | Self::InTrash { blocker_key, .. } => blocker_key,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Self::Cancelled { title, .. } | Self::InTrash { title, .. } => title,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Cancelled { blocker_key, .. } => {
                format!("Blocker {blocker_key} was cancelled — review or remove")
            }
            Self::InTrash { blocker_key, .. } => {
                format!("Blocker {blocker_key} is in Trash")
            }
        }
    }

    pub const fn short_label(&self) -> &'static str {
        match self {
            Self::Cancelled { .. } => "Cancelled blocker",
            Self::InTrash { .. } => "Blocker in Trash",
        }
    }
}

/// Detailed reference to a direct or inherited blocker Feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockerRef {
    pub blocker_id: ItemId,
    pub blocker_key: String,
    pub blocker_title: String,
    pub blocker_status: ItemStatus,
    pub blocker_in_trash: bool,
    pub via_ancestor_id: Option<ItemId>,
    pub via_ancestor_key: Option<String>,
    pub via_ancestor_title: Option<String>,
}

impl BlockerRef {
    pub const fn is_active_block(&self) -> bool {
        !self.blocker_in_trash && !self.blocker_status.is_closed()
    }

    pub const fn is_resolved(&self) -> bool {
        !self.blocker_in_trash && matches!(self.blocker_status, ItemStatus::Done)
    }

    pub const fn is_cancelled(&self) -> bool {
        !self.blocker_in_trash && matches!(self.blocker_status, ItemStatus::Cancelled)
    }

    pub fn via_label(&self) -> Option<String> {
        match (&self.via_ancestor_key, &self.via_ancestor_title) {
            (Some(key), Some(title)) => Some(format!("via {key} {title}")),
            (None, Some(title)) => Some(format!("via {title}")),
            _ => None,
        }
    }
}

/// Explains why a candidate Feature cannot be added as a direct blocker (`INV-05`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockerCandidateRejection {
    SelfReference,
    AlreadyBlockedBy,
    AncestorOfItem,
    DescendantOfItem,
    AttachedFeatureOfWorkItem,
    AncestorOfAttachedFeature,
    WouldCreateCycle,
}

impl BlockerCandidateRejection {
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::SelfReference => "Cannot block on itself",
            Self::AlreadyBlockedBy => "Already a blocker",
            Self::AncestorOfItem => "Ancestor Feature",
            Self::DescendantOfItem => "Descendant Feature",
            Self::AttachedFeatureOfWorkItem => "Attached Feature",
            Self::AncestorOfAttachedFeature => "Ancestor of attached Feature",
            Self::WouldCreateCycle => "Creates dependency cycle",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_block_state_labels_and_readiness_match_inv06() {
        assert!(!EffectiveBlockState::Unblocked.is_blocked());
        assert!(EffectiveBlockState::Unblocked.is_ready_to_work());
        assert_eq!(EffectiveBlockState::Unblocked.badge_label(None), None);

        let direct_one = EffectiveBlockState::DirectlyBlocked { active_count: 1 };
        assert!(direct_one.is_blocked());
        assert!(!direct_one.is_ready_to_work());
        assert_eq!(
            direct_one.badge_label(Some("PRO-2")).as_deref(),
            Some("Blocked: PRO-2")
        );

        let direct_many = EffectiveBlockState::DirectlyBlocked { active_count: 3 };
        assert_eq!(
            direct_many.badge_label(Some("PRO-2")).as_deref(),
            Some("Blocked by 3")
        );

        let inherited = EffectiveBlockState::InheritedBlocked {
            ancestor_blocker_count: 2,
        };
        assert!(inherited.is_blocked());
        assert!(!inherited.is_ready_to_work());
        assert_eq!(
            inherited.badge_label(None).as_deref(),
            Some("Inherited block")
        );

        let partial = EffectiveBlockState::PartiallyBlocked {
            blocked_features: 1,
            total_features: 2,
        };
        assert!(!partial.is_blocked());
        assert!(partial.has_any_block());
        assert!(partial.is_ready_to_work());
        assert_eq!(
            partial.badge_label(None).as_deref(),
            Some("Partial (1/2)")
        );
    }

    #[test]
    fn blocker_warning_messages_match_inv07() {
        let id = ItemId::new();
        let cancelled = BlockerWarning::Cancelled {
            blocker_id: id,
            blocker_key: "PRO-9".to_string(),
            title: "Legacy Sync".to_string(),
        };
        assert_eq!(
            cancelled.message(),
            "Blocker PRO-9 was cancelled — review or remove"
        );
        assert_eq!(cancelled.short_label(), "Cancelled blocker");

        let trashed = BlockerWarning::InTrash {
            blocker_id: id,
            blocker_key: "PRO-10".to_string(),
            title: "Old Auth".to_string(),
        };
        assert_eq!(trashed.message(), "Blocker PRO-10 is in Trash");
        assert_eq!(trashed.short_label(), "Blocker in Trash");
    }
}
