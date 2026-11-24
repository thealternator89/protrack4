use std::fs;
use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme as _, WindowExt as _};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, Bounds, Entity, ScrollDelta, TestAppContext, WindowBounds, WindowOptions,
    base::Root, point, px, size,
};
use protrack4::commands::{
    CreateFeature, CreateProject, CreateWorkItem, ReparentFeature, ReorderSiblingFeatures,
    UpdateItemFields, UpdateProject, UpsertAssignee, UpsertTag,
};
use protrack4::domain::{
    BlockerWarning, EffectiveBlockState, ItemKind, ItemStatus, Priority, ProjectStatus,
};
use protrack4::state::{AppState, CenterViewTab, LaunchConfig, ThemePreference};
use protrack4::ui::WorkspaceShell;
use protrack4::ui::inspector::DescriptionMode;
use protrack4::workspace::{WorkspaceManager, lock_path_for, shm_path_for, wal_path_for};
use tempfile::TempDir;

async fn wait_for_workspace_idle(cx: &mut TestAppContext, app_state: &Entity<AppState>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        let loading = app_state.read_with(cx, |app, _| app.is_loading());
        if !loading {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for AppState workspace load"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

async fn wait_until<F>(cx: &mut TestAppContext, description: &str, mut predicate: F)
where
    F: FnMut(&mut gpui_kit::App) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        if cx.update(|cx| predicate(cx)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {description}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[gpui_kit::test]
async fn shell_boots_and_navigates_views_selection_and_themes(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("seed.protrack");
    let prefs_path = dir.path().join("preferences.json");

    {
        let mut manager = WorkspaceManager::new();
        manager
            .create_workspace(&db_path, Some("Sample"), true)
            .unwrap();
        manager.close_workspace().unwrap();
    }

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        protrack4::ui::register_keybindings(cx);
    });

    let config = LaunchConfig {
        db_override: Some(db_path.clone()),
        preferences_path: Some(prefs_path),
    };

    let (handle, shell) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config, window, cx)),
        )
        .expect("open WorkspaceShell test window");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let (app_state, store) = shell.read_with(cx, |view, _| {
        (view.app_state().clone(), view.store().clone())
    });

    wait_for_workspace_idle(cx, &app_state).await;

    // Initial render & SQLite-backed seed verification
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);

        let state = store.read(cx);
        assert!(state.has_workspace());
        assert!(state.sidebar_open());
        assert!(state.inspector_open());
        assert_eq!(state.active_tab(), CenterViewTab::Outline);
        let snap = state.snapshot().expect("seeded project snapshot loaded");
        assert_eq!(snap.unallocated_work_items().len(), 2);
        assert_eq!(
            state.selected_item().map(|item| item.item_key.as_str()),
            Some("PRO-1")
        );
    })
    .unwrap();

    // Toggle sidebar, inspector, and workspace switcher modal via Cmd+B / Cmd+I / Cmd+O
    cx.update_window(handle.into(), |_, window, cx| {
        window.press("secondary-b", cx);
        assert!(!store.read(cx).sidebar_open());
        window.press("secondary-b", cx);
        assert!(store.read(cx).sidebar_open());

        window.press("secondary-i", cx);
        assert!(!store.read(cx).inspector_open());
        window.press("secondary-i", cx);
        assert!(store.read(cx).inspector_open());

        window.press("secondary-o", cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        window.close_dialog(cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
    })
    .unwrap();

    // Switch through all tabs, select shared & unallocated items, collapse feature, and render each frame
    cx.update_window(handle.into(), |_, window, cx| {
        let (pro_2_id, pro_5_id, pro_8_id) = {
            let state = store.read(cx);
            let snap = state.snapshot().unwrap();
            let find_id = |key: &str| {
                snap.active_items()
                    .into_iter()
                    .find(|item| item.item_key == key)
                    .map(|item| item.id)
                    .unwrap()
            };
            (find_id("PRO-2"), find_id("PRO-5"), find_id("PRO-8"))
        };

        // Select shared Work Item (PRO-5) in Outline and verify Inspector markdown renders cleanly
        store.update(cx, |s, cx| {
            s.select_item(Some(pro_5_id));
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(
            store.read(cx).selected_item().map(|i| (i.kind, i.item_key.as_str())),
            Some((ItemKind::WorkItem, "PRO-5"))
        );

        // Collapse PRO-2 in Outline
        store.update(cx, |s, cx| {
            s.toggle_feature_expanded(pro_2_id);
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(
            store
                .read(cx)
                .snapshot()
                .unwrap()
                .visible_feature_tree_rows(store.read(cx).expanded_features())
                .len(),
            2
        );

        // Switch to Table, Unallocated (select PRO-8), and Trash
        for tab in [
            CenterViewTab::Table,
            CenterViewTab::Unallocated,
            CenterViewTab::Trash,
            CenterViewTab::Outline,
        ] {
            store.update(cx, |s, cx| {
                s.set_active_tab(tab);
                if tab == CenterViewTab::Unallocated {
                    s.select_item(Some(pro_8_id));
                }
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(store.read(cx).active_tab(), tab);
        }
    })
    .unwrap();

    // Switch Light / Dark / System themes and render each frame cleanly
    cx.update_window(handle.into(), |_, window, cx| {
        for pref in [
            ThemePreference::Light,
            ThemePreference::Dark,
            ThemePreference::System,
        ] {
            app_state.update(cx, |app, cx| {
                app.set_theme_preference(pref, cx);
            });
            pref.apply(Some(window), cx);
            window.render_frame(cx);

            match pref {
                ThemePreference::Light => assert!(!cx.theme().is_dark()),
                ThemePreference::Dark => assert!(cx.theme().is_dark()),
                ThemePreference::System => {}
            }
        }
    })
    .unwrap();

    app_state.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());
}

#[gpui_kit::test]
async fn welcome_modal_and_preferences_relaunch_persistence(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let dir = TempDir::new().unwrap();
    let prefs_path = dir.path().join("preferences.json");
    let empty_ws_path = dir.path().join("empty.protrack");
    let ws_path = dir.path().join("relaunch.protrack");

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        protrack4::ui::register_keybindings(cx);
    });

    // Session 1: no prior workspace -> Welcome modal opens automatically
    let config1 = LaunchConfig {
        db_override: None,
        preferences_path: Some(prefs_path.clone()),
    };
    let (handle1, shell1) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config1, window, cx)),
        )
        .expect("open Session 1 WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let app_state1 = shell1.read_with(cx, |view, _| view.app_state().clone());
    let store1 = shell1.read_with(cx, |view, _| view.store().clone());

    cx.update_window(handle1.into(), |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(!app_state1.read(cx).has_open_workspace());
        assert!(window.has_active_dialog(cx));
    })
    .unwrap();

    // Create an empty workspace (0 projects) and verify empty UI + DomainEvent seeding refresh
    let empty_task = app_state1.update(cx, |app, cx| {
        app.create_workspace(empty_ws_path.clone(), false, true, cx)
    });
    wait_for_workspace_idle(cx, &app_state1).await;
    empty_task.await.unwrap();

    cx.update_window(handle1.into(), |_, window, cx| {
        window.close_dialog(cx);
        window.render_frame(cx);
        assert!(app_state1.read(cx).has_open_workspace());
        assert!(store1.read(cx).snapshot().is_none());
    })
    .unwrap();

    let seed_task = app_state1.update(cx, |app, cx| app.seed_sample_project_in_active(cx));
    seed_task.await.unwrap();
    cx.run_until_parked();

    cx.update_window(handle1.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(store1.read(cx).snapshot().unwrap().items.len(), 9);
    })
    .unwrap();

    // Switch to a fresh seeded workspace so workspace switching cleans up empty_ws_path WAL/SHM
    let create_task = app_state1.update(cx, |app, cx| {
        app.create_workspace(ws_path.clone(), true, true, cx)
    });
    wait_for_workspace_idle(cx, &app_state1).await;
    create_task.await.unwrap();

    assert!(!wal_path_for(&empty_ws_path).exists());
    assert!(!shm_path_for(&empty_ws_path).exists());
    assert!(!lock_path_for(&empty_ws_path).exists());

    cx.update_window(handle1.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(app_state1.read(cx).has_open_workspace());
        assert_eq!(store1.read(cx).snapshot().unwrap().items.len(), 9);

        // Customize theme, sidebar state, and panel widths
        app_state1.update(cx, |app, cx| {
            app.set_theme_preference(ThemePreference::Dark, cx);
            app.set_sidebar_open(false, cx);
            app.set_panel_widths(290.0, 410.0);
        });
        window.render_frame(cx);
    })
    .unwrap();

    app_state1.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(prefs_path.exists());
    assert!(!wal_path_for(&ws_path).exists());
    assert!(!shm_path_for(&ws_path).exists());
    assert!(!lock_path_for(&ws_path).exists());

    // Session 2: relaunch with the same preferences.json -> auto-reopens last workspace & restores layout
    let config2 = LaunchConfig {
        db_override: None,
        preferences_path: Some(prefs_path),
    };
    let (handle2, shell2) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config2, window, cx)),
        )
        .expect("open Session 2 WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let app_state2 = shell2.read_with(cx, |view, _| view.app_state().clone());
    let store2 = shell2.read_with(cx, |view, _| view.store().clone());
    wait_for_workspace_idle(cx, &app_state2).await;

    let canonical_ws = fs::canonicalize(&ws_path).unwrap();
    cx.update_window(handle2.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
        assert!(app_state2.read(cx).has_open_workspace());
        assert_eq!(
            app_state2.read(cx).active_workspace_path(),
            Some(canonical_ws.as_path())
        );

        let st = store2.read(cx);
        assert_eq!(st.theme_preference(), ThemePreference::Dark);
        assert!(cx.theme().is_dark());
        assert!(!st.sidebar_open());
        assert!(st.inspector_open());
        assert_eq!(st.snapshot().unwrap().items.len(), 9);

        let shell_ref = shell2.read(cx);
        assert_eq!(shell_ref.sidebar_width(), px(290.));
        assert_eq!(shell_ref.inspector_width(), px(410.));
    })
    .unwrap();

    app_state2.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&ws_path).exists());
    assert!(!shm_path_for(&ws_path).exists());
    assert!(!lock_path_for(&ws_path).exists());
}

