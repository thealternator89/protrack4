use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, Bounds, TestAppContext, WindowBounds, WindowOptions, base::Root, px, size,
};
use protrack4::domain::ItemKind;
use protrack4::state::{CenterViewTab, ThemePreference};
use protrack4::ui::WorkspaceShell;

#[gpui_kit::test]
fn shell_boots_and_navigates_views_selection_and_themes(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        protrack4::ui::register_keybindings(cx);
    });

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
            |window, cx| cx.new(|cx| WorkspaceShell::new(window, cx)),
        )
        .expect("open WorkspaceShell test window");
        (window.downcast::<Root>().expect("Base Root"), shell)
    });

    let store = shell.read_with(cx, |view, _| view.store().clone());

    // Initial render & seed verification
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);

        let state = store.read(cx);
        assert!(state.sidebar_open());
        assert!(state.inspector_open());
        assert_eq!(state.active_tab(), CenterViewTab::Outline);
        assert_eq!(state.snapshot().unallocated_work_items().len(), 2);
        assert_eq!(
            state.selected_item().map(|item| item.item_key.as_str()),
            Some("PRO-1")
        );
    })
    .unwrap();

    // Toggle sidebar and inspector via bound Cmd+B / Cmd+I keystrokes
    cx.update_window(handle.into(), |_, window, cx| {
        window.press("secondary-b", cx);
        assert!(!store.read(cx).sidebar_open());
        window.press("secondary-b", cx);
        assert!(store.read(cx).sidebar_open());

        window.press("secondary-i", cx);
        assert!(!store.read(cx).inspector_open());
        window.press("secondary-i", cx);
        assert!(store.read(cx).inspector_open());
    })
    .unwrap();

    // Switch through all tabs, select shared & unallocated items, collapse feature, and render each frame
    cx.update_window(handle.into(), |_, window, cx| {
        let (pro_2_id, pro_5_id, pro_8_id) = {
            let state = store.read(cx);
            let snap = state.snapshot();
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
            store.update(cx, |s, cx| {
                s.set_theme_preference(pref);
                cx.notify();
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
}
