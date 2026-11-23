use thiserror::Error;

use crate::domain::{ItemId, ProjectId};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("validation error: {0}")]
    Validation(String),

    #[error("project not found: {0}")]
    ProjectNotFound(ProjectId),

    #[error("item not found: {0}")]
    ItemNotFound(ItemId),

    #[error("feature nesting depth exceeds maximum of {max_depth}")]
    MaxFeatureDepthExceeded { max_depth: usize },

    #[error("cross-project reference is not allowed")]
    CrossProjectReference,

    #[error("dependency cycle detected: {0}")]
    DependencyCycle(String),

    #[error("conversion blocked: {0}")]
    ConversionBlocked(String),

    #[error("storage error: {0}")]
    Storage(String),
}