#[gpui_kit::test]
async fn phase3_project_feature_crud_and_inspector_persistence(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("phase3.protrack");
    let prefs_path = dir.path().join("preferences.json");

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        protrack4::ui::register_keybindings(cx);
    });

    let config = LaunchConfig {
        db_override: Some(db_path.clone()),
        preferences_path: Some(prefs_path.clone()),
    };

    let (handle, shell) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config.clone(), window, cx)),
        )
        .expect("open Phase 3 WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let (app_state, store, inspector) = shell.read_with(cx, |view, _| {
        (
            view.app_state().clone(),
            view.store().clone(),
            view.inspector().clone(),
        )
    });

    wait_for_workspace_idle(cx, &app_state).await;

    // 1. Open New Project modal from empty center workspace button & create project via UI
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("center-new-project-btn", cx);
        // Two frames: first mounts the dialog layer, second paints it at rest.
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert_eq!(window.find("project-modal-name").focused(), Some(true));

        window.input("Alpha Platform", cx);
    })
    .unwrap();

    // Leave the outer `update_window` so GPUI's `flush_effects` delivers `InputEvent::Change`
    // to `ProjectModalView`'s subscription before asserting the auto-suggested prefix & submitting.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("project-modal-name").value(),
            Some("Alpha Platform")
        );
        assert_eq!(window.find("project-modal-prefix").value(), Some("AP"));

        window.click("project-modal-submit", cx);
    })
    .unwrap();

    wait_until(cx, "Alpha Platform project created", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.project.name == "Alpha Platform" && s.project.key_prefix == "AP")
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
    })
    .unwrap();

    let project_id = cx.update(|cx| store.read(cx).snapshot().unwrap().project.id);

    // 2. Also create a second project via command and verify project switching works cleanly
    let create_proj_b_task = app_state.update(cx, |app, cx| {
        app.execute_command(
            Box::new(CreateProject::new(
                "Beta Mobile",
                "BM",
                "Mobile app",
                "#10B981",
                ProjectStatus::Active,
            )),
            cx,
        )
    });
    let beta_effect = create_proj_b_task.await.unwrap();
    let beta_id = beta_effect.created_project_id.unwrap();
    wait_until(cx, "switched to newly created Beta Mobile project", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.project.id == beta_id && s.projects.len() == 2)
    })
    .await;

    // Switch back to Alpha Platform
    app_state
        .update(cx, |app, cx| app.switch_project(project_id, cx))
        .await
        .unwrap();
    wait_until(cx, "switched back to Alpha Platform", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.project.id == project_id)
    })
    .await;

    // 3. Create 3 levels of nested Features: F1 -> F2 -> F3 (plus F4 sibling of F2)
    let f1_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    project_id,
                    None,
                    "Core Engine",
                    "",
                    ItemStatus::Todo,
                    Priority::High,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let f2_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    project_id,
                    Some(f1_id),
                    "Storage Layer",
                    "",
                    ItemStatus::Todo,
                    Priority::Medium,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let f3_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    project_id,
                    Some(f2_id),
                    "WAL Flush",
                    "",
                    ItemStatus::Backlog,
                    Priority::Low,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let f4_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    project_id,
                    Some(f1_id),
                    "Query Planner",
                    "",
                    ItemStatus::Todo,
                    Priority::None,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    wait_until(cx, "4 features loaded into snapshot", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.active_items().len() == 4)
    })
    .await;

    // 4. Select F3 in store, edit Title + Markdown Description via InspectorView Edit/Preview
    cx.update_window(handle.into(), |_, window, cx| {
        store.update(cx, |s, cx| {
            s.select_item(Some(f3_id));
            cx.notify();
        });
        window.render_frame(cx);

        inspector.update(cx, |view, cx| {
            view.set_description_mode(DescriptionMode::Edit, window, cx);
            view.title_input().update(cx, |inp, cx| {
                inp.replace_all("WAL Flush & Checkpoint", window, cx);
            });
            view.description_input().update(cx, |inp, cx| {
                inp.replace_all("## WAL Guarantees\n\n- Truncate WAL on close\n- Switch to `DELETE` mode", window, cx);
            });
        });
        window.render_frame(cx);
    })
    .unwrap();

    // Flush `InputEvent::Change` effects to `InspectorView` before toggling back to Preview.
    cx.update_window(handle.into(), |_, window, cx| {
        inspector.update(cx, |view, cx| {
            // Toggling back to Preview flushes dirty edits immediately.
            view.set_description_mode(DescriptionMode::Preview, window, cx);
        });
        window.render_frame(cx);
    })
    .unwrap();

    // Also update Status & Priority
    app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(UpdateItemFields {
                    item_id: f3_id,
                    title: None,
                    description: None,
                    status: Some(ItemStatus::InProgress),
                    priority: Some(Priority::Urgent),
                    assignee_id: None,
                }),
                cx,
            )
        })
        .await
        .unwrap();

    wait_until(cx, "F3 inspector edits persisted in snapshot", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.item(f3_id).is_some_and(|item| {
                item.title == "WAL Flush & Checkpoint"
                    && item.description.contains("Truncate WAL on close")
                    && item.status == ItemStatus::InProgress
                    && item.priority == Priority::Urgent
            })
        })
    })
    .await;

    // 5. Reparent F3 directly under F1, then reorder F3 before F2
    app_state
        .update(cx, |app, cx| {
            app.execute_command(Box::new(ReparentFeature::new(f3_id, Some(f1_id))), cx)
        })
        .await
        .unwrap();

    app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(ReorderSiblingFeatures::new(
                    project_id,
                    f3_id,
                    None,
                    Some(f2_id),
                )),
                cx,
            )
        })
        .await
        .unwrap();

    wait_until(cx, "reparent and reorder reflected in snapshot", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            let children: Vec<_> = s
                .child_features(Some(f1_id))
                .into_iter()
                .map(|f| f.id)
                .collect();
            children == vec![f3_id, f2_id, f4_id]
        })
    })
    .await;

    // 6. Rename Project key_prefix AP -> PRO and verify atomic rewrite AP-1..4 -> PRO-1..4
    app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(UpdateProject {
                    project_id,
                    name: "Alpha Platform v2".to_string(),
                    key_prefix: "PRO".to_string(),
                    description: "Renamed project".to_string(),
                    color: "#8B5CF6".to_string(),
                    status: ProjectStatus::Paused,
                }),
                cx,
            )
        })
        .await
        .unwrap();

    wait_until(cx, "key_prefix rewritten to PRO-N", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.project.key_prefix == "PRO"
                && s.item(f1_id).is_some_and(|i| i.item_key == "PRO-1")
                && s.item(f3_id).is_some_and(|i| i.item_key == "PRO-3")
        })
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();

    app_state.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());

    // 7. Reopen the same .protrack workspace in a brand-new WorkspaceShell -> all Phase 3 edits persist!
    let (handle2, shell2) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config, window, cx)),
        )
        .expect("reopen Phase 3 WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let app_state2 = shell2.read_with(cx, |view, _| view.app_state().clone());
    let store2 = shell2.read_with(cx, |view, _| view.store().clone());
    wait_for_workspace_idle(cx, &app_state2).await;

    cx.update_window(handle2.into(), |_, window, cx| {
        window.render_frame(cx);
        let st = store2.read(cx);
        let snap = st.snapshot().expect("reopened snapshot present");
        assert_eq!(snap.projects.len(), 2);
        assert_eq!(snap.project.name, "Alpha Platform v2");
        assert_eq!(snap.project.key_prefix, "PRO");
        assert_eq!(snap.project.status, ProjectStatus::Paused);

        let f3 = snap.item(f3_id).expect("PRO-3 persisted");
        assert_eq!(f3.item_key, "PRO-3");
        assert_eq!(f3.title, "WAL Flush & Checkpoint");
        assert!(f3.description.contains("Truncate WAL on close"));
        assert_eq!(f3.status, ItemStatus::InProgress);
        assert_eq!(f3.priority, Priority::Urgent);
        assert_eq!(f3.parent_feature_id, Some(f1_id));

        let children: Vec<_> = snap
            .child_features(Some(f1_id))
            .into_iter()
            .map(|f| f.item_key.as_str())
            .collect();
        assert_eq!(children, vec!["PRO-3", "PRO-2", "PRO-4"]);
    })
    .unwrap();

    app_state2.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());
}

#[gpui_kit::test]
async fn phase4_work_items_multi_feature_and_unallocated_triage(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("phase4.protrack");
    let prefs_path = dir.path().join("preferences.json");

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        protrack4::ui::register_keybindings(cx);
    });

    let config = LaunchConfig {
        db_override: Some(db_path.clone()),
        preferences_path: Some(prefs_path),
    };

    let (handle, shell) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config.clone(), window, cx)),
        )
        .expect("open Phase 4 WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let (app_state, store) = shell.read_with(cx, |view, _| {
        (view.app_state().clone(), view.store().clone())
    });

    wait_for_workspace_idle(cx, &app_state).await;

    // 1. Create Project and two Features (Auth, Billing)
    let project_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateProject::new(
                    "Triage Suite",
                    "TRI",
                    "",
                    "#6366F1",
                    ProjectStatus::Active,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_project_id
        .unwrap();

    let f1_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    project_id,
                    None,
                    "Auth",
                    "",
                    ItemStatus::Todo,
                    Priority::High,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let f2_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    project_id,
                    None,
                    "Billing",
                    "",
                    ItemStatus::Todo,
                    Priority::Medium,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    wait_until(cx, "2 features loaded", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.active_items().len() == 2)
    })
    .await;

    let (f1_num, f2_num) = cx.update(|cx| {
        let s = store.read(cx).snapshot().unwrap();
        (
            s.item(f1_id).unwrap().item_number as usize,
            s.item(f2_id).unwrap().item_number as usize,
        )
    });

    // 2. Navigate to Unallocated via Sidebar row and capture 3 Work Items via Enter
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("sidebar-unallocated-row", cx);
        window.render_frame(cx);
        assert_eq!(store.read(cx).active_tab(), CenterViewTab::Unallocated);
        window.click("unallocated-quick-capture", cx);
        window.render_frame(cx);
        assert_eq!(window.find("unallocated-quick-capture").focused(), Some(true));
    })
    .unwrap();

    for (expected_count, title) in [
        (1usize, "Add OAuth token refresh"),
        (2usize, "Fix invoice PDF rounding"),
        (3usize, "Audit session timeout"),
    ] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.input(title, cx);
        })
        .unwrap();

        cx.update_window(handle.into(), |_, window, cx| {
            window.press("enter", cx);
        })
        .unwrap();

        wait_until(cx, "quick-captured work item created", |cx| {
            store
                .read(cx)
                .snapshot()
                .is_some_and(|s| s.unallocated_work_items().len() == expected_count)
        })
        .await;

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("unallocated-quick-capture").value(), Some(""));
            assert_eq!(window.find("unallocated-quick-capture").focused(), Some(true));
        })
        .unwrap();
    }

    let (wi1_id, wi1_num) = cx.update(|cx| {
        let s = store.read(cx).snapshot().unwrap();
        let wi1 = s
            .unallocated_work_items()
            .into_iter()
            .find(|w| w.title == "Add OAuth token refresh")
            .unwrap();
        (wi1.id, wi1.item_number as usize)
    });

    // 3. Assign wi1 to 2 Features via [+ Assign Features] popover in Unallocated View
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("unallocated-assign-btn", wi1_num), cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.try_find("unallocated-feature-search").is_some());

        window.click(("unallocated-picker-feature", f1_num), cx);
        window.render_frame(cx);
        window.click(("unallocated-picker-feature", f2_num), cx);
        window.render_frame(cx);
        window.click("unallocated-picker-apply", cx);
    })
    .unwrap();

    wait_until(cx, "wi1 assigned to 2 Features and removed from Unallocated", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.features_for_work_item(wi1_id).len() == 2 && s.unallocated_work_items().len() == 2
        })
    })
    .await;

    // 4. Inspect wi1 in Inspector: verify 2 attached features, detach f1, re-attach f1 via popover, then click [Unallocate]
    cx.update_window(handle.into(), |_, window, cx| {
        store.update(cx, |s, cx| {
            s.select_item(Some(wi1_id));
            cx.notify();
        });
        window.render_frame(cx);
        assert!(window.try_find(("inspector-detach-feature", f1_num)).is_some());
        assert!(window.try_find(("inspector-detach-feature", f2_num)).is_some());

        window.click(("inspector-detach-feature", f1_num), cx);
    })
    .unwrap();

    wait_until(cx, "wi1 detached from f1 in Inspector", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            let feats = s.features_for_work_item(wi1_id);
            feats.len() == 1 && feats[0].id == f2_id
        })
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("inspector-attach-feature-btn", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.try_find("inspector-attach-feature-search").is_some());
        window.click(("inspector-attach-option", f1_num), cx);
    })
    .unwrap();

    wait_until(cx, "wi1 re-attached to f1 via Inspector popover", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.features_for_work_item(wi1_id).len() == 2)
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("inspector-unallocate-btn", cx);
    })
    .unwrap();

    wait_until(cx, "wi1 unallocated via Inspector [Unallocate]", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.features_for_work_item(wi1_id).is_empty() && s.unallocated_work_items().len() == 3
        })
    })
    .await;

    app_state.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());

    // 5. Reopen workspace and confirm persistence
    let (handle2, shell2) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config, window, cx)),
        )
        .expect("reopen Phase 4 WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let app_state2 = shell2.read_with(cx, |view, _| view.app_state().clone());
    let store2 = shell2.read_with(cx, |view, _| view.store().clone());
    wait_for_workspace_idle(cx, &app_state2).await;

    cx.update_window(handle2.into(), |_, window, cx| {
        window.render_frame(cx);
        let snap = store2.read(cx).snapshot().expect("reopened snapshot");
        assert_eq!(snap.unallocated_work_items().len(), 3);
        assert!(snap.item(wi1_id).is_some());
    })
    .unwrap();

    app_state2.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());
}

