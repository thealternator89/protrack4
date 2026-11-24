use crate::domain::{ItemId, ItemKind};

/// Specific structural reason preventing bidirectional Feature ↔ Work Item conversion (`INV-08`, `INV-09`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversionBlockReason {
    HasChildFeatures(usize),
    HasAttachedWorkItems(usize),
    HasActiveDependents(usize),
    AttachedToMultipleFeatures(usize),
    ExceedsMaxFeatureDepth { max_depth: usize },
}

/// Result of checking whether an active item can convert between `Feature` and `WorkItem`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversionEligibility {
    Allowed {
        target_kind: ItemKind,
        resulting_parent: Option<ItemId>,
        moves_to_unallocated: bool,
    },
    Blocked(Vec<ConversionBlockReason>),
}

impl ConversionEligibility {
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed { .. })
    }

    pub fn reasons(&self) -> &[ConversionBlockReason] {
        match self {
            Self::Allowed { .. } => &[],
            Self::Blocked(reasons) => reasons,
        }
    }
}

/// Formats an explanatory message enumerating all `ConversionBlockReason` counts (`PRODUCT_SPEC.md` §4 / `UI_SPEC.md` §5).
pub fn format_conversion_block_message(
    source_kind: ItemKind,
    reasons: &[ConversionBlockReason],
    attached_feature_names: &[String],
    dependent_keys: &[String],
) -> String {
    if reasons.is_empty() {
        return match source_kind {
            ItemKind::WorkItem => "Cannot convert to Feature.".to_string(),
            ItemKind::Feature => "Cannot convert to Work Item.".to_string(),
        };
    }

    match source_kind {
        ItemKind::WorkItem => {
            let mut parts = Vec::new();
            for reason in reasons {
                match reason {
                    ConversionBlockReason::AttachedToMultipleFeatures(n) => {
                        if attached_feature_names.is_empty() {
                            parts.push(format!(
                                "attached to {n} Features. Remove extra Features until 0 or 1 remains"
                            ));
                        } else {
                            let preview = if attached_feature_names.len() > 3 {
                                format!(
                                    "{}, +{} more",
                                    attached_feature_names[..3].join(", "),
                                    attached_feature_names.len() - 3
                                )
                            } else {
                                attached_feature_names.join(", ")
                            };
                            parts.push(format!(
                                "attached to {n} Features ({preview}). Remove extra Features until 0 or 1 remains before converting"
                            ));
                        }
                    }
                    ConversionBlockReason::ExceedsMaxFeatureDepth { max_depth } => {
                        parts.push(format!(
                            "parent Feature is already at max nesting depth ({max_depth})"
                        ));
                    }
                    _ => {}
                }
            }
            format!("Cannot convert to Feature: {}.", parts.join("; "))
        }
        ItemKind::Feature => {
            let mut structural = Vec::new();
            let mut dependents_part: Option<String> = None;

            for reason in reasons {
                match reason {
                    ConversionBlockReason::HasChildFeatures(n) => {
                        let noun = if *n == 1 {
                            "child Feature"
                        } else {
                            "child Features"
                        };
                        structural.push(format!("has {n} {noun}"));
                    }
                    ConversionBlockReason::HasAttachedWorkItems(n) => {
                        let noun = if *n == 1 {
                            "attached Work Item"
                        } else {
                            "attached Work Items"
                        };
                        structural.push(format!("has {n} {noun}"));
                    }
                    ConversionBlockReason::HasActiveDependents(n) => {
                        let noun = if *n == 1 { "item" } else { "items" };
                        let verb = if *n == 1 { "is" } else { "are" };
                        let keys_suffix = if dependent_keys.is_empty() {
                            String::new()
                        } else if dependent_keys.len() > 3 {
                            format!(
                                " ({}, +{} more)",
                                dependent_keys[..3].join(", "),
                                dependent_keys.len() - 3
                            )
                        } else {
                            format!(" ({})", dependent_keys.join(", "))
                        };
                        dependents_part = Some(format!(
                            "{n} {noun}{keys_suffix} {verb} blocked on this Feature"
                        ));
                    }
                    _ => {}
                }
            }

            let mut clauses = Vec::new();
            if !structural.is_empty() {
                clauses.push(structural.join(" and "));
            }
            if let Some(dep) = dependents_part {
                clauses.push(dep);
            }
            format!("Cannot convert to Work Item: {}.", clauses.join("; "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_messages_enumerate_exact_counts_and_names() {
        let wi_msg = format_conversion_block_message(
            ItemKind::WorkItem,
            &[ConversionBlockReason::AttachedToMultipleFeatures(2)],
            &["Auth".to_string(), "Billing".to_string()],
            &[],
        );
        assert!(wi_msg.contains("2 Features (Auth, Billing)"));
        assert!(wi_msg.contains("0 or 1 remains"));

        let depth_msg = format_conversion_block_message(
            ItemKind::WorkItem,
            &[ConversionBlockReason::ExceedsMaxFeatureDepth { max_depth: 32 }],
            &[],
            &[],
        );
        assert_eq!(
            depth_msg,
            "Cannot convert to Feature: parent Feature is already at max nesting depth (32)."
        );

        let feat_msg = format_conversion_block_message(
            ItemKind::Feature,
            &[
                ConversionBlockReason::HasChildFeatures(2),
                ConversionBlockReason::HasAttachedWorkItems(3),
                ConversionBlockReason::HasActiveDependents(1),
            ],
            &[],
            &["PRO-11".to_string()],
        );
        assert!(feat_msg.contains("has 2 child Features and has 3 attached Work Items"));
        assert!(feat_msg.contains("1 item (PRO-11) is blocked on this Feature"));
    }
}
