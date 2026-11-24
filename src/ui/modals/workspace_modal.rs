use std::path::{Path, PathBuf};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    WindowExt as _, h_flex, notification::Notification, v_flex,
};
use gpui_kit::{
    App, AppContext as _, AsyncWindowContext, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, StatefulInteractiveElement as _, Styled as _, Task,
    TestSupportExt as _, Window, div, prelude::FluentBuilder as _, px,
};

use crate::domain::DomainError;
use crate::state::AppState;
use crate::workspace::{default_workspaces_dir, normalize_protrack_path};

pub struct WorkspaceModalState {
    pub seed_sample_project: bool,
    pub busy: bool,
}

async fn finish_modal_workspace_task(
    cx: &mut AsyncWindowContext,
    app_state: &Entity<AppState>,
    modal_state: &Entity<WorkspaceModalState>,
    task: Task<Result<(), DomainError>>,
    success_toast: Option<String>,
) {
    let outcome = task.await;
    modal_state.update(cx, |s, cx| {
        s.busy = false;
        cx.notify();
    });
    match outcome {
        Ok(()) => {
            let _ = cx.update(|window, cx| {
                window.close_dialog(cx);
                if let Some(msg) = success_toast {
                    window.push_notification(Notification::success(msg), cx);
                }
            });
        }
        Err(DomainError::WorkspaceLocked { path }) => {
            let app_state = app_state.clone();
            let locked_path = PathBuf::from(path);
            let _ = cx.update(|window, cx| {
                app_state.update(cx, |app, _| {
                    let _ = app.take_pending_locked_workspace();
                });
                window.close_dialog(cx);
                window.defer(cx, move |window, cx| {
                    open_workspace_locked_modal(app_state, locked_path, window, cx);
                });
            });
        }
        Err(_) => {
            let _ = cx.update(|window, _| window.refresh());
        }
    }
}