#[gpui_kit::test]
async fn phase5_global_assignees_and_project_scoped_tags(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("phase5.protrack");
    let prefs_path = dir.path().join("preferences.json");

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        protrack4::ui::register_keybindings(cx);
    });

    let config = LaunchConfig {
        db_override: Some(db_path.clone()),
        preferences_path: Some(prefs_path),
    };

    let (handle, shell) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config.clone(), window, cx)),
        )
        .expect("open Phase 5 WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let (app_state, store) = shell.read_with(cx, |view, _| {
        (view.app_state().clone(), view.store().clone())
    });

    wait_for_workspace_idle(cx, &app_state).await;

    // 1. Create Project A (ALP) and Project B (BET), plus items in each
    let proj_a_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateProject::new(
                    "Alpha",
                    "ALP",
                    "",
                    "#6366F1",
                    ProjectStatus::Active,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_project_id
        .unwrap();

    let wi_a_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(protrack4::commands::CreateWorkItem::new(
                    proj_a_id,
                    "Wire OAuth callback",
                    "",
                    ItemStatus::Todo,
                    Priority::High,
                    vec![],
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let proj_b_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateProject::new(
                    "Beta",
                    "BET",
                    "",
                    "#10B981",
                    ProjectStatus::Active,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_project_id
        .unwrap();

    let wi_b_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(protrack4::commands::CreateWorkItem::new(
                    proj_b_id,
                    "Mobile push notification",
                    "",
                    ItemStatus::Todo,
                    Priority::Medium,
                    vec![],
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    // Switch back to Project A
    app_state
        .update(cx, |app, cx| app.switch_project(proj_a_id, cx))
        .await
        .unwrap();
    wait_until(cx, "Project A active", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.project.id == proj_a_id && s.item(wi_a_id).is_some())
    })
    .await;

    // 2. Open Global Assignees Manager via Sidebar and create 2 Assignees (MB, AI)
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("sidebar-assignees", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert!(window.try_find("assignee-modal-name").is_some());

        window.click("assignee-modal-name", cx);
        window.input("Mark Benson", cx);
    })
    .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("assignee-modal-handle").value(), Some("MB"));
        window.click("assignee-modal-submit", cx);
    })
    .unwrap();

    wait_until(cx, "first assignee created", |cx| {
        store.read(cx).assignees().len() == 1
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("assignee-modal-name", cx);
        window.input("Claude Agent", cx);
    })
    .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("assignee-modal-handle", cx);
        window.press("secondary-a", cx);
        window.input("AI", cx);
    })
    .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("assignee-modal-submit", cx);
    })
    .unwrap();

    wait_until(cx, "second assignee created", |cx| {
        store.read(cx).assignees().len() == 2
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("assignee-modal-close", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
    })
    .unwrap();

    // 3. Open Project Tags Manager via Sidebar and create 3 Tags in Project A (bug, core, ui)
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("sidebar-tags", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert!(window.try_find("tag-modal-name").is_some());
    })
    .unwrap();

    for (expected_count, tag_name) in [(1usize, "bug"), (2usize, "core"), (3usize, "ui")] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("tag-modal-name", cx);
            window.input(tag_name, cx);
        })
        .unwrap();

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("tag-modal-submit", cx);
        })
        .unwrap();

        wait_until(cx, "project tag created", |cx| {
            store
                .read(cx)
                .snapshot()
                .is_some_and(|s| s.tags.len() == expected_count)
        })
        .await;
    }

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("tag-modal-close", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
    })
    .unwrap();

    let (mb_id, ai_id) = cx.update(|cx| {
        let assignees = store.read(cx).assignees();
        let mb = assignees.iter().find(|a| a.handle == "MB").unwrap().id;
        let ai = assignees.iter().find(|a| a.handle == "AI").unwrap().id;
        (mb, ai)
    });

    // 4. Select wi_a_id, assign MB, and attach/detach tags via Inspector popover & chip [x]
    app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(UpdateItemFields {
                    item_id: wi_a_id,
                    title: None,
                    description: None,
                    status: None,
                    priority: None,
                    assignee_id: Some(Some(mb_id)),
                }),
                cx,
            )
        })
        .await
        .unwrap();

    wait_until(cx, "wi_a assigned to MB", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.item(wi_a_id).and_then(|i| i.assignee_id) == Some(mb_id))
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        store.update(cx, |s, cx| {
            s.select_item(Some(wi_a_id));
            cx.notify();
        });
        window.render_frame(cx);
        window.click("inspector-add-tag-btn", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.try_find("inspector-tag-search").is_some());

        // Tags are sorted alphabetically: 0="bug", 1="core", 2="ui"
        window.click(("inspector-tag-option", 0usize), cx);
    })
    .unwrap();

    wait_until(cx, "first tag attached via Inspector popover", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.tags_for_item(wi_a_id).len() == 1)
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("inspector-tag-option", 2usize), cx);
    })
    .unwrap();

    wait_until(cx, "second tag attached via Inspector popover", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.tags_for_item(wi_a_id).len() == 2)
    })
    .await;

    // Close popover, remove first tag chip via [x], and render Outline/Table/Unallocated views
    cx.update_window(handle.into(), |_, window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
        window.click(("inspector-remove-tag", 0usize), cx);
    })
    .unwrap();

    wait_until(cx, "tag removed via Inspector chip [x]", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            let names: Vec<_> = s.tags_for_item(wi_a_id).into_iter().map(|t| t.name.as_str()).collect();
            names == vec!["ui"]
        })
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        for tab in [
            CenterViewTab::Outline,
            CenterViewTab::Table,
            CenterViewTab::Unallocated,
        ] {
            store.update(cx, |s, cx| {
                s.set_active_tab(tab);
                cx.notify();
            });
            window.render_frame(cx);
        }
    })
    .unwrap();

    // 5. Archive Mark Benson -> preserved on wi_a_id, excluded from selectable_assignees(None)
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("sidebar-assignees", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        // Sorted alphabetically: 0="Claude Agent", 1="Mark Benson"
        window.click(("assignee-row-archive", 1usize), cx);
    })
    .unwrap();

    wait_until(cx, "Mark Benson archived", |cx| {
        store
            .read(cx)
            .assignees()
            .iter()
            .any(|a| a.id == mb_id && a.is_archived())
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("assignee-modal-close", cx);
        window.render_frame(cx);

        let snap = store.read(cx).snapshot().unwrap();
        assert_eq!(snap.item(wi_a_id).unwrap().assignee_id, Some(mb_id));
        assert_eq!(
            snap.selectable_assignees(None)
                .into_iter()
                .map(|a| a.id)
                .collect::<Vec<_>>(),
            vec![ai_id]
        );
        assert_eq!(
            snap.selectable_assignees(Some(mb_id))
                .into_iter()
                .map(|a| a.id)
                .collect::<Vec<_>>(),
            vec![ai_id, mb_id]
        );
    })
    .unwrap();

    // 6. Switch to Project B -> global Assignees persist, Project A Tags are strictly isolated
    app_state
        .update(cx, |app, cx| app.switch_project(proj_b_id, cx))
        .await
        .unwrap();
    wait_until(cx, "Project B active", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.project.id == proj_b_id)
    })
    .await;

    app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(UpdateItemFields {
                    item_id: wi_b_id,
                    title: None,
                    description: None,
                    status: None,
                    priority: None,
                    assignee_id: Some(Some(ai_id)),
                }),
                cx,
            )
        })
        .await
        .unwrap();

    wait_until(cx, "wi_b assigned to AI in Project B", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.item(wi_b_id).and_then(|i| i.assignee_id) == Some(ai_id))
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let snap_b = store.read(cx).snapshot().unwrap();
        assert_eq!(snap_b.assignees.len(), 2);
        assert!(snap_b.tags.is_empty());
        assert_eq!(snap_b.item(wi_b_id).unwrap().assignee_id, Some(ai_id));
    })
    .unwrap();

    app_state.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());

    // 7. Reopen workspace and confirm persistence of Assignees & Tags
    let (handle2, shell2) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config, window, cx)),
        )
        .expect("reopen Phase 5 WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let app_state2 = shell2.read_with(cx, |view, _| view.app_state().clone());
    let store2 = shell2.read_with(cx, |view, _| view.store().clone());
    wait_for_workspace_idle(cx, &app_state2).await;

    app_state2
        .update(cx, |app, cx| app.switch_project(proj_a_id, cx))
        .await
        .unwrap();
    wait_until(cx, "Project A reloaded", |cx| {
        store2
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.project.id == proj_a_id)
    })
    .await;

    cx.update_window(handle2.into(), |_, window, cx| {
        window.render_frame(cx);
        let snap = store2.read(cx).snapshot().unwrap();
        assert_eq!(snap.assignees.len(), 2);
        assert_eq!(snap.tags.len(), 3);
        assert_eq!(snap.item(wi_a_id).unwrap().assignee_id, Some(mb_id));
        assert_eq!(
            snap.tags_for_item(wi_a_id)
                .into_iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["ui"]
        );
    })
    .unwrap();

    app_state2.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());
}

