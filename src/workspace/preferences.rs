use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};

use crate::domain::{DomainError, ProjectId};
use crate::state::ThemePreference;

pub const DEFAULT_SIDEBAR_WIDTH: f32 = 260.0;
pub const DEFAULT_INSPECTOR_WIDTH: f32 = 380.0;
const MAX_RECENT_WORKSPACES: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentWorkspace {
    pub path: PathBuf,
    pub name: String,
    pub last_opened: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppPreferences {
    pub last_workspace_path: Option<PathBuf>,
    pub recent_workspaces: Vec<RecentWorkspace>,
    pub theme: ThemePreference,
    pub sidebar_width: f32,
    pub inspector_width: f32,
    pub sidebar_collapsed: bool,
    pub inspector_open: bool,
    pub active_project_by_workspace: HashMap<String, ProjectId>,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            last_workspace_path: None,
            recent_workspaces: Vec::new(),
            theme: ThemePreference::System,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            inspector_width: DEFAULT_INSPECTOR_WIDTH,
            sidebar_collapsed: false,
            inspector_open: true,
            active_project_by_workspace: HashMap::new(),
        }
    }
}

impl AppPreferences {
    pub fn load_from(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(raw) => match serde_json::from_str::<Self>(&raw) {
                Ok(mut prefs) => {
                    prefs.clamp_widths();
                    prefs
                }
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "Failed to parse preferences.json; using defaults"
                    );
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "Failed to read preferences.json; using defaults"
                );
                Self::default()
            }
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<(), DomainError> {
        let mut normalized = self.clone();
        normalized.clamp_widths();

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                DomainError::Storage(format!(
                    "failed to create preferences directory '{}': {err}",
                    parent.display()
                ))
            })?;
        }

        let json = serde_json::to_string_pretty(&normalized).map_err(|err| {
            DomainError::Storage(format!("failed to serialize preferences.json: {err}"))
        })?;

        let mut tmp_os = OsString::from(path.as_os_str());
        tmp_os.push(".tmp");
        let tmp_path = PathBuf::from(tmp_os);

        {
            let mut file = fs::File::create(&tmp_path).map_err(|err| {
                DomainError::Storage(format!(
                    "failed to write temporary preferences file '{}': {err}",
                    tmp_path.display()
                ))
            })?;
            file.write_all(json.as_bytes()).map_err(|err| {
                DomainError::Storage(format!(
                    "failed to write preferences payload '{}': {err}",
                    tmp_path.display()
                ))
            })?;
            file.flush().map_err(|err| {
                DomainError::Storage(format!("failed to flush preferences temp file: {err}"))
            })?;
            file.sync_all().map_err(|err| {
                DomainError::Storage(format!("failed to sync preferences temp file: {err}"))
            })?;
        }

        fs::rename(&tmp_path, path).map_err(|err| {
            let _ = fs::remove_file(&tmp_path);
            DomainError::Storage(format!(
                "failed to save preferences file '{}': {err}",
                path.display()
            ))
        })?;

        Ok(())
    }

    pub fn clamp_widths(&mut self) {
        if !self.sidebar_width.is_finite() {
            self.sidebar_width = DEFAULT_SIDEBAR_WIDTH;
        } else {
            self.sidebar_width = self.sidebar_width.clamp(200.0, 400.0);
        }

        if !self.inspector_width.is_finite() {
            self.inspector_width = DEFAULT_INSPECTOR_WIDTH;
        } else {
            self.inspector_width = self.inspector_width.clamp(300.0, 540.0);
        }
    }

    pub fn record_workspace_opened(&mut self, path: &Path, name: &str, update_last: bool) {
        let canonical = canonical_or_original(path);
        if update_last {
            self.last_workspace_path = Some(canonical.clone());
        }

        self.recent_workspaces
            .retain(|entry| canonical_or_original(&entry.path) != canonical);

        let clean_name = if name.trim().is_empty() {
            canonical
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Workspace")
                .to_string()
        } else {
            name.trim().to_string()
        };

        self.recent_workspaces.insert(
            0,
            RecentWorkspace {
                path: canonical,
                name: clean_name,
                last_opened: Utc::now(),
            },
        );
        self.recent_workspaces.truncate(MAX_RECENT_WORKSPACES);
    }

    pub fn remove_recent_workspace(&mut self, path: &Path) {
        let target = canonical_or_original(path);
        self.recent_workspaces
            .retain(|entry| entry.path != path && canonical_or_original(&entry.path) != target);
        if let Some(last) = &self.last_workspace_path
            && (last == path || canonical_or_original(last) == target)
        {
            self.last_workspace_path = None;
        }
    }

    pub fn active_project_for(&self, path: &Path) -> Option<ProjectId> {
        let key = workspace_key(path);
        self.active_project_by_workspace
            .get(&key)
            .copied()
            .or_else(|| {
                let raw_key = path.to_string_lossy().into_owned();
                self.active_project_by_workspace.get(&raw_key).copied()
            })
    }

    pub fn set_active_project_for(&mut self, path: &Path, project_id: ProjectId) {
        self.active_project_by_workspace
            .insert(workspace_key(path), project_id);
    }
}

fn canonical_or_original(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn workspace_key(path: &Path) -> String {
    canonical_or_original(path).to_string_lossy().into_owned()
}

pub fn default_preferences_path() -> Result<PathBuf, DomainError> {
    let dirs = ProjectDirs::from("nz", "thealternator", "ProTrack4").ok_or_else(|| {
        DomainError::Storage("unable to resolve OS application config directory".to_string())
    })?;
    Ok(dirs.config_dir().join("preferences.json"))
}

pub fn default_workspaces_dir() -> PathBuf {
    if let Some(user_dirs) = UserDirs::new() {
        if let Some(docs) = user_dirs.document_dir() {
            return docs.join("ProTrack4");
        }
        return user_dirs.home_dir().join("ProTrack4");
    }
    PathBuf::from(".")
}
