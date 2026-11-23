pub mod manager;
pub mod preferences;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub use manager::{WorkspaceHandle, WorkspaceManager, lock_path_for, shm_path_for, wal_path_for};
pub use preferences::{
    AppPreferences, RecentWorkspace, default_preferences_path, default_workspaces_dir,
};

/// Ensures workspace file paths end with `.protrack`.
pub fn normalize_protrack_path(path: &Path) -> PathBuf {
    let has_ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("protrack"));

    if has_ext {
        path.to_path_buf()
    } else {
        let mut os = OsString::from(path.as_os_str());
        os.push(".protrack");
        PathBuf::from(os)
    }
}

/// Parses `--db <path>` or `--db=<path>` from CLI arguments (including argv[0] at index 0).
pub fn parse_cli_db_arg<I, S>(args: I) -> Result<Option<PathBuf>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = args.into_iter().skip(1);
    let mut found: Option<PathBuf> = None;

    while let Some(arg) = iter.next() {
        let s = arg.as_ref();
        if s == "--db" {
            let Some(value) = iter.next() else {
                return Err("missing path argument after --db".to_string());
            };
            let v = value.as_ref().trim();
            if v.is_empty() {
                return Err("empty path argument after --db".to_string());
            }
            found = Some(PathBuf::from(v));
        } else if let Some(value) = s.strip_prefix("--db=") {
            let v = value.trim();
            if v.is_empty() {
                return Err("empty path argument in --db=<path>".to_string());
            }
            found = Some(PathBuf::from(v));
        }
    }

    Ok(found)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::*;
    use crate::db::migrations::checkpoint_and_switch_to_delete_mode;
    use crate::domain::{DomainError, ProjectId};
    use crate::state::ThemePreference;

    #[test]
    fn cli_arg_and_path_normalization() {
        assert_eq!(
            normalize_protrack_path(Path::new("/tmp/work")),
            PathBuf::from("/tmp/work.protrack")
        );
        assert_eq!(
            normalize_protrack_path(Path::new("/tmp/work.protrack")),
            PathBuf::from("/tmp/work.protrack")
        );
        assert_eq!(
            normalize_protrack_path(Path::new("/tmp/work.PROTRACK")),
            PathBuf::from("/tmp/work.PROTRACK")
        );

        assert_eq!(
            parse_cli_db_arg(["protrack4", "--db", "/tmp/a.protrack"]).unwrap(),
            Some(PathBuf::from("/tmp/a.protrack"))
        );
        assert_eq!(
            parse_cli_db_arg(["protrack4", "--db=/tmp/b.protrack"]).unwrap(),
            Some(PathBuf::from("/tmp/b.protrack"))
        );
        assert_eq!(parse_cli_db_arg(["protrack4"]).unwrap(), None);
        assert!(parse_cli_db_arg(["protrack4", "--db"]).is_err());
        assert!(parse_cli_db_arg(["protrack4", "--db="]).is_err());
    }

    #[test]
    fn preferences_round_trip_corrupt_fallback_and_recents_management() {
        let dir = TempDir::new().unwrap();
        let pref_path = dir.path().join("nested/preferences.json");

        // Missing file loads defaults
        let mut prefs = AppPreferences::load_from(&pref_path);
        assert_eq!(prefs.theme, ThemePreference::System);
        assert_eq!(prefs.sidebar_width, 260.0);
        assert_eq!(prefs.inspector_width, 380.0);

        let ws_a = dir.path().join("alpha.protrack");
        let ws_b = dir.path().join("beta.protrack");
        fs::write(&ws_a, b"").unwrap();
        fs::write(&ws_b, b"").unwrap();

        let proj_a = ProjectId::new();
        prefs.theme = ThemePreference::Dark;
        prefs.sidebar_width = 50.0; // clamps to 200.0
        prefs.inspector_width = 900.0; // clamps to 540.0
        prefs.sidebar_collapsed = true;
        prefs.inspector_open = false;
        prefs.record_workspace_opened(&ws_a, "Alpha", true);
        prefs.record_workspace_opened(&ws_b, "Beta", true);
        prefs.record_workspace_opened(&ws_a, "Alpha Renamed", true); // dedup + move to front
        prefs.set_active_project_for(&ws_a, proj_a);
        prefs.save_to(&pref_path).unwrap();

        let loaded = AppPreferences::load_from(&pref_path);
        assert_eq!(loaded.theme, ThemePreference::Dark);
        assert_eq!(loaded.sidebar_width, 200.0);
        assert_eq!(loaded.inspector_width, 540.0);
        assert!(loaded.sidebar_collapsed);
        assert!(!loaded.inspector_open);
        assert_eq!(loaded.recent_workspaces.len(), 2);
        assert_eq!(loaded.recent_workspaces[0].name, "Alpha Renamed");
        assert_eq!(loaded.active_project_for(&ws_a), Some(proj_a));

        // Corrupt JSON falls back cleanly to defaults
        fs::write(&pref_path, "{ not valid json").unwrap();
        let fallback = AppPreferences::load_from(&pref_path);
        assert_eq!(fallback, AppPreferences::default());
    }

    #[test]
    fn workspace_manager_create_switch_lock_forward_guard_and_clean_delete_mode() {
        let dir = TempDir::new().unwrap();
        let path_a = dir.path().join("alpha.protrack");
        let path_b = dir.path().join("beta.protrack");
        let path_future = dir.path().join("future.protrack");

        // Prepare a future v999 workspace file
        {
            let mut manager = WorkspaceManager::new();
            manager.create_workspace(&path_future, Some("Future"), false).unwrap();
            manager.close_workspace().unwrap();
            let conn = Connection::open(&path_future).unwrap();
            conn.execute("UPDATE workspace_meta SET schema_version = 999 WHERE id = 1;", [])
                .unwrap();
            checkpoint_and_switch_to_delete_mode(&conn).unwrap();
        }
        let future_bytes = fs::read(&path_future).unwrap();

        let mut manager = WorkspaceManager::new();

        // Create workspace A with starter sample project
        let handle_a = manager
            .create_workspace(&path_a, Some("Alpha"), true)
            .unwrap();
        assert_eq!(handle_a.meta.schema_version, 1);
        assert!(lock_path_for(&path_a).exists());

        let overview_a = handle_a.db.load_workspace_overview_blocking(None).unwrap();
        let snap_a = overview_a.active_snapshot.unwrap();
        assert_eq!(snap_a.projects.len(), 1);
        assert_eq!(snap_a.items.len(), 9);
        assert_eq!(snap_a.unallocated_work_items().len(), 2);

        // Concurrent open on already-locked workspace A returns WorkspaceLocked
        let lock_err = WorkspaceManager::open_handle(&path_a, false, None, false)
            .err()
            .expect("concurrent handle must fail with lock error");
        assert!(matches!(lock_err, DomainError::WorkspaceLocked { .. }));

        // Switch to empty workspace B (0 projects)
        let handle_b = manager.create_workspace(&path_b, Some("Beta"), false).unwrap();
        let overview_b = handle_b.db.load_workspace_overview_blocking(None).unwrap();
        assert!(overview_b.projects.is_empty());
        assert!(overview_b.active_snapshot.is_none());

        // Workspace A must now have ZERO lingering -wal, -shm, or .lock files and be in DELETE mode
        assert!(!wal_path_for(&path_a).exists());
        assert!(!shm_path_for(&path_a).exists());
        assert!(!lock_path_for(&path_a).exists());
        let bytes_a = fs::read(&path_a).unwrap();
        assert_eq!(bytes_a[18], 1);
        assert_eq!(bytes_a[19], 1);

        // Attempting to switch to v999 workspace must fail cleanly without mutating file
        // and leave workspace B active
        let forward_err = manager.open_workspace(&path_future).err().unwrap();
        assert_eq!(
            forward_err,
            DomainError::ForwardSchemaVersion {
                file_version: 999,
                max_supported: 1,
            }
        );
        assert_eq!(fs::read(&path_future).unwrap(), future_bytes);
        assert!(!wal_path_for(&path_future).exists());
        assert!(!shm_path_for(&path_future).exists());
        assert!(!lock_path_for(&path_future).exists());
        assert!(manager.active().is_some());

        // Close workspace B cleanly -> zero lingering companion files
        manager.close_workspace().unwrap();
        assert!(!wal_path_for(&path_b).exists());
        assert!(!shm_path_for(&path_b).exists());
        assert!(!lock_path_for(&path_b).exists());

        // Reopen workspace A -> data persists
        let reopened_a = manager.open_workspace(&path_a).unwrap();
        let reopened_overview = reopened_a.db.load_workspace_overview_blocking(None).unwrap();
        assert_eq!(reopened_overview.active_snapshot.unwrap().items.len(), 9);
        manager.close_workspace().unwrap();
        assert!(!wal_path_for(&path_a).exists());
        assert!(!shm_path_for(&path_a).exists());
    }
}