#[gpui_kit::test]
async fn phase5_manager_lists_scroll_when_exceeding_max_height(cx: &mut TestAppContext) {
    use protrack4::commands::{UpsertAssignee, UpsertTag};

    cx.executor().allow_parking();

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("phase5_scroll.protrack");
    let prefs_path = dir.path().join("preferences.json");

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        protrack4::ui::register_keybindings(cx);
    });

    let config = LaunchConfig {
        db_override: Some(db_path.clone()),
        preferences_path: Some(prefs_path),
    };

    let (handle, shell) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config, window, cx)),
        )
        .expect("open Phase 5 scroll WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let (app_state, store) = shell.read_with(cx, |view, _| {
        (view.app_state().clone(), view.store().clone())
    });

    wait_for_workspace_idle(cx, &app_state).await;

    let project_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateProject::new(
                    "Scroll Suite",
                    "SCR",
                    "",
                    "#6366F1",
                    ProjectStatus::Active,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_project_id
        .unwrap();

    for i in 1..=8 {
        app_state
            .update(cx, |app, cx| {
                app.execute_command(
                    Box::new(UpsertAssignee::create(
                        format!("Member {i:02}"),
                        format!("M{i}"),
                        "#64748B",
                        Some("Engineer".to_string()),
                        "Team member notes",
                    )),
                    cx,
                )
            })
            .await
            .unwrap();

        app_state
            .update(cx, |app, cx| {
                app.execute_command(
                    Box::new(UpsertTag::create(
                        project_id,
                        format!("tag-{i:02}"),
                        "#8B5CF6",
                    )),
                    cx,
                )
            })
            .await
            .unwrap();
    }

    wait_until(cx, "8 assignees and 8 tags loaded", |cx| {
        let st = store.read(cx);
        st.assignees().len() == 8 && st.snapshot().is_some_and(|s| s.tags.len() == 8)
    })
    .await;

    // 1. Verify Assignees Manager list clamps to max_h(210) and scrolls to reveal row 7
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("sidebar-assignees", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));

        let viewport = window.find("assignee-list-viewport").bounds();
        assert!(
            viewport.size.height <= px(210.5),
            "assignee list viewport should clamp to 210px, got {:?}",
            viewport.size.height
        );

        let last_row_before = window.find(("assignee-row", 7usize)).bounds();
        assert!(
            last_row_before.bottom() > viewport.bottom(),
            "8th assignee row should start below clamped viewport"
        );

        window.scroll(
            "assignee-list-viewport",
            ScrollDelta::Pixels(point(px(0.), px(-280.))),
            cx,
        );
        window.render_frame(cx);

        let last_row_after = window.find(("assignee-row", 7usize)).bounds();
        assert!(
            last_row_after.origin.y < last_row_before.origin.y,
            "scrolling assignee list must move rows upward (before={:?}, after={:?})",
            last_row_before.origin.y,
            last_row_after.origin.y
        );
        assert!(
            last_row_after.bottom() <= viewport.bottom() + px(1.),
            "8th assignee row should scroll cleanly into viewport (row={:?}, viewport={:?})",
            last_row_after,
            viewport
        );

        window.click("assignee-modal-close", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
    })
    .unwrap();

    // 2. Verify Project Tags Manager list also clamps to max_h(210) and scrolls to reveal row 7
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("sidebar-tags", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));

        let viewport = window.find("tag-list-viewport").bounds();
        assert!(
            viewport.size.height <= px(210.5),
            "tag list viewport should clamp to 210px, got {:?}",
            viewport.size.height
        );

        let last_row_before = window.find(("tag-row", 7usize)).bounds();
        assert!(
            last_row_before.bottom() > viewport.bottom(),
            "8th tag row should start below clamped viewport"
        );

        window.scroll(
            "tag-list-viewport",
            ScrollDelta::Pixels(point(px(0.), px(-280.))),
            cx,
        );
        window.render_frame(cx);

        let last_row_after = window.find(("tag-row", 7usize)).bounds();
        assert!(
            last_row_after.origin.y < last_row_before.origin.y,
            "scrolling tag list must move rows upward (before={:?}, after={:?})",
            last_row_before.origin.y,
            last_row_after.origin.y
        );
        assert!(
            last_row_after.bottom() <= viewport.bottom() + px(1.),
            "8th tag row should scroll cleanly into viewport (row={:?}, viewport={:?})",
            last_row_after,
            viewport
        );

        window.click("tag-modal-close", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
    })
    .unwrap();

    app_state.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());
}

#[gpui_kit::test]
async fn phase6_outline_view_progress_shared_popover_and_inline_add(cx: &mut TestAppContext) {
    use protrack4::ui::outline_view::OutlineScope;

    cx.executor().allow_parking();

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("phase6.protrack");
    let prefs_path = dir.path().join("preferences.json");

    {
        let mut manager = WorkspaceManager::new();
        manager
            .create_workspace(&db_path, Some("Outline Seed"), true)
            .unwrap();
        manager.close_workspace().unwrap();
    }

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        protrack4::ui::register_keybindings(cx);
    });

    let config = LaunchConfig {
        db_override: Some(db_path.clone()),
        preferences_path: Some(prefs_path),
    };

    let (handle, shell) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config, window, cx)),
        )
        .expect("open Phase 6 WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let (app_state, store, outline_view) = shell.read_with(cx, |view, _| {
        (
            view.app_state().clone(),
            view.store().clone(),
            view.outline_view().clone(),
        )
    });

    wait_for_workspace_idle(cx, &app_state).await;

    // 1. Verify SQLite-loaded deduplicated progress metrics on the seeded project
    let (pro_1_id, pro_2_id, pro_3_id, pro_4_id, pro_5_id) = cx.update(|cx| {
        let snap = store.read(cx).snapshot().unwrap();
        let find_id = |key: &str| {
            snap.active_items()
                .into_iter()
                .find(|i| i.item_key == key)
                .map(|i| i.id)
                .unwrap()
        };
        let f1 = find_id("PRO-1");
        let f2 = find_id("PRO-2");
        let f3 = find_id("PRO-3");
        let f4 = find_id("PRO-4");
        let w5 = find_id("PRO-5");

        assert_eq!(snap.feature_progress_detail(f1).detailed_label(), "1/3 (33%)");
        assert_eq!(snap.feature_progress_detail(f2).detailed_label(), "1/3 (33%)");
        assert_eq!(snap.feature_progress_detail(f3).detailed_label(), "1/3 (33%)");
        assert_eq!(snap.feature_progress_detail(f4).detailed_label(), "0/1 (0%)");
        (f1, f2, f3, f4, w5)
    });

    // 2. Render OutlineView and use inline creation to add a Work Item and a Sub-Feature under PRO-4
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("outline-inline-trigger", 4usize), cx);
        window.render_frame(cx);
        assert_eq!(
            outline_view.read(cx).inline_add_feature_id(),
            Some(pro_4_id)
        );
        assert_eq!(window.find("outline-inline-input").focused(), Some(true));
        window.input("Verify WAL checkpoint mode", cx);
    })
    .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.press("enter", cx);
    })
    .unwrap();

    wait_until(cx, "inline work item created under PRO-4", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.work_items_for_feature(pro_4_id).len() == 2)
    })
    .await;

    // Progress updated live: PRO-1 & PRO-2 & PRO-3 -> 1/4 (25%), PRO-4 -> 0/2 (0%), PRO-4 status untouched
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let snap = store.read(cx).snapshot().unwrap();
        assert_eq!(snap.feature_progress_detail(pro_1_id).detailed_label(), "1/4 (25%)");
        assert_eq!(snap.feature_progress_detail(pro_4_id).detailed_label(), "0/2 (0%)");
        assert_eq!(snap.item(pro_4_id).unwrap().status, ItemStatus::Todo);

        // Switch inline kind to Sub-Feature and add a sub-feature under PRO-4
        window.click("outline-inline-kind-feature", cx);
        window.render_frame(cx);
        assert_eq!(outline_view.read(cx).inline_add_kind(), ItemKind::Feature);
        window.input("Trigger Guard Suite", cx);
    })
    .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.press("enter", cx);
    })
    .unwrap();

    wait_until(cx, "inline sub-feature created under PRO-4", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.child_features(Some(pro_4_id)).len() == 1)
    })
    .await;

    // 3. Test [Selected Subtree] scope and [Hide Done/Cancelled] toggle
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("outline-feature", 2usize), cx);
        window.render_frame(cx);
        assert_eq!(store.read(cx).focused_feature_id(), Some(pro_2_id));

        window.click("outline-scope-subtree", cx);
        window.render_frame(cx);
        assert_eq!(outline_view.read(cx).scope(), OutlineScope::SelectedSubtree);
        assert!(window.try_find(("outline-feature", 1usize)).is_none());
        assert!(window.try_find(("outline-feature", 2usize)).is_some());

        // Toggle Hide Done/Cancelled -> PRO-5 (Done) is hidden in Outline
        window.click("outline-toggle-hide-closed", cx);
        window.render_frame(cx);
        assert!(outline_view.read(cx).hide_closed());
        assert!(window.try_find(("outline-wi", 200_005usize)).is_none());

        // Show Done/Cancelled again & return to All Features
        window.click("outline-toggle-hide-closed", cx);
        window.click("outline-scope-all", cx);
        window.render_frame(cx);
        assert_eq!(outline_view.read(cx).scope(), OutlineScope::AllFeatures);
        assert!(!outline_view.read(cx).hide_closed());
        assert!(window.try_find(("outline-wi", 200_005usize)).is_some());
    })
    .unwrap();

    // 4. Click [Shared: +1 feature] popover on PRO-5 under PRO-2 and jump to PRO-3
    cx.update_window(handle.into(), |_, window, cx| {
        window.click(("outline-shared-btn", 200_005usize), cx);
        window.render_frame(cx);
        window.render_frame(cx);
        // Jump item 1 in shared list corresponds to PRO-3 (sorted by item_number: PRO-2=0, PRO-3=1)
        window.click(("outline-shared-jump", 200_005usize * 100 + 1), cx);
        window.render_frame(cx);
        assert_eq!(store.read(cx).selected_item_id(), Some(pro_3_id));
    })
    .unwrap();

    // 5. Right-click an Outline row to verify ContextMenu opens cleanly
    cx.update_window(handle.into(), |_, window, cx| {
        window.right_click(("outline-wi", 300_005usize), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
        window.render_frame(cx);
    })
    .unwrap();

    // 6. Verify inline-created items survive restart
    let _ = pro_5_id;
    app_state.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());
}

