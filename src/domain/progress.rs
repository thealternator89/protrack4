use serde::{Deserialize, Serialize};

/// Deduplicated subtree completion metrics for a Feature (`INV-11`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureProgress {
    pub work_items_done: usize,
    pub work_items_total: usize,
    pub child_features_done: usize,
    pub child_features_total: usize,
}

impl FeatureProgress {
    /// Returns `(done, total)` using distinct non-cancelled Work Items when present,
    /// falling back to non-cancelled descendant Features when `work_items_total == 0` (`INV-11`).
    pub const fn effective_counts(&self) -> (usize, usize) {
        if self.work_items_total > 0 {
            (self.work_items_done, self.work_items_total)
        } else {
            (self.child_features_done, self.child_features_total)
        }
    }

    pub const fn is_using_feature_fallback(&self) -> bool {
        self.work_items_total == 0 && self.child_features_total > 0
    }

    pub const fn is_empty(&self) -> bool {
        self.work_items_total == 0 && self.child_features_total == 0
    }

    pub const fn is_complete(&self) -> bool {
        let (done, total) = self.effective_counts();
        total > 0 && done == total
    }

    pub fn completion_ratio(&self) -> Option<f64> {
        let (done, total) = self.effective_counts();
        if total > 0 {
            Some(done as f64 / total as f64)
        } else {
            None
        }
    }

    pub fn percentage(&self) -> Option<u8> {
        self.completion_ratio()
            .map(|ratio| (ratio * 100.0).round().clamp(0.0, 100.0) as u8)
    }

    pub fn progress_bar_value(&self) -> f32 {
        self.completion_ratio()
            .map(|ratio| (ratio * 100.0).clamp(0.0, 100.0) as f32)
            .unwrap_or(0.0)
    }

    pub fn ratio_label(&self) -> String {
        let (done, total) = self.effective_counts();
        if total == 0 {
            "—".to_string()
        } else {
            format!("{done}/{total}")
        }
    }

    pub fn percentage_label(&self) -> String {
        match self.percentage() {
            Some(pct) => format!("{pct}%"),
            None => "—".to_string(),
        }
    }

    pub fn detailed_label(&self) -> String {
        let (done, total) = self.effective_counts();
        match self.percentage() {
            Some(pct) => format!("{done}/{total} ({pct}%)"),
            None => "—".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FeatureProgress;

    #[test]
    fn formatting_and_fallback_match_inv11() {
        let normal = FeatureProgress {
            work_items_done: 5,
            work_items_total: 8,
            child_features_done: 1,
            child_features_total: 3,
        };
        assert_eq!(normal.effective_counts(), (5, 8));
        assert!(!normal.is_using_feature_fallback());
        assert!(!normal.is_empty());
        assert!(!normal.is_complete());
        assert_eq!(normal.percentage(), Some(63));
        assert_eq!(normal.ratio_label(), "5/8");
        assert_eq!(normal.percentage_label(), "63%");
        assert_eq!(normal.detailed_label(), "5/8 (63%)");
        assert!((normal.progress_bar_value() - 62.5).abs() < 0.01);

        let complete = FeatureProgress {
            work_items_done: 3,
            work_items_total: 3,
            child_features_done: 0,
            child_features_total: 0,
        };
        assert!(complete.is_complete());
        assert_eq!(complete.percentage(), Some(100));
        assert_eq!(complete.detailed_label(), "3/3 (100%)");
        assert_eq!(complete.progress_bar_value(), 100.0);

        let fallback = FeatureProgress {
            work_items_done: 0,
            work_items_total: 0,
            child_features_done: 1,
            child_features_total: 2,
        };
        assert!(fallback.is_using_feature_fallback());
        assert!(!fallback.is_empty());
        assert_eq!(fallback.effective_counts(), (1, 2));
        assert_eq!(fallback.percentage(), Some(50));
        assert_eq!(fallback.detailed_label(), "1/2 (50%)");

        let empty = FeatureProgress::default();
        assert!(empty.is_empty());
        assert!(!empty.is_using_feature_fallback());
        assert!(!empty.is_complete());
        assert_eq!(empty.completion_ratio(), None);
        assert_eq!(empty.percentage(), None);
        assert_eq!(empty.progress_bar_value(), 0.0);
        assert_eq!(empty.ratio_label(), "—");
        assert_eq!(empty.percentage_label(), "—");
        assert_eq!(empty.detailed_label(), "—");
    }
}
