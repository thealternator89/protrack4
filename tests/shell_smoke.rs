use std::fs;
use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme as _, WindowExt as _};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, Bounds, Entity, TestAppContext, WindowBounds, WindowOptions, base::Root, px,
    size,
};
use protrack4::commands::{
    CreateFeature, CreateProject, ReparentFeature, ReorderSiblingFeatures, UpdateItemFields,
    UpdateProject,
};
use protrack4::domain::{ItemKind, ItemStatus, Priority, ProjectStatus};
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