#[gpui_kit::test]
async fn phase7_features_only_blocking_engine_and_transitive_propagation(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("phase7.protrack");
    let prefs_path = dir.path().join("preferences.json");

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        protrack4::ui::register_keybindings(cx);
    });

    let config = LaunchConfig {
        db_override: Some(db_path.clone()),
        preferences_path: Some(prefs_path),
    };

    let (handle, shell) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config, window, cx)),
        )
        .expect("open Phase 7 WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let (app_state, store) = shell.read_with(cx, |view, _| {
        (view.app_state().clone(), view.store().clone())
    });

    wait_for_workspace_idle(cx, &app_state).await;

    // 1. Create Project and Features: A -> A1, B (blocker), C (unblocked), plus exclusive & shared Work Items
    let pid = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateProject::new(
                    "Blocking Suite",
                    "BLK",
                    "",
                    "#6366F1",
                    ProjectStatus::Active,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_project_id
        .unwrap();

    let fa_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    None,
                    "Feature Alpha",
                    "",
                    ItemStatus::Todo,
                    Priority::High,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let fa1_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    Some(fa_id),
                    "Child Alpha One",
                    "",
                    ItemStatus::Todo,
                    Priority::Medium,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let fb_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    None,
                    "Blocker Beta",
                    "",
                    ItemStatus::InProgress,
                    Priority::Urgent,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let fc_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    None,
                    "Parallel Gamma",
                    "",
                    ItemStatus::Todo,
                    Priority::Low,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let wi_excl_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateWorkItem::new(
                    pid,
                    "Exclusive task",
                    "",
                    ItemStatus::Todo,
                    Priority::Medium,
                    vec![fa1_id],
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let wi_shared_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateWorkItem::new(
                    pid,
                    "Shared task",
                    "",
                    ItemStatus::Todo,
                    Priority::Low,
                    vec![fa1_id, fc_id],
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    wait_until(cx, "6 items loaded", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.active_items().len() == 6)
    })
    .await;

    let (fa_num, fa1_num, fb_num, fc_num, wi_excl_num, wi_shared_num) = cx.update(|cx| {
        let s = store.read(cx).snapshot().unwrap();
        let num = |id| s.item(id).unwrap().item_number as usize;
        (
            num(fa_id),
            num(fa1_id),
            num(fb_id),
            num(fc_id),
            num(wi_excl_id),
            num(wi_shared_id),
        )
    });

    // 2. Select Feature A, open Inspector's [+ Add Blocker Feature…] popover:
    //    - A and A1 must be pre-filtered out when search is empty
    //    - Searching "Child Alpha" surfaces A1 as disabled with reason
    //    - Clearing search and clicking B adds the direct blocker
    cx.update_window(handle.into(), |_, window, cx| {
        store.update(cx, |s, cx| {
            s.expand_all_features();
            s.select_item(Some(fa_id));
            cx.notify();
        });
        window.render_frame(cx);
        window.click("inspector-add-blocker-btn", cx);
        window.render_frame(cx);
        window.render_frame(cx);

        assert!(window.try_find("inspector-blocker-search").is_some());
        assert!(window.try_find(("inspector-blocker-option", fa_num)).is_none());
        assert!(window.try_find(("inspector-blocker-option", fa1_num)).is_none());
        assert!(window.try_find(("inspector-blocker-option", fb_num)).is_some());
        assert!(window.try_find(("inspector-blocker-option", fc_num)).is_some());

        window.input("Child Alpha", cx);
        window.render_frame(cx);
        assert!(window.try_find(("inspector-blocker-disabled", fa1_num)).is_some());

        window.click("inspector-add-blocker-btn", cx);
        window.render_frame(cx);
        window.click("inspector-add-blocker-btn", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        window.click(("inspector-blocker-option", fb_num), cx);
    })
    .unwrap();

    wait_until(cx, "blocker B added to A", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.effective_block_state(fa_id) == EffectiveBlockState::DirectlyBlocked { active_count: 1 }
        })
    })
    .await;

    // 3. Verify transitive propagation & Outline/Sidebar badges
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let snap = store.read(cx).snapshot().unwrap();
        assert_eq!(
            snap.effective_block_state(fa1_id),
            EffectiveBlockState::InheritedBlocked {
                ancestor_blocker_count: 1
            }
        );
        assert_eq!(
            snap.effective_block_state(wi_excl_id),
            EffectiveBlockState::InheritedBlocked {
                ancestor_blocker_count: 1
            }
        );
        assert_eq!(
            snap.effective_block_state(wi_shared_id),
            EffectiveBlockState::PartiallyBlocked {
                blocked_features: 1,
                total_features: 2,
            }
        );

        assert!(window.try_find(("sidebar-block-dot", fa_num)).is_some());
        assert!(window.try_find(("sidebar-block-dot", fa1_num)).is_some());
        assert!(window.try_find(("outline-feature-block", fa_num)).is_some());
        assert!(window.try_find(("outline-feature-block", fa1_num)).is_some());
        assert!(
            window
                .try_find(("outline-wi-block", fa1_num * 100_000 + wi_excl_num))
                .is_some()
        );
        assert!(
            window
                .try_find(("outline-wi-block", fa1_num * 100_000 + wi_shared_num))
                .is_some()
        );
        assert!(window.try_find("inspector-direct-blocker").is_none());
        assert!(window.try_find(("inspector-direct-blocker", 0usize)).is_some());

        // Select Child A1 and verify read-only Inherited Blockers list + jump button
        store.update(cx, |s, cx| {
            s.select_item(Some(fa1_id));
            cx.notify();
        });
        window.render_frame(cx);
        assert!(window.try_find(("inspector-inherited-blocker", 0usize)).is_some());
        window.click(("inspector-inherited-via", 0usize), cx);
        window.render_frame(cx);
        assert_eq!(store.read(cx).selected_item_id(), Some(fa_id));
    })
    .unwrap();

    // 4. Soft-blocking: move exclusive WI to InProgress -> succeeds with warning_toast
    let start_eff = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(UpdateItemFields {
                    item_id: wi_excl_id,
                    title: None,
                    description: None,
                    status: Some(ItemStatus::InProgress),
                    priority: None,
                    assignee_id: None,
                }),
                cx,
            )
        })
        .await
        .unwrap();
    assert_eq!(
        start_eff.warning_toast.as_deref(),
        Some("Warning: BLK-5 is currently blocked by BLK-3")
    );

    wait_until(cx, "exclusive WI moved to InProgress", |cx| {
        store
            .read(cx)
            .snapshot()
            .and_then(|s| s.item(wi_excl_id))
            .is_some_and(|i| i.status == ItemStatus::InProgress)
    })
    .await;

    // 5. Mark blocker B Cancelled -> A becomes Unblocked with Cancelled warning
    app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(UpdateItemFields {
                    item_id: fb_id,
                    title: None,
                    description: None,
                    status: Some(ItemStatus::Cancelled),
                    priority: None,
                    assignee_id: None,
                }),
                cx,
            )
        })
        .await
        .unwrap();

    wait_until(cx, "cancelled blocker updates A to Unblocked with warning", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.effective_block_state(fa_id) == EffectiveBlockState::Unblocked
                && matches!(
                    s.blocker_warnings(fa_id),
                    [BlockerWarning::Cancelled { .. }]
                )
        })
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("inspector-blocker-warning", 0usize)).is_some());
    })
    .unwrap();

    // 6. Mark blocker B Done -> warnings and blocks clear live
    app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(UpdateItemFields {
                    item_id: fb_id,
                    title: None,
                    description: None,
                    status: Some(ItemStatus::Done),
                    priority: None,
                    assignee_id: None,
                }),
                cx,
            )
        })
        .await
        .unwrap();

    wait_until(cx, "done blocker clears blocks and warnings", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.effective_block_state(fa_id) == EffectiveBlockState::Unblocked
                && s.effective_block_state(wi_excl_id) == EffectiveBlockState::Unblocked
                && s.effective_block_state(wi_shared_id) == EffectiveBlockState::Unblocked
                && s.blocker_warnings(fa_id).is_empty()
        })
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("outline-feature-block", fa_num)).is_none());
        assert!(window.try_find(("sidebar-block-dot", fa_num)).is_none());
        assert!(window.try_find(("inspector-blocker-warning", 0usize)).is_none());
    })
    .unwrap();

    app_state.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());
}

