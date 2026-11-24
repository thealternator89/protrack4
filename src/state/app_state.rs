use std::fs;
use std::path::{Path, PathBuf};

use gpui_kit::{AppContext as _, Context, Entity, Task};

use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::{DbService, DomainEventReceiver, SearchResultItem};
use crate::domain::{DomainError, DomainEvent, ItemId, ItemKind, ProjectId};
use crate::state::{CenterViewTab, ProjectStore, ThemePreference};
use crate::workspace::{
    AppPreferences, WorkspaceManager, default_preferences_path, normalize_protrack_path,
};

#[derive(Debug, Clone, Default)]
pub struct LaunchConfig {
    pub db_override: Option<PathBuf>,
    pub preferences_path: Option<PathBuf>,
}

pub struct AppState {
    preferences_path: Option<PathBuf>,
    preferences: AppPreferences,
    workspace_manager: WorkspaceManager,
    db_override_active: bool,
    custom_preferences_path: bool,
    is_loading: bool,
    needs_welcome_modal: bool,
    last_error: Option<String>,
    desired_project_id: Option<ProjectId>,
    project_store: Entity<ProjectStore>,
    event_listener_task: Option<Task<()>>,
    pending_workspace_task: Option<Task<()>>,
    request_generation: u64,
}

impl AppState {
    pub fn new(config: LaunchConfig, cx: &mut Context<Self>) -> Self {
        let custom_preferences_path = config.preferences_path.is_some();
        let preferences_path = config
            .preferences_path
            .or_else(|| default_preferences_path().ok());

        let preferences = preferences_path
            .as_deref()
            .map(AppPreferences::load_from)
            .unwrap_or_default();

        let project_store = cx.new(|_| ProjectStore::new_from_preferences(&preferences));

        Self {
            preferences_path,
            preferences,
            workspace_manager: WorkspaceManager::new(),
            db_override_active: config.db_override.is_some(),
            custom_preferences_path,
            is_loading: false,
            needs_welcome_modal: false,
            last_error: None,
            desired_project_id: None,
            project_store,
            event_listener_task: None,
            pending_workspace_task: None,
            request_generation: 0,
        }
    }

    pub fn project_store(&self) -> &Entity<ProjectStore> {
        &self.project_store
    }

    pub fn preferences(&self) -> &AppPreferences {
        &self.preferences
    }

    pub fn preferences_path(&self) -> Option<&Path> {
        self.preferences_path.as_deref()
    }

    pub fn is_loading(&self) -> bool {
        self.is_loading
    }

