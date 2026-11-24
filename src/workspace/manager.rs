use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use fs4::fs_std::FileExt;
use rusqlite::{Connection, OpenFlags};

use crate::db::{
    DbService, checkpoint_and_switch_to_delete_mode, inspect_and_migrate, load_workspace_meta,
    map_sqlite_err, seed_sample_project,
};
use crate::domain::{DomainError, WorkspaceMeta};

static SHUTDOWN_SIGNAL_RECEIVED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn handle_shutdown_signal(_sig: i32) {
    SHUTDOWN_SIGNAL_RECEIVED.store(true, Ordering::SeqCst);
}

/// Installs async-signal-safe `SIGINT` and `SIGTERM` handlers that flag the main loop to flush
/// SQLite WAL (`PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE;`) and exit cleanly.
pub fn install_signal_handlers() {
    #[cfg(unix)]
    unsafe {
        unsafe extern "C" {
            fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
        }
        const SIGINT: i32 = 2;
        const SIGTERM: i32 = 15;
        signal(SIGINT, handle_shutdown_signal);
        signal(SIGTERM, handle_shutdown_signal);
    }
}

/// Returns `true` once if a shutdown signal (`SIGINT` / `SIGTERM`) was received.
pub fn take_shutdown_signal() -> bool {
    SHUTDOWN_SIGNAL_RECEIVED.swap(false, Ordering::SeqCst)
}

pub fn lock_path_for(path: &Path) -> PathBuf {
    let mut os = OsString::from(path.as_os_str());
    os.push(".lock");
    PathBuf::from(os)
}

pub fn wal_path_for(path: &Path) -> PathBuf {
    let mut os = OsString::from(path.as_os_str());
    os.push("-wal");
    PathBuf::from(os)
}

pub fn shm_path_for(path: &Path) -> PathBuf {
    let mut os = OsString::from(path.as_os_str());
    os.push("-shm");
    PathBuf::from(os)
}

pub struct WorkspaceHandle {
    pub path: PathBuf,
    pub meta: WorkspaceMeta,
    pub db: DbService,
    lock_path: PathBuf,
    lock_file: Option<File>,
    closed: bool,
}

impl WorkspaceHandle {
    pub fn close(&mut self) -> Result<(), DomainError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;

        let db_res = self.db.shutdown();

        if let Some(lock_file) = self.lock_file.take() {
            let _ = fs::remove_file(&self.lock_path);
            let _ = FileExt::unlock(&lock_file);
            drop(lock_file);
        }

        db_res
    }
}

impl Drop for WorkspaceHandle {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[derive(Default)]
pub struct WorkspaceManager {
    active: Option<WorkspaceHandle>,
}

impl WorkspaceManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active(&self) -> Option<&WorkspaceHandle> {
        self.active.as_ref()
    }

    pub fn active_path(&self) -> Option<&Path> {
        self.active.as_ref().map(|handle| handle.path.as_path())
    }

    pub fn open_handle(
        path: &Path,
        create_if_missing: bool,
        workspace_name: Option<&str>,
        seed_sample: bool,
    ) -> Result<WorkspaceHandle, DomainError> {
        if !create_if_missing && !path.exists() {
            return Err(DomainError::WorkspaceNotFound {
                path: path.display().to_string(),
            });
        }

        if create_if_missing
            && path.exists()
            && fs::metadata(path).is_ok_and(|meta| meta.len() > 0)
        {
            return Err(DomainError::Validation(format!(
                "A non-empty file already exists at '{}'. Use Open Existing .protrack… or choose a new filename.",
                path.display()
            )));
        }

        if create_if_missing && let Some(parent) = path.parent() && !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| {
                DomainError::Storage(format!(
                    "failed to create workspace directory '{}': {err}",
                    parent.display()
                ))
            })?;
        }

        let lock_path = lock_path_for(path);
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|err| {
                DomainError::Storage(format!(
                    "failed to open advisory lock file '{}': {err}",
                    lock_path.display()
                ))
            })?;

        if let Err(err) = lock_file.try_lock_exclusive() {
            if err.kind() == fs4::lock_contended_error().kind()
                || err.kind() == std::io::ErrorKind::WouldBlock
            {
                return Err(DomainError::WorkspaceLocked {
                    path: path.display().to_string(),
                });
            }
            return Err(DomainError::Storage(format!(
                "failed to lock workspace '{}': {err}",
                lock_path.display()
            )));
        }

        let flags = if create_if_missing {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        };

        let mut conn = match Connection::open_with_flags(path, flags) {
            Ok(conn) => conn,
            Err(err) => {
                release_lock_without_opening(lock_file, &lock_path);
                return Err(map_sqlite_err(err));
            }
        };

        let derived_name = workspace_name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .or_else(|| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "Personal Workspace".to_string());

        let meta = match inspect_and_migrate(&mut conn, Some(path), &derived_name) {
            Ok(meta) => meta,
            Err(err) => {
                drop(conn);
                release_lock_without_opening(lock_file, &lock_path);
                return Err(err);
            }
        };

        let meta = if create_if_missing && seed_sample {
            if let Err(err) = seed_sample_project(&mut conn) {
                let _ = checkpoint_and_switch_to_delete_mode(&conn);
                drop(conn);
                release_lock_without_opening(lock_file, &lock_path);
                return Err(err);
            }
            match load_workspace_meta(&conn) {
                Ok(updated) => updated,
                Err(_) => meta,
            }
        } else {
            meta
        };

        let db = match DbService::spawn(conn) {
            Ok(db) => db,
            Err(err) => {
                release_lock_without_opening(lock_file, &lock_path);
                return Err(err);
            }
        };

        let canonical_path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

        Ok(WorkspaceHandle {
            path: canonical_path,
            meta,
            db,
            lock_path,
            lock_file: Some(lock_file),
            closed: false,
        })
    }

    pub fn install_handle(&mut self, handle: WorkspaceHandle) -> Result<&WorkspaceHandle, DomainError> {
        let mut previous = self.active.take();
        if let Some(mut old) = previous.take() {
            old.close()?;
        }
        self.active = Some(handle);
        Ok(self.active.as_ref().expect("active handle just installed"))
    }

    pub fn take_active(&mut self) -> Option<WorkspaceHandle> {
        self.active.take()
    }

    pub fn create_workspace(
        &mut self,
        path: impl AsRef<Path>,
        workspace_name: Option<&str>,
        seed_sample: bool,
    ) -> Result<&WorkspaceHandle, DomainError> {
        let handle = Self::open_handle(path.as_ref(), true, workspace_name, seed_sample)?;
        self.install_handle(handle)
    }

    pub fn open_workspace(&mut self, path: impl AsRef<Path>) -> Result<&WorkspaceHandle, DomainError> {
        let target_path = path.as_ref();
        if let Some(active) = &self.active {
            let target_canon = fs::canonicalize(target_path).unwrap_or_else(|_| target_path.to_path_buf());
            if active.path == target_canon {
                return Ok(self.active.as_ref().expect("active workspace present"));
            }
        }

        let handle = Self::open_handle(target_path, false, None, false)?;
        self.install_handle(handle)
    }

    pub fn close_workspace(&mut self) -> Result<(), DomainError> {
        if let Some(mut handle) = self.active.take() {
            handle.close()?;
        }
        Ok(())
    }
}

fn release_lock_without_opening(lock_file: File, lock_path: &Path) {
    let _ = fs::remove_file(lock_path);
    let _ = FileExt::unlock(&lock_file);
    drop(lock_file);
}