#[gpui_kit::test]
async fn phase8_strictly_guarded_feature_work_item_conversion(cx: &mut TestAppContext) {
    use protrack4::commands::AddBlocker;
    use protrack4::domain::ConversionBlockReason;

    cx.executor().allow_parking();

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("phase8.protrack");
    let prefs_path = dir.path().join("preferences.json");

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        protrack4::ui::register_keybindings(cx);
    });

    let config = LaunchConfig {
        db_override: Some(db_path.clone()),
        preferences_path: Some(prefs_path),
    };

    let (handle, shell) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config, window, cx)),
        )
        .expect("open Phase 8 WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let (app_state, store) = shell.read_with(cx, |view, _| {
        (view.app_state().clone(), view.store().clone())
    });

    wait_for_workspace_idle(cx, &app_state).await;

    let pid = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateProject::new(
                    "Conversion Suite",
                    "CNV",
                    "",
                    "#6366F1",
                    ProjectStatus::Active,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_project_id
        .unwrap();

    let fp_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    None,
                    "Parent Feature",
                    "",
                    ItemStatus::Todo,
                    Priority::High,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let fleaf_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    Some(fp_id),
                    "Leaf Subfeature",
                    "",
                    ItemStatus::InProgress,
                    Priority::Medium,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let fb_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    None,
                    "Blocker Feature",
                    "",
                    ItemStatus::Todo,
                    Priority::Urgent,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let wi_unalloc_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateWorkItem::new(
                    pid,
                    "Unallocated Task",
                    "",
                    ItemStatus::Todo,
                    Priority::High,
                    vec![],
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let wi_shared_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateWorkItem::new(
                    pid,
                    "Shared Task",
                    "",
                    ItemStatus::Todo,
                    Priority::Low,
                    vec![fp_id, fb_id],
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    // Outgoing blockers on both fleaf_id and wi_unalloc_id pointing to fb_id (must survive conversion)
    app_state
        .update(cx, |app, cx| {
            app.execute_command(Box::new(AddBlocker::new(fleaf_id, fb_id)), cx)
        })
        .await
        .unwrap();
    app_state
        .update(cx, |app, cx| {
            app.execute_command(Box::new(AddBlocker::new(wi_unalloc_id, fb_id)), cx)
        })
        .await
        .unwrap();

    wait_until(cx, "5 items loaded", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.active_items().len() == 5)
    })
    .await;

    let (wi_unalloc_num, wi_unalloc_key, fleaf_key) = cx.update(|cx| {
        let s = store.read(cx).snapshot().unwrap();
        let w = s.item(wi_unalloc_id).unwrap();
        let l = s.item(fleaf_id).unwrap();
        assert_eq!(
            s.conversion_eligibility(fp_id).unwrap().reasons(),
            &[
                ConversionBlockReason::HasChildFeatures(1),
                ConversionBlockReason::HasAttachedWorkItems(1),
            ]
        );
        assert!(s.conversion_tooltip(fp_id).contains("has 1 child Feature and has 1 attached Work Item"));
        assert_eq!(
            s.conversion_eligibility(fb_id).unwrap().reasons(),
            &[
                ConversionBlockReason::HasAttachedWorkItems(1),
                ConversionBlockReason::HasActiveDependents(2),
            ]
        );
        assert_eq!(
            s.conversion_eligibility(wi_shared_id).unwrap().reasons(),
            &[ConversionBlockReason::AttachedToMultipleFeatures(2)]
        );
        assert!(s.conversion_tooltip(wi_shared_id).contains("2 Features"));
        (
            w.item_number as usize,
            w.item_key.clone(),
            l.item_key.clone(),
        )
    });

    // 1. In Unallocated View, convert wi_unalloc -> Root Feature preserving item_key and outgoing blocker
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("sidebar-unallocated-row", cx);
        window.render_frame(cx);
        assert_eq!(store.read(cx).active_tab(), CenterViewTab::Unallocated);
        window.click(("unallocated-convert-btn", wi_unalloc_num), cx);
    })
    .unwrap();

    wait_until(cx, "wi_unalloc converted to root Feature", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.item(wi_unalloc_id).is_some_and(|i| {
                i.kind == ItemKind::Feature
                    && i.item_key == wi_unalloc_key
                    && i.parent_feature_id.is_none()
            }) && s.unallocated_work_items().is_empty()
                && s.direct_blockers(wi_unalloc_id).len() == 1
        })
    })
    .await;

    // 2. In Inspector, convert that root Feature back to Unallocated Work Item
    cx.update_window(handle.into(), |_, window, cx| {
        store.update(cx, |s, cx| {
            s.select_item(Some(wi_unalloc_id));
            cx.notify();
        });
        window.render_frame(cx);
        window.click("inspector-convert-btn", cx);
    })
    .unwrap();

    wait_until(cx, "converted back to Unallocated Work Item", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.item(wi_unalloc_id)
                .is_some_and(|i| i.kind == ItemKind::WorkItem && i.item_key == wi_unalloc_key)
                && s.unallocated_work_items().len() == 1
                && s.direct_blockers(wi_unalloc_id).len() == 1
        })
    })
    .await;

    // 3. Convert Leaf Subfeature -> Work Item attached to [Parent Feature] -> back to Child Feature under Parent Feature
    cx.update_window(handle.into(), |_, window, cx| {
        store.update(cx, |s, cx| {
            s.select_item(Some(fleaf_id));
            cx.notify();
        });
        window.render_frame(cx);
        window.click("inspector-convert-btn", cx);
    })
    .unwrap();

    wait_until(cx, "Leaf Feature converted to Work Item under Parent", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.item(fleaf_id).is_some_and(|i| {
                i.kind == ItemKind::WorkItem
                    && i.item_key == fleaf_key
                    && i.parent_feature_id.is_none()
            }) && s.features_for_work_item(fleaf_id).iter().any(|f| f.id == fp_id)
                && s.direct_blockers(fleaf_id).len() == 1
        })
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("inspector-convert-btn", cx);
    })
    .unwrap();

    wait_until(cx, "Work Item converted back to Child Feature under Parent", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.item(fleaf_id).is_some_and(|i| {
                i.kind == ItemKind::Feature
                    && i.item_key == fleaf_key
                    && i.parent_feature_id == Some(fp_id)
            }) && s.direct_blockers(fleaf_id).len() == 1
        })
    })
    .await;

    app_state.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());
}

#[gpui_kit::test]
async fn phase9_filtered_virtualized_table_view(cx: &mut TestAppContext) {
    use gpui_kit::component::table::ColumnSort;
    use protrack4::commands::{AddBlocker, SetItemTags, UpsertAssignee, UpsertTag};
    use protrack4::ui::table_view::{
        AssigneeFilter, BlockFilter, PriorityFilter, TableSortColumn,
    };

    cx.executor().allow_parking();

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("phase9.protrack");
    let prefs_path = dir.path().join("preferences.json");

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        protrack4::ui::register_keybindings(cx);
    });

    let config = LaunchConfig {
        db_override: Some(db_path.clone()),
        preferences_path: Some(prefs_path),
    };

    let (handle, shell) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config, window, cx)),
        )
        .expect("open Phase 9 WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let (app_state, store, table_view) = shell.read_with(cx, |view, _| {
        (
            view.app_state().clone(),
            view.store().clone(),
            view.table_view().clone(),
        )
    });

    wait_for_workspace_idle(cx, &app_state).await;

    // 1. Seed Project, Assignees, Tags, Features, shared 3-Feature WI, warning WI, and 50+ items
    let pid = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateProject::new(
                    "Table Scale",
                    "TBL",
                    "",
                    "#6366F1",
                    ProjectStatus::Active,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_project_id
        .unwrap();

    let mb_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(UpsertAssignee::create(
                    "Mark Benson",
                    "MB",
                    "#6366F1",
                    None,
                    "",
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_assignee_id
        .unwrap();

    let tag_core_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(Box::new(UpsertTag::create(pid, "core", "#8B5CF6")), cx)
        })
        .await
        .unwrap()
        .created_tag_id
        .unwrap();

    let f_a_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    None,
                    "Core Auth",
                    "",
                    ItemStatus::InProgress,
                    Priority::High,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let f_b_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    None,
                    "CLI Sync",
                    "",
                    ItemStatus::Todo,
                    Priority::High,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let f_c_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    None,
                    "Web Sync",
                    "",
                    ItemStatus::Todo,
                    Priority::Medium,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let f_blocker_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    None,
                    "Active Blocker",
                    "",
                    ItemStatus::Todo,
                    Priority::Urgent,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let f_cancelled_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    None,
                    "Cancelled Blocker",
                    "",
                    ItemStatus::Cancelled,
                    Priority::Low,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    // Block Core Auth on Active Blocker -> shared WI across (Core Auth, CLI Sync, Web Sync) is PartiallyBlocked (ReadyToWork!)
    app_state
        .update(cx, |app, cx| {
            app.execute_command(Box::new(AddBlocker::new(f_a_id, f_blocker_id)), cx)
        })
        .await
        .unwrap();

    let shared_wi_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateWorkItem::new(
                    pid,
                    "OAuth token refresh hook",
                    "",
                    ItemStatus::Todo,
                    Priority::High,
                    vec![f_a_id, f_b_id, f_c_id],
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(UpdateItemFields {
                    item_id: shared_wi_id,
                    title: None,
                    description: None,
                    status: None,
                    priority: None,
                    assignee_id: Some(Some(mb_id)),
                }),
                cx,
            )
        })
        .await
        .unwrap();

    app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(SetItemTags::new(shared_wi_id, vec![tag_core_id])),
                cx,
            )
        })
        .await
        .unwrap();

    let warn_wi_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateWorkItem::new(
                    pid,
                    "Warning task",
                    "",
                    ItemStatus::Todo,
                    Priority::Medium,
                    vec![f_c_id],
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    app_state
        .update(cx, |app, cx| {
            app.execute_command(Box::new(AddBlocker::new(warn_wi_id, f_cancelled_id)), cx)
        })
        .await
        .unwrap();

    // Add 50 bulk Work Items so virtualized DataTable renders 57 total items
    for i in 1..=50 {
        let prio = if i % 10 == 0 {
            Priority::High
        } else {
            Priority::Low
        };
        app_state
            .update(cx, |app, cx| {
                app.execute_command(
                    Box::new(CreateWorkItem::new(
                        pid,
                        format!("Bulk item {i:02}"),
                        "",
                        ItemStatus::Todo,
                        prio,
                        vec![f_c_id],
                    )),
                    cx,
                )
            })
            .await
            .unwrap();
    }

    wait_until(cx, "57 items loaded", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.active_items().len() == 57)
    })
    .await;

    let shared_wi_num = cx.update(|cx| {
        store
            .read(cx)
            .snapshot()
            .unwrap()
            .item(shared_wi_id)
            .unwrap()
            .item_number as usize
    });

    // 2. Switch to Table tab and verify deduplication + multi-facet filtering
    cx.update_window(handle.into(), |_, window, cx| {
        store.update(cx, |s, cx| {
            s.set_active_tab(CenterViewTab::Table);
            cx.notify();
        });
        window.render_frame(cx);
        window.render_frame(cx);

        table_view.update(cx, |tv, cx| {
            assert_eq!(tv.filtered_rows(cx).len(), 57);
            assert_eq!(
                tv.filtered_rows(cx)
                    .iter()
                    .filter(|r| r.item.id == shared_wi_id)
                    .count(),
                1
            );

            // Multi-facet filter: ReadyToWork + Priority High + Assignee MB + Tag core
            tv.set_block_filter(BlockFilter::ReadyToWork, cx);
            tv.set_priority_filter(PriorityFilter::Specific(Priority::High), cx);
            tv.set_assignee_filter(AssigneeFilter::Specific(mb_id), cx);
            tv.toggle_tag_filter(tag_core_id, cx);

            let rows = tv.filtered_rows(cx);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].item.id, shared_wi_id);
            assert_eq!(rows[0].feature_labels.len(), 3);
        });
        window.render_frame(cx);

        // Click the rendered DataTable row and verify Inspector / store selection syncs
        window.click(("table-row", shared_wi_num), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(store.read(cx).selected_item_id(), Some(shared_wi_id));

        // HasWarnings filter surfaces warn_wi_id
        table_view.update(cx, |tv, cx| {
            tv.clear_filters(None, cx);
            tv.set_block_filter(BlockFilter::HasWarnings, cx);
            let rows = tv.filtered_rows(cx);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].item.id, warn_wi_id);
        });
        window.render_frame(cx);

        // Clear filters & sort by Priority descending -> Urgent (Active Blocker) first
        window.click("table-filter-clear", cx);
        window.render_frame(cx);
        table_view.update(cx, |tv, cx| {
            assert_eq!(tv.filtered_rows(cx).len(), 57);
            tv.sort_by(TableSortColumn::Priority, ColumnSort::Descending, cx);
            assert_eq!(tv.filtered_rows(cx)[0].item.id, f_blocker_id);
        });
        window.render_frame(cx);
    })
    .unwrap();

    // Right-click a row in DataTable to verify ContextMenu opens cleanly
    cx.update_window(handle.into(), |_, window, cx| {
        window.right_click(("table-row", shared_wi_num), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
        window.render_frame(cx);
    })
    .unwrap();

    app_state.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());
}


