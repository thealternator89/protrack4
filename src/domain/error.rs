use thiserror::Error;

use crate::domain::{AssigneeId, ItemId, ProjectId, TagId};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("validation error: {0}")]
    Validation(String),

    #[error("project not found: {0}")]
    ProjectNotFound(ProjectId),

    #[error("item not found: {0}")]
    ItemNotFound(ItemId),

    #[error("assignee not found: {0}")]
    AssigneeNotFound(AssigneeId),

    #[error("tag not found: {0}")]
    TagNotFound(TagId),

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

    #[error(
        "This workspace was created by a newer version of ProTrack (schema v{file_version}, max supported v{max_supported}). Please upgrade ProTrack to open it."
    )]
    ForwardSchemaVersion {
        file_version: u32,
        max_supported: u32,
    },

    #[error("This workspace is currently open in another ProTrack window ({path}).")]
    WorkspaceLocked { path: String },

    #[error("Workspace file not found: {path}")]
    WorkspaceNotFound { path: String },

    #[error("Invalid workspace file: {0}")]
    InvalidWorkspace(String),
}