    pub fn needs_welcome_modal(&self) -> bool {
        self.needs_welcome_modal
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn clear_error(&mut self, cx: &mut Context<Self>) {
        if self.last_error.take().is_some() {
            cx.notify();
        }
    }

    pub fn clear_welcome_flag(&mut self) {
        self.needs_welcome_modal = false;
    }

    pub fn has_open_workspace(&self) -> bool {
        self.workspace_manager.active().is_some()
    }

    pub fn active_workspace_path(&self) -> Option<&Path> {
        self.workspace_manager.active_path()
    }

    pub fn startup_open(&mut self, db_override: Option<PathBuf>, cx: &mut Context<Self>) {
        if let Some(override_path) = db_override {
            let create_if_missing = !override_path.exists();
            let task = if create_if_missing {
                self.create_workspace(override_path, false, false, cx)
            } else {
                self.open_workspace(override_path, false, cx)
            };
            self.pending_workspace_task = Some(cx.spawn(async move |_, _| {
                let _ = task.await;
            }));
            return;
        }

        if let Some(last_path) = self.preferences.last_workspace_path.clone() {
            if last_path.exists() {
                let task = self.open_workspace(last_path, true, cx);
                self.pending_workspace_task = Some(cx.spawn(async move |_, _| {
                    let _ = task.await;
                }));
                return;
            }
            self.last_error = Some(format!(
                "Last workspace file was not found: {}",
                last_path.display()
            ));
        }

        self.needs_welcome_modal = true;
        cx.notify();
    }

    pub fn open_workspace(
        &mut self,
        path: PathBuf,
        record_recents: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), DomainError>> {
        self.open_or_create_workspace(path, false, false, record_recents, cx)
    }

    pub fn create_workspace(
        &mut self,
        path: PathBuf,
        seed_sample: bool,
        record_recents: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), DomainError>> {
        let normalized = normalize_protrack_path(&path);
        self.open_or_create_workspace(normalized, true, seed_sample, record_recents, cx)
    }

    pub fn seed_sample_project_in_active(&mut self, cx: &mut Context<Self>) -> Task<Result<(), DomainError>> {
        let Some(handle) = self.workspace_manager.active() else {
            return Task::ready(Err(DomainError::Validation(
                "No workspace is currently open".to_string(),
            )));
        };
        let db = handle.db.clone();
        let active_path = handle.path.clone();

        cx.spawn(async move |this, cx| {
            let project_id = db.seed_sample_project().await?;
            let overview = db.load_workspace_overview(Some(project_id)).await?;
            this.update(cx, |app, cx| {
                app.desired_project_id = Some(project_id);
                app.preferences
                    .set_active_project_for(&active_path, project_id);
                app.persist_preferences_if_allowed();
                app.project_store.update(cx, |store, cx| {
                    store.apply_workspace_overview(active_path, overview);
                    cx.notify();
                });
                cx.notify();
            })
            .map_err(|_| DomainError::Storage("AppState dropped before seed completed".into()))?;
            Ok(())
        })
    }

    pub fn switch_project(
        &mut self,
        project_id: ProjectId,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), DomainError>> {
        let Some(handle) = self.workspace_manager.active() else {
            return Task::ready(Err(DomainError::Validation(
                "No workspace is currently open".to_string(),
            )));
        };
        let db = handle.db.clone();
        let active_path = handle.path.clone();

        self.desired_project_id = Some(project_id);
        self.preferences
            .set_active_project_for(&active_path, project_id);
        self.persist_preferences_if_allowed();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let overview = db.load_workspace_overview(Some(project_id)).await?;
            this.update(cx, |app, cx| {
                if app.desired_project_id == Some(project_id)
                    && app.active_workspace_path() == Some(active_path.as_path())
                {
                    app.project_store.update(cx, |store, cx| {
                        store.apply_workspace_overview(active_path, overview);
                        cx.notify();
                    });
                    cx.notify();
                }
            })
            .map_err(|_| {
                DomainError::Storage("AppState dropped before project switch completed".into())
            })?;
            Ok(())
        })
    }

    pub fn search_items_fts(
        &self,
        query: String,
        project_id: Option<ProjectId>,
        limit: usize,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<SearchResultItem>, DomainError>> {
        let Some(handle) = self.workspace_manager.active() else {
            return Task::ready(Ok(Vec::new()));
        };
        let db = handle.db.clone();
        cx.spawn(async move |_, _| db.search_items_fts(query, project_id, limit).await)
    }

    pub fn jump_to_item(
        &mut self,
        project_id: ProjectId,
        item_id: ItemId,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), DomainError>> {
        let current_project_id = self
            .project_store
            .read(cx)
            .snapshot()
            .map(|snap| snap.project.id);

        if current_project_id == Some(project_id) {
            self.project_store.update(cx, |store, cx| {
                apply_jump_selection(store, item_id);
                cx.notify();
            });
            return Task::ready(Ok(()));
        }

        let Some(handle) = self.workspace_manager.active() else {
            return Task::ready(Err(DomainError::Validation(
                "No workspace is currently open".to_string(),
            )));
        };
        let db = handle.db.clone();
        let active_path = handle.path.clone();

        self.desired_project_id = Some(project_id);
        self.preferences
            .set_active_project_for(&active_path, project_id);
        self.persist_preferences_if_allowed();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let overview = db.load_workspace_overview(Some(project_id)).await?;
            this.update(cx, |app, cx| {
                if app.desired_project_id == Some(project_id)
                    && app.active_workspace_path() == Some(active_path.as_path())
                {
                    app.project_store.update(cx, |store, cx| {
                        store.apply_workspace_overview(active_path, overview);
                        apply_jump_selection(store, item_id);
                        cx.notify();
                    });
                    cx.notify();
                }
            })
            .map_err(|_| DomainError::Storage("AppState dropped before jump completed".into()))?;
            Ok(())
        })
    }

    pub fn execute_command(
        &mut self,
        command: Box<dyn UndoableCommand>,
        cx: &mut Context<Self>,
    ) -> Task<Result<CommandEffect, DomainError>> {
        let Some(handle) = self.workspace_manager.active() else {
            return Task::ready(Err(DomainError::Validation(
                "No workspace is currently open".to_string(),
            )));
        };
        let db = handle.db.clone();
        let active_path = handle.path.clone();

        cx.spawn(async move |this, cx| {
            let effect = db.execute_command(command).await?;
            let target_project_id = this
                .update(cx, |app, cx| {
                    if let Some(created_pid) = effect.created_project_id
                        && app.active_workspace_path() == Some(active_path.as_path())
                    {
                        app.desired_project_id = Some(created_pid);
                        app.preferences
                            .set_active_project_for(&active_path, created_pid);
                        app.persist_preferences_if_allowed();
                    }

                    effect
                        .created_project_id
                        .or(app.desired_project_id)
                        .or_else(|| app.project_store.read(cx).snapshot().map(|s| s.project.id))
                })
                .map_err(|_| {
                    DomainError::Storage("AppState dropped before command refresh".into())
                })?;

            let overview = db.load_workspace_overview(target_project_id).await?;
            this.update(cx, |app, cx| {
                if app.active_workspace_path() != Some(active_path.as_path()) {
                    return;
                }
                if let Some(wanted) = app.desired_project_id
                    && overview.projects.iter().any(|p| p.id == wanted)
                    && overview.active_snapshot.as_ref().map(|s| s.project.id) != Some(wanted)
                {
                    return;
                }
                if let Some(snap) = &overview.active_snapshot {
                    app.desired_project_id = Some(snap.project.id);
                    app.preferences
                        .set_active_project_for(&active_path, snap.project.id);
                }

                app.project_store.update(cx, |store, cx| {
                    store.apply_workspace_overview(active_path, overview);
                    if let Some(created_id) = effect.created_item_id
                        && store
                            .snapshot()
                            .and_then(|s| s.item(created_id))
                            .is_some_and(|item| {
                                item.is_active()
                                    && item.kind == crate::domain::ItemKind::Feature
                            })
                    {
                        store.expand_feature(created_id);
                    }
                    if let Some(item_id) = effect.selected_item_id {
                        store.expand_ancestors_of(item_id);
                        store.select_item(Some(item_id));
                    }
                    cx.notify();
                });
                cx.notify();
            })
            .map_err(|_| DomainError::Storage("AppState dropped before UI refresh".into()))?;

            Ok(effect)
        })
    }

    /// Synchronously executes a command on the active DB actor during window-close flush.
    pub fn execute_command_for_shutdown(
        &self,
        command: Box<dyn UndoableCommand>,
    ) -> Result<CommandEffect, DomainError> {
        let Some(handle) = self.workspace_manager.active() else {
            return Err(DomainError::Validation(
                "No workspace is currently open".to_string(),
            ));
        };
        handle.db.execute_command_blocking(command)
    }

    fn open_or_create_workspace(
        &mut self,
        path: PathBuf,
        create_if_missing: bool,
        seed_sample: bool,
        record_recents: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), DomainError>> {
        if !create_if_missing && let Some(active) = self.workspace_manager.active() {
            let target_canon = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if active.path == target_canon {
                let db = active.db.clone();
                let active_path = active.path.clone();
                let preferred = self.preferences.active_project_for(&active_path);
                self.desired_project_id = preferred;
                self.request_generation += 1;
                let req_gen = self.request_generation;
                self.is_loading = true;
                self.last_error = None;
                cx.notify();

                return cx.spawn(async move |this, cx| {
                    let overview = db.load_workspace_overview(preferred).await?;
                    this.update(cx, |app, cx| {
                        if app.request_generation != req_gen {
                            return;
                        }
                        if record_recents && !app.db_override_active {
                            app.preferences.record_workspace_opened(
                                &active_path,
                                &overview.meta.workspace_name,
                                true,
                            );
                            app.persist_preferences_if_allowed();
                        }
                        app.project_store.update(cx, |store, cx| {
                            store.apply_workspace_overview(active_path, overview);
                            cx.notify();
                        });
                        app.is_loading = false;
                        app.needs_welcome_modal = false;
                        app.last_error = None;
                        cx.notify();
                    })
                    .map_err(|_| {
                        DomainError::Storage("AppState dropped before refresh completed".into())
                    })?;
                    Ok(())
                });
            }
        }

        self.request_generation += 1;
        let req_gen = self.request_generation;
        self.is_loading = true;
        self.last_error = None;
        cx.notify();

        let preferred_project_id = self.preferences.active_project_for(&path);
        self.desired_project_id = preferred_project_id;
        let bg_path = path.clone();

        cx.spawn(async move |this, cx| {
            let handle_res = cx
                .background_executor()
                .spawn(async move {
                    WorkspaceManager::open_handle(&bg_path, create_if_missing, None, seed_sample)
                })
                .await;

            let mut handle = match handle_res {
                Ok(handle) => handle,
                Err(err) => {
                    let _ = this.update(cx, |app, cx| {
                        if app.request_generation == req_gen {
                            app.is_loading = false;
                            app.last_error = Some(err.to_string());
                            app.needs_welcome_modal = !app.has_open_workspace();
                            cx.notify();
                        }
                    });
                    return Err(err);
                }
            };

            let overview_res = handle.db.load_workspace_overview(preferred_project_id).await;
            let overview = match overview_res {
                Ok(overview) => overview,
                Err(err) => {
                    cx.background_executor()
                        .spawn(async move {
                            let _ = handle.close();
                        })
                        .await;
                    let _ = this.update(cx, |app, cx| {
                        if app.request_generation == req_gen {
                            app.is_loading = false;
                            app.last_error = Some(err.to_string());
                            app.needs_welcome_modal = !app.has_open_workspace();
                            cx.notify();
                        }
                    });
                    return Err(err);
                }
            };

            let opened_path = handle.path.clone();
            let db_service = handle.db.clone();
            let events_rx = db_service.subscribe_events();

            enum SwapOutcome {
                Installed(Option<crate::workspace::WorkspaceHandle>),
                Superseded(crate::workspace::WorkspaceHandle),
            }

            let outcome = this
                .update(cx, |app, cx| {
                    if app.request_generation != req_gen {
                        return SwapOutcome::Superseded(handle);
                    }

                    let old_handle = app.workspace_manager.take_active();
                    if let Err(err) = app.workspace_manager.install_handle(handle) {
                        tracing::error!(error = %err, "Failed to install new workspace handle");
                    }

                    if record_recents {
                        app.db_override_active = false;
                        app.preferences.record_workspace_opened(
                            &opened_path,
                            &overview.meta.workspace_name,
                            true,
                        );
                    }
                    if let Some(snap) = &overview.active_snapshot {
                        app.desired_project_id = Some(snap.project.id);
                        app.preferences
                            .set_active_project_for(&opened_path, snap.project.id);
                    } else {
                        app.desired_project_id = None;
                    }
                    if record_recents || !app.db_override_active || app.custom_preferences_path {
                        app.save_preferences();
                    }

                    app.project_store.update(cx, |store, cx| {
                        store.apply_workspace_overview(opened_path, overview);
                        cx.notify();
                    });

                    app.spawn_event_listener(db_service, events_rx, cx);
                    app.is_loading = false;
                    app.needs_welcome_modal = false;
                    app.last_error = None;
                    cx.notify();

                    SwapOutcome::Installed(old_handle)
                })
                .map_err(|_| DomainError::Storage("AppState dropped before open completed".into()))?;

            match outcome {
                SwapOutcome::Installed(Some(mut old)) => {
                    cx.background_executor()
                        .spawn(async move {
                            let _ = old.close();
                        })
                        .await;
                }
                SwapOutcome::Installed(None) => {}
                SwapOutcome::Superseded(mut stale) => {
                    cx.background_executor()
                        .spawn(async move {
                            let _ = stale.close();
                        })
                        .await;
                }
            }

            Ok(())
        })
    }

    fn spawn_event_listener(
        &mut self,
        db: DbService,
        rx: DomainEventReceiver,
        cx: &mut Context<Self>,
    ) {
        self.event_listener_task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                let preferred_project_id = match this.read_with(cx, |app, cx| {
                    app.desired_project_id
                        .or_else(|| app.project_store.read(cx).snapshot().map(|snap| snap.project.id))
                        .or_else(|| {
                            app.active_workspace_path()
                                .and_then(|p| app.preferences.active_project_for(p))
                        })
                }) {
                    Ok(id) => id,
                    Err(_) => break,
                };

                let target_project = match event {
                    DomainEvent::ItemsChanged { project_id }
                    | DomainEvent::TrashChanged { project_id }
                    | DomainEvent::TagsChanged { project_id } => preferred_project_id.or(Some(project_id)),
                    DomainEvent::WorkspaceSwitched
                    | DomainEvent::ProjectsChanged
                    | DomainEvent::AssigneesChanged => preferred_project_id,
                };

                match db.load_workspace_overview(target_project).await {
                    Ok(overview) => {
                        let _ = this.update(cx, |app, cx| {
                            if let Some(wanted) = app.desired_project_id
                                && overview.projects.iter().any(|p| p.id == wanted)
                                && overview.active_snapshot.as_ref().map(|s| s.project.id)
                                    != Some(wanted)
                            {
                                return;
                            }
                            if let Some(path) = app.active_workspace_path().map(Path::to_path_buf)
                            {
                                app.project_store.update(cx, |store, cx| {
                                    store.apply_workspace_overview(path, overview);
                                    cx.notify();
                                });
                            }
                        });
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "Failed to refresh snapshot on DomainEvent");
                    }
                }
            }
        }));
    }

    pub fn set_theme_preference(&mut self, pref: ThemePreference, cx: &mut Context<Self>) {
        self.preferences.theme = pref;
        self.project_store.update(cx, |store, cx| {
            store.set_theme_preference(pref);
            cx.notify();
        });
        self.persist_preferences_if_allowed();
        cx.notify();
    }

    pub fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.preferences.sidebar_collapsed = !open;
        self.project_store.update(cx, |store, cx| {
            store.set_sidebar_open(open);
            cx.notify();
        });
        self.persist_preferences_if_allowed();
        cx.notify();
    }

    pub fn set_inspector_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.preferences.inspector_open = open;
        self.project_store.update(cx, |store, cx| {
            store.set_inspector_open(open);
            cx.notify();
        });
        self.persist_preferences_if_allowed();
        cx.notify();
    }

    pub fn set_panel_widths(&mut self, sidebar_width: f32, inspector_width: f32) {
        self.preferences.sidebar_width = sidebar_width;
        self.preferences.inspector_width = inspector_width;
        self.preferences.clamp_widths();
    }

    pub fn remove_recent_workspace(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.preferences.remove_recent_workspace(path);
        self.persist_preferences_if_allowed();
        cx.notify();
    }

    pub fn save_preferences(&self) {
        let Some(path) = &self.preferences_path else {
            return;
        };
        if let Err(err) = self.preferences.save_to(path) {
            tracing::warn!(path = %path.display(), error = %err, "Failed to save preferences.json");
        }
    }

    fn persist_preferences_if_allowed(&self) {
        if !self.db_override_active || self.custom_preferences_path {
            self.save_preferences();
        }
    }

    pub fn close_workspace_sync(&mut self, cx: &mut Context<Self>) -> Result<(), DomainError> {
        self.persist_preferences_if_allowed();
        self.desired_project_id = None;
        self.event_listener_task.take();
        self.pending_workspace_task.take();
        let res = self.workspace_manager.close_workspace();
        self.project_store.update(cx, |store, cx| {
            store.clear_workspace();
            cx.notify();
        });
        cx.notify();
        res
    }
}

fn apply_jump_selection(store: &mut ProjectStore, item_id: ItemId) {
    let is_unallocated_wi = store.snapshot().is_some_and(|snap| {
        snap.item(item_id).is_some_and(|item| {
            item.is_active()
                && item.kind == ItemKind::WorkItem
                && snap.features_for_work_item(item_id).is_empty()
        })
    });

    if is_unallocated_wi {
        store.set_active_tab(CenterViewTab::Unallocated);
    } else {
        store.set_active_tab(CenterViewTab::Outline);
        store.expand_ancestors_of(item_id);
        if store.snapshot().is_some_and(|snap| {
            snap.item(item_id)
                .is_some_and(|item| item.is_active() && item.kind == ItemKind::Feature)
        }) {
            store.expand_feature(item_id);
        }
    }
    store.select_item(Some(item_id));
}