#[gpui_kit::test]
async fn phase10_full_cascade_trash_restore_and_permanent_purge(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("phase10.protrack");
    let prefs_path = dir.path().join("preferences.json");

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        protrack4::ui::register_keybindings(cx);
    });

    let config = LaunchConfig {
        db_override: Some(db_path.clone()),
        preferences_path: Some(prefs_path),
    };

    let (handle, shell) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config, window, cx)),
        )
        .expect("open Phase 10 WorkspaceShell");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let (app_state, store) = shell.read_with(cx, |view, _| {
        (view.app_state().clone(), view.store().clone())
    });

    wait_for_workspace_idle(cx, &app_state).await;

    // 1. Seed a Project with:
    //    F1 -> F2, F3, exclusive W2 on [F2], shared W1 on [F2, F3], and unallocated W3
    let pid = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateProject::new(
                    "Trash Suite",
                    "TRS",
                    "",
                    "#6366F1",
                    ProjectStatus::Active,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_project_id
        .unwrap();

    let f1_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    None,
                    "Auth Epic",
                    "",
                    ItemStatus::Todo,
                    Priority::High,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let f2_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    Some(f1_id),
                    "OAuth Flow",
                    "",
                    ItemStatus::Todo,
                    Priority::Medium,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let f3_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateFeature::new(
                    pid,
                    None,
                    "CLI Client",
                    "",
                    ItemStatus::Todo,
                    Priority::Medium,
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let w1_shared_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateWorkItem::new(
                    pid,
                    "Shared Token Refresh",
                    "",
                    ItemStatus::Todo,
                    Priority::High,
                    vec![f2_id, f3_id],
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let _w2_excl_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateWorkItem::new(
                    pid,
                    "Exclusive Callback",
                    "",
                    ItemStatus::Todo,
                    Priority::Low,
                    vec![f2_id],
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let w3_unalloc_id = app_state
        .update(cx, |app, cx| {
            app.execute_command(
                Box::new(CreateWorkItem::new(
                    pid,
                    "Standalone Task",
                    "",
                    ItemStatus::Todo,
                    Priority::None,
                    vec![],
                )),
                cx,
            )
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    wait_until(cx, "6 items loaded", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.active_items().len() == 6)
    })
    .await;

    let (f1_num, f3_num, w3_num) = cx.update(|cx| {
        let s = store.read(cx).snapshot().unwrap();
        (
            s.item(f1_id).unwrap().item_number as usize,
            s.item(f3_id).unwrap().item_number as usize,
            s.item(w3_unalloc_id).unwrap().item_number as usize,
        )
    });

    // 2. Select F1 in Inspector, scroll to the footer, and click [Move to Trash]
    cx.update_window(handle.into(), |_, window, cx| {
        store.update(cx, |s, cx| {
            s.select_item(Some(f1_id));
            cx.notify();
        });
        window.render_frame(cx);
        window.scroll(
            "inspector-viewport",
            ScrollDelta::Pixels(point(px(0.), px(-600.))),
            cx,
        );
        window.render_frame(cx);
        window.click("inspector-trash-btn", cx);
    })
    .unwrap();

    wait_until(cx, "F1 subtree moved to Trash and shared W1 kept active", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.trash_count == 1
                && s.active_items().len() == 3
                && s.item(w1_shared_id).is_some_and(|w| w.is_active())
                && s.features_for_work_item(w1_shared_id).len() == 1
        })
    })
    .await;

    // 3. Click Sidebar Trash nav -> TrashView renders F1 group; click [Restore] -> subtree + shared link return
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("sidebar-trash-nav", cx);
        window.render_frame(cx);
        assert_eq!(store.read(cx).active_tab(), CenterViewTab::Trash);
        assert!(window.try_find(("trash-group-row", f1_num)).is_some());
        window.click(("trash-restore-btn", f1_num), cx);
    })
    .unwrap();

    wait_until(cx, "F1 subtree restored with shared W1 link", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.trash_count == 0
                && s.active_items().len() == 6
                && s.features_for_work_item(w1_shared_id).len() == 2
        })
    })
    .await;

    // 4. Right-click F1 in Outline and verify context menu triggers Move to Trash;
    //    also trash W3 from Table context menu so Trash has 2 groups
    cx.update_window(handle.into(), |_, window, cx| {
        store.update(cx, |s, cx| {
            s.set_active_tab(CenterViewTab::Outline);
            s.expand_all_features();
            cx.notify();
        });
        window.render_frame(cx);
        window.right_click(("outline-feature", f1_num), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("popup-menu").click(11usize, cx);
    })
    .unwrap();

    wait_until(cx, "F1 trashed via Outline context menu", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.trash_count == 1 && s.active_items().len() == 3)
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        store.update(cx, |s, cx| {
            s.set_active_tab(CenterViewTab::Table);
            cx.notify();
        });
        window.render_frame(cx);
        window.render_frame(cx);
        window.right_click(("table-row", w3_num), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("popup-menu").click(7usize, cx);
    })
    .unwrap();

    wait_until(cx, "W3 trashed via Table context menu", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.trash_count == 2 && s.active_items().len() == 2)
    })
    .await;

    // 5. Open Trash View -> test [Delete Forever] Cancel then Confirm on F1 (sparing W1 on F3)
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("sidebar-trash-nav", cx);
        window.render_frame(cx);
        assert!(window.try_find(("trash-group-row", f1_num)).is_some());
        assert!(window.try_find(("trash-group-row", w3_num)).is_some());

        window.click(("trash-purge-btn", f1_num), cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        window.click("trash-confirm-cancel", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
        assert_eq!(store.read(cx).snapshot().unwrap().trash_count, 2);

        window.click(("trash-purge-btn", f1_num), cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        window.click("trash-confirm-purge", cx);
    })
    .unwrap();

    wait_until(cx, "F1 permanently purged while W1 survives", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.trash_count == 1
                && s.item(w1_shared_id).is_some_and(|w| w.is_active())
                && s.features_for_work_item(w1_shared_id).len() == 1
                && s.features_for_work_item(w1_shared_id)[0].item_number as usize == f3_num
        })
    })
    .await;

    // 6. Test [Empty Trash] confirmation dialog removes W3 cleanly
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("trash-empty-btn", cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        window.click("trash-confirm-empty", cx);
    })
    .unwrap();

    wait_until(cx, "Trash emptied", |cx| {
        store
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.trash_count == 0 && s.trash_groups.is_empty())
    })
    .await;

    app_state.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());
}