pub fn open_workspace_locked_modal(
    app_state: Entity<AppState>,
    locked_path: PathBuf,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }

    let modal_state = cx.new(|_| WorkspaceModalState {
        seed_sample_project: false,
        busy: false,
    });

    let app_state_for_dialog = app_state.clone();
    let modal_state_for_dialog = modal_state.clone();

    window.open_dialog(cx, move |dialog, _window, cx| {
        let has_open_workspace = app_state_for_dialog.read(cx).has_open_workspace();
        let app_state_content = app_state_for_dialog.clone();
        let locked_for_content = locked_path.clone();
        let app_state_footer = app_state_for_dialog.clone();
        let modal_state_footer = modal_state_for_dialog.clone();
        let locked_for_footer = locked_path.clone();
        let busy = modal_state_footer.read(cx).busy || app_state_footer.read(cx).is_loading();

        dialog
            .width(px(480.))
            .title("Workspace Is Currently Locked")
            .close_button(has_open_workspace)
            .overlay_closable(has_open_workspace)
            .keyboard(has_open_workspace)
            .content(move |content, _window, cx| {
                let error = app_state_content.read(cx).last_error().map(str::to_string);
                let warn = cx.theme().warning;
                let warn_bg = warn.opacity(if cx.theme().is_dark() { 0.16 } else { 0.10 });
                let warn_border = warn.opacity(if cx.theme().is_dark() { 0.42 } else { 0.30 });

                content.child(
                    v_flex()
                        .id("workspace-locked-modal-body")
                        .test_support()
                        .w_full()
                        .gap_3()
                        .child(
                            h_flex()
                                .w_full()
                                .p_3()
                                .gap_2p5()
                                .items_start()
                                .rounded(cx.theme().radius)
                                .bg(warn_bg)
                                .border_1()
                                .border_color(warn_border)
                                .child(
                                    Icon::new(IconName::TriangleAlert)
                                        .small()
                                        .text_color(warn),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_semibold()
                                                .child("This .protrack workspace is currently open in another ProTrack window."),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("To protect SQLite integrity and prevent concurrent write corruption, ProTrack holds an exclusive advisory lock while a workspace is open."),
                                        ),
                                ),
                        )
                        .child(
                            v_flex()
                                .w_full()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .font_semibold()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("LOCKED WORKSPACE FILE"),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .px_2p5()
                                        .py_1p5()
                                        .rounded(cx.theme().radius)
                                        .bg(cx.theme().muted)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .text_xs()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .child(locked_for_content.display().to_string()),
                                ),
                        )
                        .when_some(error, |this, err| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(err),
                            )
                        }),
                )
            })
            .footer(
                DialogFooter::new()
                    .when(has_open_workspace, |f| {
                        f.child(
                            Button::new("workspace-locked-close")
                                .ghost()
                                .label("Keep Current Workspace")
                                .disabled(busy)
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                }),
                        )
                    })
                    .child(
                        Button::new("workspace-locked-choose-another")
                            .outline()
                            .icon(IconName::FolderOpen)
                            .label("Choose Another Workspace…")
                            .disabled(busy)
                            .on_click({
                                let app_state = app_state_footer.clone();
                                move |_, window, cx| {
                                    app_state.update(cx, |app, cx| app.clear_error(cx));
                                    window.close_dialog(cx);
                                    let app_state = app_state.clone();
                                    window.defer(cx, move |window, cx| {
                                        open_workspace_modal(app_state, window, cx);
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("workspace-locked-retry")
                            .primary()
                            .label("Retry Opening")
                            .loading(busy)
                            .disabled(busy)
                            .on_click({
                                let app_state = app_state_footer.clone();
                                let modal_state = modal_state_footer.clone();
                                let locked_path = locked_for_footer.clone();
                                move |_, window, cx| {
                                    let app_state = app_state.clone();
                                    let modal_state = modal_state.clone();
                                    let locked_path = locked_path.clone();
                                    window
                                        .spawn(cx, async move |cx| {
                                            mark_modal_busy(cx, &modal_state);
                                            let task = app_state.update(cx, |app, cx| {
                                                app.open_workspace(locked_path.clone(), true, cx)
                                            });
                                            let outcome = task.await;
                                            modal_state.update(cx, |s, cx| {
                                                s.busy = false;
                                                cx.notify();
                                            });
                                            match outcome {
                                                Ok(()) => {
                                                    let label = locked_path
                                                        .file_name()
                                                        .and_then(|n| n.to_str())
                                                        .unwrap_or("workspace")
                                                        .to_string();
                                                    let _ = cx.update(|window, cx| {
                                                        window.close_dialog(cx);
                                                        window.push_notification(
                                                            Notification::success(format!(
                                                                "Opened {label}"
                                                            )),
                                                            cx,
                                                        );
                                                    });
                                                }
                                                Err(_) => {
                                                    app_state.update(cx, |app, _| {
                                                        let _ = app.take_pending_locked_workspace();
                                                    });
                                                    let _ = cx.update(|window, _| window.refresh());
                                                }
                                            }
                                        })
                                        .detach();
                                }
                            }),
                    ),
            )
    });
}

fn mark_modal_busy(cx: &mut AsyncWindowContext, modal_state: &Entity<WorkspaceModalState>) {
    modal_state.update(cx, |s, cx| {
        s.busy = true;
        cx.notify();
    });
    let _ = cx.update(|window, _| window.refresh());
}

pub fn open_workspace_modal(app_state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }

    let modal_state = cx.new(|_| WorkspaceModalState {
        seed_sample_project: true,
        busy: false,
    });

    let app_state_for_dialog = app_state.clone();
    let modal_state_for_dialog = modal_state.clone();

    window.open_dialog(cx, move |dialog, _window, cx| {
        let has_workspace = app_state_for_dialog.read(cx).has_open_workspace();
        let title = if has_workspace {
            "Switch Workspace"
        } else {
            "Welcome to ProTrack4"
        };

        let app_state_inner = app_state_for_dialog.clone();
        let modal_state_inner = modal_state_for_dialog.clone();

        dialog
            .width(px(520.))
            .title(title)
            .close_button(has_workspace)
            .overlay_closable(has_workspace)
            .keyboard(has_workspace)
            .content(move |content, _window, cx| {
                let app = app_state_inner.read(cx);
                let active_path = app.active_workspace_path().map(Path::to_path_buf);
                let recents = app.preferences().recent_workspaces.clone();
                let error = app.last_error().map(str::to_string);
                let is_loading = app.is_loading();

                let mstate = modal_state_inner.read(cx);
                let seed_sample = mstate.seed_sample_project;
                let busy = mstate.busy || is_loading;

                let subtitle = if let Some(path) = &active_path {
                    format!("Active workspace: {}", path.display())
                } else {
                    "Choose a portable .protrack SQLite file to create or open your workspace."
                        .to_string()
                };

                let start_dir = {
                    let preferred = default_workspaces_dir();
                    if preferred.exists() {
                        preferred
                    } else {
                        directories::UserDirs::new()
                            .and_then(|u| u.document_dir().map(Path::to_path_buf))
                            .unwrap_or_else(|| PathBuf::from("."))
                    }
                };

                let danger = cx.theme().danger;
                let danger_bg = danger.opacity(if cx.theme().is_dark() { 0.16 } else { 0.10 });
                let danger_border = danger.opacity(if cx.theme().is_dark() { 0.42 } else { 0.30 });

                let mut body = v_flex().w_full().gap_3p5().child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(subtitle),
                );

                if let Some(err_msg) = error {
                    body = body.child(
                        h_flex()
                            .w_full()
                            .px_3()
                            .py_2()
                            .gap_2()
                            .items_start()
                            .rounded(cx.theme().radius)
                            .bg(danger_bg)
                            .border_1()
                            .border_color(danger_border)
                            .child(
                                Icon::new(IconName::TriangleAlert)
                                    .small()
                                    .text_color(danger),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(danger)
                                    .child(err_msg),
                            ),
                    );
                }

                // Primary action row
                body = body.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            div().flex_1().child(
                                Button::new("workspace-modal-create")
                                    .primary()
                                    .w_full()
                                    .icon(IconName::Plus)
                                    .label("Create New Workspace…")
                                    .disabled(busy)
                                    .on_click({
                                        let app_state = app_state_inner.clone();
                                        let modal_state = modal_state_inner.clone();
                                        let start_dir = start_dir.clone();
                                        move |_, window, cx| {
                                            let prompt = cx.prompt_for_new_path(
                                                &start_dir,
                                                Some("workspace.protrack"),
                                            );
                                            let app_state = app_state.clone();
                                            let modal_state = modal_state.clone();
                                            window
                                                .spawn(cx, async move |cx| {
                                                    let Ok(Ok(Some(chosen_path))) = prompt.await
                                                    else {
                                                        return;
                                                    };
                                                    let normalized =
                                                        normalize_protrack_path(&chosen_path);
                                                    let seed = modal_state.read_with(cx, |s, _| {
                                                        s.seed_sample_project
                                                    });
                                                    mark_modal_busy(cx, &modal_state);

                                                    let task = app_state.update(cx, |app, cx| {
                                                        app.create_workspace(
                                                            normalized.clone(),
                                                            seed,
                                                            true,
                                                            cx,
                                                        )
                                                    });
                                                    let label = normalized
                                                        .file_name()
                                                        .and_then(|n| n.to_str())
                                                        .unwrap_or("workspace")
                                                        .to_string();
                                                    finish_modal_workspace_task(
                                                        cx,
                                                        &app_state,
                                                        &modal_state,
                                                        task,
                                                        Some(format!("Opened workspace {label}")),
                                                    )
                                                    .await;
                                                })
                                                .detach();
                                        }
                                    }),
                            ),
                        )
                        .child(
                            div().flex_1().child(
                                Button::new("workspace-modal-open")
                                    .outline()
                                    .w_full()
                                    .icon(IconName::FolderOpen)
                                    .label("Open Existing .protrack…")
                                    .disabled(busy)
                                    .on_click({
                                        let app_state = app_state_inner.clone();
                                        let modal_state = modal_state_inner.clone();
                                        move |_, window, cx| {
                                            let prompt = cx.prompt_for_paths(PathPromptOptions {
                                                files: true,
                                                directories: false,
                                                multiple: false,
                                                prompt: Some("Open .protrack Workspace".into()),
                                            });
                                            let app_state = app_state.clone();
                                            let modal_state = modal_state.clone();
                                            window
                                                .spawn(cx, async move |cx| {
                                                    let Ok(Ok(Some(paths))) = prompt.await else {
                                                        return;
                                                    };
                                                    let Some(chosen_path) =
                                                        paths.into_iter().next()
                                                    else {
                                                        return;
                                                    };
                                                    mark_modal_busy(cx, &modal_state);

                                                    let task = app_state.update(cx, |app, cx| {
                                                        app.open_workspace(
                                                            chosen_path.clone(),
                                                            true,
                                                            cx,
                                                        )
                                                    });
                                                    let label = chosen_path
                                                        .file_name()
                                                        .and_then(|n| n.to_str())
                                                        .unwrap_or("workspace")
                                                        .to_string();
                                                    finish_modal_workspace_task(
                                                        cx,
                                                        &app_state,
                                                        &modal_state,
                                                        task,
                                                        Some(format!("Switched to {label}")),
                                                    )
                                                    .await;
                                                })
                                                .detach();
                                        }
                                    }),
                            ),
                        ),
                );

                // Seed Sample Project toggle
                body = body.child(
                    v_flex()
                        .w_full()
                        .p_3()
                        .gap_1()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().muted.opacity(0.4))
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(
                            Switch::new("workspace-modal-seed-switch")
                                .small()
                                .checked(seed_sample)
                                .disabled(busy)
                                .label("Seed Sample Project")
                                .on_change({
                                    let modal_state = modal_state_inner.clone();
                                    move |checked, window, cx| {
                                        modal_state.update(cx, |s, cx| {
                                            s.seed_sample_project = *checked;
                                            cx.notify();
                                        });
                                        window.refresh();
                                    }
                                }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("When creating a new workspace, pre-populate a starter ProTrack v1 project with sample Features, shared Work Items, Assignees, and Tags."),
                        ),
                );

                // Recent workspaces list
                let recents_list = if recents.is_empty() {
                    div()
                        .w_full()
                        .px_3()
                        .py_4()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("No recent workspaces yet. Create a new .protrack file to begin.")
                        .into_any_element()
                } else {
                    v_flex()
                        .w_full()
                        .max_h(px(220.))
                        .overflow_hidden()
                        .child(
                            div().flex_1().overflow_hidden().child(
                                v_flex()
                                    .size_full()
                                    .gap_1()
                                    .overflow_y_scrollbar()
                                    .id("workspace-modal-recents-scroll")
                                    .children(recents.into_iter().enumerate().map(|(ix, entry)| {
                                        let exists = entry.path.exists();
                                        let is_active =
                                            active_path.as_ref().is_some_and(|p| p == &entry.path);
                                        let open_path = entry.path.clone();
                                        let remove_path = entry.path.clone();
                                        let display_path = entry.path.display().to_string();
                                        let can_open = exists && !busy;

                                        h_flex()
                                            .id(("workspace-recent-row", ix))
                                            .flex_shrink_0()
                                            .w_full()
                                .px_2p5()
                                .py_2()
                                .gap_2()
                                .items_center()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(if is_active {
                                    cx.theme().primary.opacity(0.45)
                                } else {
                                    cx.theme().border
                                })
                                .bg(if is_active {
                                    cx.theme().accent
                                } else {
                                    cx.theme().popover
                                })
                                .when(can_open && !is_active, |this| {
                                    this.cursor_pointer().hover(|s| s.bg(cx.theme().list_hover))
                                })
                                .when(can_open, |this| {
                                    let app_state = app_state_inner.clone();
                                    let modal_state = modal_state_inner.clone();
                                    this.on_click(move |_, window, cx| {
                                        let open_path = open_path.clone();
                                        let app_state = app_state.clone();
                                        let modal_state = modal_state.clone();
                                        window
                                            .spawn(cx, async move |cx| {
                                                mark_modal_busy(cx, &modal_state);
                                                let task = app_state.update(cx, |app, cx| {
                                                    app.open_workspace(
                                                        open_path.clone(),
                                                        true,
                                                        cx,
                                                    )
                                                });
                                                finish_modal_workspace_task(
                                                    cx,
                                                    &app_state,
                                                    &modal_state,
                                                    task,
                                                    None,
                                                )
                                                .await;
                                            })
                                            .detach();
                                    })
                                })
                                .child(
                                    Icon::new(IconName::HardDrive)
                                        .small()
                                        .text_color(if is_active {
                                            cx.theme().primary
                                        } else {
                                            cx.theme().muted_foreground
                                        }),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .overflow_hidden()
                                                .whitespace_nowrap()
                                                .text_ellipsis()
                                                .text_xs()
                                                .font_semibold()
                                                .child(entry.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .overflow_hidden()
                                                .whitespace_nowrap()
                                                .text_ellipsis()
                                                .text_size(px(11.))
                                                .font_family(cx.theme().mono_font_family.clone())
                                                .text_color(cx.theme().muted_foreground)
                                                .child(display_path),
                                        ),
                                )
                                .when(is_active, |this| {
                                    this.child(Tag::info().xsmall().outline().child("Active"))
                                })
                                .when(!exists, |this| {
                                    this.child(Tag::warning().xsmall().outline().child("Missing"))
                                })
                                .when(!is_active, |this| {
                                    let app_state = app_state_inner.clone();
                                    this.child(
                                        Button::new(("workspace-recent-remove", ix))
                                            .ghost()
                                            .xsmall()
                                            .compact()
                                            .accessibility_label("Remove from recent workspaces")
                                            .icon(IconName::Close)
                                            .tooltip("Remove from recent workspaces")
                                            .on_click(move |_, window, cx| {
                                                cx.stop_propagation();
                                                app_state.update(cx, |app, cx| {
                                                    app.remove_recent_workspace(&remove_path, cx);
                                                });
                                                window.refresh();
                                            }),
                                    )
                                })
                        })),
                        ),
                        )
                        .into_any_element()
                };

                body = body.child(
                    v_flex()
                        .w_full()
                        .gap_1p5()
                        .child(
                            div()
                                .text_size(px(10.))
                                .font_semibold()
                                .text_color(cx.theme().muted_foreground)
                                .child("RECENT WORKSPACES"),
                        )
                        .child(recents_list),
                );

                content.child(body)
            })
    });
}