fn drag_to_with_modifiers(
    window: &mut gpui_kit::Window,
    from_id: impl Into<gpui_kit::ElementId>,
    to_id: impl Into<gpui_kit::ElementId>,
    modifiers: gpui_kit::Modifiers,
    cx: &mut gpui_kit::App,
) {
    use gpui_kit::{InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent};

    window.render_frame(cx);
    let from = window.find(from_id).bounds().center();
    let to = window.find(to_id).bounds().center();

    window.dispatch_event(
        MouseMoveEvent {
            position: from,
            pressed_button: None,
            modifiers,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);

    window.dispatch_event(
        MouseDownEvent {
            button: MouseButton::Left,
            position: from,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);

    for step in 1..=8 {
        let fraction = step as f32 / 8.0;
        let pos = point(
            from.x + (to.x - from.x) * fraction,
            from.y + (to.y - from.y) * fraction,
        );
        window.dispatch_event(
            MouseMoveEvent {
                position: pos,
                pressed_button: Some(MouseButton::Left),
                modifiers,
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    }

    window.dispatch_event(
        MouseUpEvent {
            button: MouseButton::Left,
            position: to,
            modifiers,
            click_count: 1,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

#[gpui_kit::test]
async fn phase11_dnd_quick_search_and_quick_add(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("phase11.protrack");
    let prefs_path = dir.path().join("preferences.json");

    {
        let mut manager = WorkspaceManager::new();
        manager
            .create_workspace(&db_path, Some("Phase11"), false)
            .unwrap();
        manager.close_workspace().unwrap();
    }

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        protrack4::ui::register_keybindings(cx);
    });

    let config = LaunchConfig {
        db_override: Some(db_path.clone()),
        preferences_path: Some(prefs_path),
    };

    let (handle, shell) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Default::default(),
            size: size(px(1280.), px(820.)),
        };
        let (window, shell) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceShell::new_with_config(config, window, cx)),
        )
        .expect("open WorkspaceShell test window");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let (app_state, store) = shell.read_with(cx, |view, _| {
        (view.app_state().clone(), view.store().clone())
    });
    wait_for_workspace_idle(cx, &app_state).await;

    // Seed two projects, assignees, tags, features, and work items
    let p1_id = cx
        .update(|cx| {
            app_state.update(cx, |app, cx| {
                app.execute_command(
                    Box::new(CreateProject::new(
                        "Platform",
                        "PRO",
                        "",
                        "#6366F1",
                        ProjectStatus::Active,
                    )),
                    cx,
                )
            })
        })
        .await
        .unwrap()
        .created_project_id
        .unwrap();

    let p2_id = cx
        .update(|cx| {
            app_state.update(cx, |app, cx| {
                app.execute_command(
                    Box::new(CreateProject::new(
                        "Security",
                        "SEC",
                        "",
                        "#10B981",
                        ProjectStatus::Active,
                    )),
                    cx,
                )
            })
        })
        .await
        .unwrap()
        .created_project_id
        .unwrap();

    let s1_id = cx
        .update(|cx| {
            app_state.update(cx, |app, cx| {
                app.execute_command(
                    Box::new(CreateFeature::new(
                        p2_id,
                        None,
                        "Audit Trail Engine",
                        "Security migration log",
                        ItemStatus::InProgress,
                        Priority::High,
                    )),
                    cx,
                )
            })
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    // Switch back to P1 and seed F1, F2, F3, W1, W2, U1, U2, Assignee, Tag
    cx.update(|cx| {
        app_state.update(cx, |app, cx| app.switch_project(p1_id, cx))
    })
    .await
    .unwrap();

    let _tag_id = cx
        .update(|cx| {
            app_state.update(cx, |app, cx| {
                app.execute_command(Box::new(UpsertTag::create(p1_id, "core", "#8B5CF6")), cx)
            })
        })
        .await
        .unwrap()
        .created_tag_id
        .unwrap();

    let _assignee_id = cx
        .update(|cx| {
            app_state.update(cx, |app, cx| {
                app.execute_command(
                    Box::new(UpsertAssignee::create("Mark", "MB", "#6366F1", None, "")),
                    cx,
                )
            })
        })
        .await
        .unwrap()
        .created_assignee_id
        .unwrap();

    let mut f_ids = Vec::new();
    for title in ["Auth Core", "Token Cache", "Billing"] {
        let id = cx
            .update(|cx| {
                app_state.update(cx, |app, cx| {
                    app.execute_command(
                        Box::new(CreateFeature::new(
                            p1_id,
                            None,
                            title,
                            "",
                            ItemStatus::Todo,
                            Priority::Medium,
                        )),
                        cx,
                    )
                })
            })
            .await
            .unwrap()
            .created_item_id
            .unwrap();
        f_ids.push(id);
    }
    let (f1_id, f2_id, f3_id) = (f_ids[0], f_ids[1], f_ids[2]);

    let w1_id = cx
        .update(|cx| {
            app_state.update(cx, |app, cx| {
                app.execute_command(
                    Box::new(CreateWorkItem::new(
                        p1_id,
                        "JWT Signer",
                        "",
                        ItemStatus::Todo,
                        Priority::High,
                        vec![f1_id],
                    )),
                    cx,
                )
            })
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let w2_id = cx
        .update(|cx| {
            app_state.update(cx, |app, cx| {
                app.execute_command(
                    Box::new(CreateWorkItem::new(
                        p1_id,
                        "Session Store",
                        "",
                        ItemStatus::Todo,
                        Priority::Low,
                        vec![f1_id],
                    )),
                    cx,
                )
            })
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let u1_id = cx
        .update(|cx| {
            app_state.update(cx, |app, cx| {
                app.execute_command(
                    Box::new(CreateWorkItem::new(
                        p1_id,
                        "Inbox One",
                        "",
                        ItemStatus::Backlog,
                        Priority::None,
                        vec![],
                    )),
                    cx,
                )
            })
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    let u2_id = cx
        .update(|cx| {
            app_state.update(cx, |app, cx| {
                app.execute_command(
                    Box::new(CreateWorkItem::new(
                        p1_id,
                        "Inbox Two",
                        "",
                        ItemStatus::Todo,
                        Priority::Medium,
                        vec![],
                    )),
                    cx,
                )
            })
        })
        .await
        .unwrap()
        .created_item_id
        .unwrap();

    wait_until(cx, "P1 seeded snapshot ready", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.project.id == p1_id
                && s.child_features(None).len() == 3
                && s.work_items_for_feature(f1_id).len() == 2
                && s.unallocated_work_items().len() == 2
        })
    })
    .await;

    let (f1_num, f2_num, f3_num, w1_num, w2_num, u1_num, u2_num) = cx.update(|cx| {
        let s = store.read(cx).snapshot().unwrap();
        (
            s.item(f1_id).unwrap().item_number as usize,
            s.item(f2_id).unwrap().item_number as usize,
            s.item(f3_id).unwrap().item_number as usize,
            s.item(w1_id).unwrap().item_number as usize,
            s.item(w2_id).unwrap().item_number as usize,
            s.item(u1_id).unwrap().item_number as usize,
            s.item(u2_id).unwrap().item_number as usize,
        )
    });

    // 1. Sidebar DnD: Feature sibling reorder (F2 before F1)
    cx.update_window(handle.into(), |_, window, cx| {
        store.update(cx, |s, cx| {
            s.set_active_tab(CenterViewTab::Outline);
            s.expand_all_features();
            cx.notify();
        });
        window.render_frame(cx);
        window.drag_to(
            ("sidebar-feature", f2_num),
            ("sidebar-feature-gap-before", f1_num),
            cx,
        );
    })
    .unwrap();

    wait_until(cx, "F2 reordered before F1", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.child_features(None)
                .into_iter()
                .map(|f| f.id)
                .collect::<Vec<_>>()
                == vec![f2_id, f1_id, f3_id]
        })
    })
    .await;

    // 2. Sidebar DnD: Reparent F2 under F1, test cycle rejection F1->F2, then promote F2 to Root
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.drag_to(("sidebar-feature", f2_num), ("sidebar-feature", f1_num), cx);
    })
    .unwrap();

    wait_until(cx, "F2 reparented under F1", |cx| {
        store
            .read(cx)
            .snapshot()
            .and_then(|s| s.item(f2_id))
            .is_some_and(|f| f.parent_feature_id == Some(f1_id))
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.drag_to(("sidebar-feature", f1_num), ("sidebar-feature", f2_num), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let s = store.read(cx).snapshot().unwrap();
        assert_eq!(s.item(f1_id).unwrap().parent_feature_id, None);
        assert_eq!(s.item(f2_id).unwrap().parent_feature_id, Some(f1_id));
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.drag_to(("sidebar-feature", f2_num), "sidebar-root-drop-zone", cx);
    })
    .unwrap();

    wait_until(cx, "F2 promoted back to root", |cx| {
        store
            .read(cx)
            .snapshot()
            .and_then(|s| s.item(f2_id))
            .is_some_and(|f| f.parent_feature_id.is_none())
    })
    .await;

    // 3. Outline DnD: reorder W2 before W1 inside F1
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.drag_to(
            ("outline-wi", f1_num * 100_000 + w2_num),
            ("outline-wi-gap-before", f1_num * 100_000 + w1_num),
            cx,
        );
    })
    .unwrap();

    wait_until(cx, "W2 reordered before W1 inside F1", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.work_items_for_feature(f1_id)
                .into_iter()
                .map(|w| w.id)
                .collect::<Vec<_>>()
                == vec![w2_id, w1_id]
        })
    })
    .await;

    // 4. Outline DnD: default move W1 from F1 -> F2, then Option/Alt-drag W1 from F2 -> F3 (multi-attach)
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.drag_to(
            ("outline-wi", f1_num * 100_000 + w1_num),
            ("outline-feature", f2_num),
            cx,
        );
    })
    .unwrap();

    wait_until(cx, "W1 moved from F1 to F2", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.features_for_work_item(w1_id)
                .into_iter()
                .map(|f| f.id)
                .collect::<Vec<_>>()
                == vec![f2_id]
        })
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        drag_to_with_modifiers(
            window,
            ("outline-wi", f2_num * 100_000 + w1_num),
            ("outline-feature", f3_num),
            gpui_kit::Modifiers::alt(),
            cx,
        );
    })
    .unwrap();

    wait_until(cx, "W1 Alt-attached to F3 while keeping F2", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            let fids: Vec<_> = s
                .features_for_work_item(w1_id)
                .into_iter()
                .map(|f| f.id)
                .collect();
            fids.len() == 2 && fids.contains(&f2_id) && fids.contains(&f3_id)
        })
    })
    .await;

    // 5. Drag W2 (single feature) -> sidebar-unallocated-row, then drag W1 (multi-feature) -> confirm Unallocate All
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.drag_to(
            ("outline-wi", f1_num * 100_000 + w2_num),
            "sidebar-unallocated-row",
            cx,
        );
    })
    .unwrap();

    wait_until(cx, "W2 unallocated via sidebar drop", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.unallocated_work_items().iter().any(|w| w.id == w2_id)
                && s.unallocated_work_items().len() == 3
        })
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.drag_to(
            ("outline-wi", f2_num * 100_000 + w1_num),
            "sidebar-unallocated-row",
            cx,
        );
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        window.click("dnd-unalloc-all", cx);
    })
    .unwrap();

    wait_until(cx, "W1 unallocated via multi-feature drop dialog", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.unallocated_work_items().iter().any(|w| w.id == w1_id)
                && s.unallocated_work_items().len() == 4
        })
    })
    .await;

    // 6. Unallocated View: reorder U2 before U1, then drag U1 onto Sidebar Feature F1
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("sidebar-unallocated-row", cx);
        window.render_frame(cx);
        window.drag_to(
            ("unallocated-row", u2_num),
            ("unallocated-gap-before", u1_num),
            cx,
        );
    })
    .unwrap();

    wait_until(cx, "U2 reordered before U1 in Unallocated", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            let ids: Vec<_> = s.unallocated_work_items().into_iter().map(|w| w.id).collect();
            let pos_u2 = ids.iter().position(|id| *id == u2_id);
            let pos_u1 = ids.iter().position(|id| *id == u1_id);
            matches!((pos_u2, pos_u1), (Some(a), Some(b)) if a < b)
        })
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.drag_to(("unallocated-row", u1_num), ("sidebar-feature", f1_num), cx);
    })
    .unwrap();

    wait_until(cx, "U1 allocated to F1 via drag from Unallocated", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.work_items_for_feature(f1_id).iter().any(|w| w.id == u1_id)
                && s.unallocated_work_items().len() == 3
        })
    })
    .await;

    // 7. Quick Search (Cmd+K): search across Projects & jump to SEC-1, plus Toggle Theme
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("toolbar-search-btn", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert_eq!(window.find("quick-search-input").focused(), Some(true));
        window.input("Audit", cx);
    })
    .unwrap();

    // Wait for active-project search (0 items), then switch to All Projects to find SEC-1
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("quick-search-item", 0usize)).is_none());
        window.click("quick-search-scope-all", cx);
    })
    .unwrap();

    wait_until(cx, "All Projects search finds SEC-1", |cx| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.try_find(("quick-search-item", 0usize)).is_some()
        })
        .unwrap()
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.click(("quick-search-item", 0usize), cx);
    })
    .unwrap();

    wait_until(cx, "Jump from Quick Search switches to SEC project & selects SEC-1", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.project.id == p2_id
                && store.read(cx).selected_item_id() == Some(s1_id)
                && store.read(cx).active_tab() == CenterViewTab::Outline
        })
    })
    .await;

    // Test Quick Search action: Toggle Theme + switch back to P1
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("toolbar-search-btn", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        window.click("quick-search-cmd-toggle-theme", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
        assert_eq!(store.read(cx).theme_preference(), ThemePreference::Light);
    })
    .unwrap();

    cx.update(|cx| {
        app_state.update(cx, |app, cx| app.switch_project(p1_id, cx))
    })
    .await
    .unwrap();

    wait_until(cx, "switched back to P1", |cx| {
        store.read(cx).snapshot().is_some_and(|s| s.project.id == p1_id)
    })
    .await;

    // 8. Quick Add (Cmd+N): Unallocated context vs Feature context
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("sidebar-unallocated-row", cx);
        window.render_frame(cx);
        window.click("toolbar-new-btn", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx));
        assert_eq!(window.find("quick-add-title").focused(), Some(true));
        window.input("Quick Unallocated Task", cx);
        window.click(("quick-add-tag", 0usize), cx);
        window.click("quick-add-submit", cx);
    })
    .unwrap();

    wait_until(cx, "Quick Add creates unallocated WI with tag", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.unallocated_work_items()
                .iter()
                .find(|w| w.title == "Quick Unallocated Task")
                .is_some_and(|w| s.tags_for_item(w.id).len() == 1)
        })
    })
    .await;

    // Contextual Quick Add from Outline with F1 selected: creates WI on F1, then Sub-Feature under F1
    cx.update_window(handle.into(), |_, window, cx| {
        store.update(cx, |s, cx| {
            s.set_active_tab(CenterViewTab::Outline);
            s.select_item(Some(f1_id));
            cx.notify();
        });
        window.render_frame(cx);
        window.click("toolbar-new-btn", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        window.input("Contextual F1 Task", cx);
        window.click("quick-add-submit", cx);
    })
    .unwrap();

    wait_until(cx, "Contextual Quick Add attaches WI to F1", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.work_items_for_feature(f1_id)
                .iter()
                .any(|w| w.title == "Contextual F1 Task")
        })
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        store.update(cx, |s, cx| {
            s.select_item(Some(f1_id));
            cx.notify();
        });
        window.render_frame(cx);
        window.click("toolbar-new-btn", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        window.click("quick-add-kind-feature", cx);
        window.click("quick-add-title", cx);
        window.input("Contextual Sub-Feature", cx);
        window.click("quick-add-submit", cx);
    })
    .unwrap();

    wait_until(cx, "Contextual Quick Add creates Sub-Feature under F1", |cx| {
        store.read(cx).snapshot().is_some_and(|s| {
            s.child_features(Some(f1_id))
                .iter()
                .any(|f| f.title == "Contextual Sub-Feature")
        })
    })
    .await;

    app_state.update(cx, |app, cx| {
        app.close_workspace_sync(cx).unwrap();
    });
    assert!(!wal_path_for(&db_path).exists());
    assert!(!shm_path_for(&db_path).exists());
    assert!(!lock_path_for(&db_path).exists());
}
