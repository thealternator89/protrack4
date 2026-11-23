use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    StyledExt as _, Theme, TitleBar, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Render, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, div, prelude::FluentBuilder as _, px,
};

use crate::domain::{ItemId, ItemKind, ProjectSnapshot, clamped_depth_badge, visual_indent_px};
use crate::state::{AppState, CenterViewTab, LaunchConfig, ProjectStore, ThemePreference};
use crate::ui::inspector::InspectorView;
use crate::ui::modals::{open_new_project_modal, open_workspace_modal};
use crate::ui::sidebar::render_sidebar;
use crate::ui::unallocated_view::UnallocatedView;
use crate::ui::{
    OpenWorkspace, ToggleInspector, ToggleSidebar, assignee_pill, build_project_switcher_menu,
    domain_tag_chip, kind_badge, parse_hex_or, priority_badge, progress_pill, status_badge,
    status_color, status_icon,
};

pub struct WorkspaceShell {
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    inspector: Entity<InspectorView>,
    unallocated_view: Entity<UnallocatedView>,
    focus_handle: FocusHandle,
    resizable_state: Entity<ResizableState>,
    sidebar_width: Pixels,
    inspector_width: Pixels,
    last_sidebar_open: bool,
    last_inspector_open: bool,
    last_applied_theme: ThemePreference,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_config(LaunchConfig::default(), window, cx)
    }

    pub fn new_with_config(
        config: LaunchConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let app_state = cx.new(|cx| AppState::new(config.clone(), cx));
        let store = app_state.read(cx).project_store().clone();
        let inspector = cx.new(|cx| InspectorView::new(app_state.clone(), store.clone(), window, cx));
        let unallocated_view =
            cx.new(|cx| UnallocatedView::new(app_state.clone(), store.clone(), window, cx));
        let prefs = app_state.read(cx).preferences().clone();

        prefs.theme.apply(Some(window), cx);

        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        let store_sub = cx.observe(&store, |this, store, cx| {
            let s = store.read(cx);
            let s_open = s.sidebar_open();
            let i_open = s.inspector_open();
            if s_open != this.last_sidebar_open || i_open != this.last_inspector_open {
                this.last_sidebar_open = s_open;
                this.last_inspector_open = i_open;
                this.resizable_state = cx.new(|_| ResizableState::default());
            }
            cx.notify();
        });
        let app_sub = cx.observe(&app_state, |_, _, cx| cx.notify());
        let appearance_sub = cx.observe_window_appearance(window, |this, window, cx| {
            if this.store.read(cx).theme_preference() == ThemePreference::System {
                Theme::sync_system_appearance(Some(window), cx);
                cx.notify();
            }
        });

        let app_for_close = app_state.clone();
        let inspector_for_close = inspector.clone();
        window.on_window_should_close(cx, move |_, cx| {
            inspector_for_close.update(cx, |view, cx| view.flush_pending_edits_for_shutdown(cx));
            if let Err(err) = app_for_close.update(cx, |app, cx| app.close_workspace_sync(cx)) {
                tracing::warn!(error = %err, "Failed to close workspace cleanly on window close");
            }
            true
        });

        app_state.update(cx, |app, cx| {
            app.startup_open(config.db_override, cx);
        });

        let resizable_state = cx.new(|_| ResizableState::default());
        let sidebar_width = px(prefs.sidebar_width);
        let inspector_width = px(prefs.inspector_width);
        let last_sidebar_open = !prefs.sidebar_collapsed;
        let last_inspector_open = prefs.inspector_open;

        Self {
            app_state,
            store,
            inspector,
            unallocated_view,
            focus_handle,
            resizable_state,
            sidebar_width,
            inspector_width,
            last_sidebar_open,
            last_inspector_open,
            last_applied_theme: prefs.theme,
            _subscriptions: vec![store_sub, app_sub, appearance_sub],
        }
    }

    pub fn app_state(&self) -> &Entity<AppState> {
        &self.app_state
    }

    pub fn store(&self) -> &Entity<ProjectStore> {
        &self.store
    }

    pub fn inspector(&self) -> &Entity<InspectorView> {
        &self.inspector
    }

    pub fn unallocated_view(&self) -> &Entity<UnallocatedView> {
        &self.unallocated_view
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    pub fn sidebar_width(&self) -> Pixels {
        self.sidebar_width
    }

    pub fn inspector_width(&self) -> Pixels {
        self.inspector_width
    }

    fn toggle_sidebar_action(
        &mut self,
        _: &ToggleSidebar,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_sidebar_inner(cx);
    }

    fn toggle_inspector_action(
        &mut self,
        _: &ToggleInspector,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_inspector_inner(cx);
    }

    fn open_workspace_action(
        &mut self,
        _: &OpenWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_workspace_modal(self.app_state.clone(), window, cx);
    }

    fn toggle_sidebar_inner(&mut self, cx: &mut Context<Self>) {
        let next = !self.store.read(cx).sidebar_open();
        self.app_state.update(cx, |app, cx| {
            app.set_sidebar_open(next, cx);
        });
        self.last_sidebar_open = next;
        self.resizable_state = cx.new(|_| ResizableState::default());
        cx.notify();
    }

    fn toggle_inspector_inner(&mut self, cx: &mut Context<Self>) {
        let next = !self.store.read(cx).inspector_open();
        self.app_state.update(cx, |app, cx| {
            app.set_inspector_open(next, cx);
        });
        self.last_inspector_open = next;
        self.resizable_state = cx.new(|_| ResizableState::default());
        cx.notify();
    }

    fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.store.read(cx).theme_preference().cycle();
        self.app_state.update(cx, |app, cx| {
            app.set_theme_preference(next, cx);
        });
        self.last_applied_theme = next;
        next.apply(Some(window), cx);
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.store.read(cx);
        let snapshot = state.snapshot();
        let has_workspace = state.has_workspace();
        let active_tab = state.active_tab();
        let sidebar_open = state.sidebar_open();
        let inspector_open = state.inspector_open();
        let theme_pref = state.theme_preference();
        let unallocated_count = snapshot.map_or(0, |s| s.unallocated_work_items().len());

        let amber = cx.theme().warning;
        let amber_bg = amber.opacity(if cx.theme().is_dark() { 0.22 } else { 0.14 });
        let amber_border = amber.opacity(if cx.theme().is_dark() { 0.42 } else { 0.30 });

        let theme_icon = match theme_pref {
            ThemePreference::System => IconName::Palette,
            ThemePreference::Light => IconName::Sun,
            ThemePreference::Dark => IconName::Moon,
        };

        let mut tab_bar = TabBar::new("center-view-tabs")
            .segmented()
            .small()
            .when_some(active_tab.toolbar_index(), |bar, ix| {
                bar.selected_index(ix)
            })
            .child(Tab::new().label("Outline"))
            .child(Tab::new().label("Table"))
            .child({
                let mut tab = Tab::new().label("Unallocated");
                if unallocated_count > 0 {
                    tab = tab.suffix(
                        div()
                            .ml_1p5()
                            .px_1p5()
                            .py_0p5()
                            .rounded(px(999.))
                            .bg(amber_bg)
                            .border_1()
                            .border_color(amber_border)
                            .text_size(px(10.))
                            .font_semibold()
                            .text_color(amber)
                            .child(unallocated_count.to_string()),
                    );
                }
                tab
            });

        let store = self.store.clone();
        tab_bar = tab_bar.on_click(move |ix, _, cx| {
            store.update(cx, |s, cx| {
                s.set_active_tab(CenterViewTab::from_toolbar_index(*ix));
                cx.notify();
            });
        });

        let project_pill = if let Some(snap) = snapshot {
            let project = &snap.project;
            let project_color = parse_hex_or(&project.color, cx.theme().primary);
            h_flex()
                .gap_1p5()
                .items_center()
                .child(div().size(px(8.)).rounded(px(2.)).bg(project_color))
                .child(
                    div()
                        .text_xs()
                        .font_medium()
                        .child(format!("{} · {}", project.key_prefix, project.name)),
                )
        } else {
            h_flex()
                .gap_1p5()
                .items_center()
                .child(
                    div()
                        .size(px(8.))
                        .rounded(px(2.))
                        .bg(cx.theme().muted_foreground.opacity(0.45)),
                )
                .child(
                    div()
                        .text_xs()
                        .font_medium()
                        .text_color(cx.theme().muted_foreground)
                        .child(if has_workspace {
                            "No Projects"
                        } else {
                            "No Workspace"
                        }),
                )
        };

        h_flex()
            .h(px(44.))
            .w_full()
            .px_3()
            .gap_2()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            // Left group: Sidebar toggle + Workspace switcher + Project dropdown stub
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Button::new("toolbar-toggle-sidebar")
                            .ghost()
                            .small()
                            .selected(sidebar_open)
                            .icon(if sidebar_open {
                                IconName::PanelLeftClose
                            } else {
                                IconName::PanelLeftOpen
                            })
                            .tooltip_with_action("Toggle Sidebar", &ToggleSidebar, None)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_sidebar_inner(cx);
                            })),
                    )
                    .child(
                        Button::new("toolbar-workspace-switcher")
                            .ghost()
                            .small()
                            .icon(IconName::HardDrive)
                            .label(state.workspace_name())
                            .dropdown_caret(true)
                            .tooltip_with_action(
                                "Switch or create .protrack workspace",
                                &OpenWorkspace,
                                None,
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                open_workspace_modal(this.app_state.clone(), window, cx);
                            })),
                    )
                    .child({
                        let projects = state.projects().to_vec();
                        let active_project = snapshot.map(|s| s.project.clone());
                        let active_item_count = snapshot.map_or(0, |s| s.items.len());
                        let app_state_for_menu = self.app_state.clone();
                        Button::new("toolbar-project-switcher")
                            .outline()
                            .small()
                            .dropdown_caret(true)
                            .disabled(!has_workspace)
                            .tooltip(if has_workspace {
                                "Switch Project, create a new Project, or open Project Settings"
                            } else {
                                "Open or create a workspace first"
                            })
                            .child(project_pill)
                            .dropdown_menu(move |menu, _, cx| {
                                build_project_switcher_menu(
                                    menu,
                                    projects.clone(),
                                    active_project.clone(),
                                    active_item_count,
                                    app_state_for_menu.clone(),
                                    cx,
                                )
                            })
                    }),
            )
            // Center group: View tabs
            .child(tab_bar)
            // Right group: Disabled Cmd+K / + New stubs + Theme toggle + Inspector toggle
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Button::new("toolbar-search-stub")
                            .outline()
                            .small()
                            .icon(IconName::Search)
                            .label("Search ⌘K")
                            .disabled(true)
                            .tooltip("Quick Search (Cmd+K) — Available in Phase 11"),
                    )
                    .child(
                        Button::new("toolbar-new-stub")
                            .primary()
                            .small()
                            .icon(IconName::Plus)
                            .label("New")
                            .disabled(true)
                            .tooltip("Quick Add (Cmd+N) — Available in Phase 11"),
                    )
                    .child(
                        Button::new("toolbar-theme-toggle")
                            .ghost()
                            .small()
                            .icon(theme_icon)
                            .label(theme_pref.label())
                            .tooltip("Cycle theme: System / Light / Dark")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.cycle_theme(window, cx);
                            })),
                    )
                    .child(
                        Button::new("toolbar-toggle-inspector")
                            .ghost()
                            .small()
                            .selected(inspector_open)
                            .icon(if inspector_open {
                                IconName::PanelRightClose
                            } else {
                                IconName::PanelRightOpen
                            })
                            .tooltip_with_action("Toggle Inspector", &ToggleInspector, None)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_inspector_inner(cx);
                            })),
                    ),
            )
    }

    fn render_center_workspace(&self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = self.store.read(cx);
        let app = self.app_state.read(cx);
        let has_workspace = state.has_workspace();
        let workspace_name = state.workspace_name();
        let workspace_path = state.workspace_path().map(|p| p.display().to_string());
        let active_tab = state.active_tab();
        let selected_item_id = state.selected_item_id();
        let is_loading = app.is_loading();
        let last_error = app.last_error().map(str::to_string);

        let Some(snapshot) = state.snapshot() else {
            let empty_body = if is_loading {
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .p_8()
                    .gap_2()
                    .child(
                        Icon::new(IconName::LoaderCircle)
                            .large()
                            .text_color(cx.theme().primary),
                    )
                    .child(div().text_sm().font_semibold().child("Opening workspace…"))
                    .into_any_element()
            } else if !has_workspace {
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .p_8()
                    .gap_3()
                    .child(
                        Icon::new(IconName::HardDrive)
                            .large()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(div().text_sm().font_semibold().child("No Workspace Open"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Create a new .protrack SQLite file or open an existing workspace."),
                    )
                    .when_some(last_error, |this, err| {
                        this.child(
                            div()
                                .px_3()
                                .py_1p5()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().danger.opacity(0.12))
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child(err),
                        )
                    })
                    .child(
                        Button::new("center-open-workspace-btn")
                            .primary()
                            .small()
                            .icon(IconName::FolderOpen)
                            .label("Open or Create Workspace…")
                            .on_click({
                                let app_state = self.app_state.clone();
                                move |_, window, cx| {
                                    open_workspace_modal(app_state.clone(), window, cx);
                                }
                            }),
                    )
                    .into_any_element()
            } else {
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .p_8()
                    .gap_3()
                    .child(
                        Icon::new(IconName::FolderOpen)
                            .large()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .child(format!("{workspace_name} — No Projects Yet")),
                    )
                    .when_some(workspace_path, |this, path| {
                        this.child(
                            div()
                                .text_xs()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_color(cx.theme().muted_foreground)
                                .child(path),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Create a new Project in this workspace, or seed the starter ProTrack v1 sample project."),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("center-new-project-btn")
                                    .primary()
                                    .small()
                                    .icon(IconName::Plus)
                                    .label("New Project…")
                                    .on_click({
                                        let app_state = self.app_state.clone();
                                        move |_, window, cx| {
                                            open_new_project_modal(app_state.clone(), window, cx);
                                        }
                                    }),
                            )
                            .child(
                                Button::new("center-seed-sample-btn")
                                    .outline()
                                    .small()
                                    .icon(IconName::FolderOpen)
                                    .label("Seed Sample Project")
                                    .on_click({
                                        let app_state = self.app_state.clone();
                                        move |_, _, cx| {
                                            app_state
                                                .update(cx, |app, cx| {
                                                    app.seed_sample_project_in_active(cx)
                                                })
                                                .detach();
                                        }
                                    }),
                            )
                            .child(
                                Button::new("center-switch-workspace-btn")
                                    .outline()
                                    .small()
                                    .icon(IconName::HardDrive)
                                    .label("Switch Workspace…")
                                    .on_click({
                                        let app_state = self.app_state.clone();
                                        move |_, window, cx| {
                                            open_workspace_modal(app_state.clone(), window, cx);
                                        }
                                    }),
                            ),
                    )
                    .into_any_element()
            };

            return v_flex()
                .size_full()
                .overflow_hidden()
                .bg(cx.theme().background)
                .child(empty_body);
        };

        let item_count = match active_tab {
            CenterViewTab::Outline | CenterViewTab::Table => snapshot.active_items().len(),
            CenterViewTab::Unallocated => snapshot.unallocated_work_items().len(),
            CenterViewTab::Trash => snapshot.trash_count,
        };

        let header = h_flex()
            .h(px(40.))
            .px_4()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .child(format!("{} › {}", snapshot.project.name, active_tab.label())),
                    )
                    .child(Tag::secondary().xsmall().child(format!("{item_count} items"))),
            )
            .when(active_tab == CenterViewTab::Outline, |this| {
                this.child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("outline-expand-all")
                                .ghost()
                                .xsmall()
                                .label("Expand All")
                                .on_click({
                                    let store = self.store.clone();
                                    move |_, _, cx| {
                                        store.update(cx, |s, cx| {
                                            s.expand_all_features();
                                            cx.notify();
                                        });
                                    }
                                }),
                        )
                        .child(
                            Button::new("outline-collapse-all")
                                .ghost()
                                .xsmall()
                                .label("Collapse All")
                                .on_click({
                                    let store = self.store.clone();
                                    move |_, _, cx| {
                                        store.update(cx, |s, cx| {
                                            s.collapse_all_features();
                                            cx.notify();
                                        });
                                    }
                                }),
                        ),
                )
            });

        let body = match active_tab {
            CenterViewTab::Outline => self
                .render_outline_body(snapshot, selected_item_id, cx)
                .into_any_element(),
            CenterViewTab::Table => self
                .render_table_body(snapshot, selected_item_id, cx)
                .into_any_element(),
            CenterViewTab::Unallocated => self.unallocated_view.clone().into_any_element(),
            CenterViewTab::Trash => self.render_trash_body(cx).into_any_element(),
        };

        v_flex()
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().background)
            .child(header)
            .child(body)
    }

    fn render_outline_body(
        &self,
        snapshot: &ProjectSnapshot,
        selected_item_id: Option<ItemId>,
        cx: &App,
    ) -> impl IntoElement {
        let state = self.store.read(cx);
        let tree_rows = snapshot.visible_feature_tree_rows(state.expanded_features());

        let mut list = v_flex()
            .flex_1()
            .min_h_0()
            .p_3()
            .gap_1()
            .overflow_y_scrollbar()
            .id("center-outline-scroll");

        for row in tree_rows {
            let Some(feature) = snapshot.item(row.item_id) else {
                continue;
            };
            let attached_wis = snapshot.work_items_for_feature(row.item_id);
            let has_outline_children = row.has_children || !attached_wis.is_empty();
            let is_expanded = state.is_feature_expanded(row.item_id);
            let is_selected = selected_item_id == Some(row.item_id);
            let (done, total) = snapshot.feature_progress(row.item_id);
            let depth_badge = clamped_depth_badge(row.depth);
            let tags = snapshot.tags_for_item(feature.id);
            let assignee = feature.assignee_id.and_then(|aid| snapshot.assignee(aid));
            let feat_id = feature.id;

            list = list.child(
                h_flex()
                    .id(("outline-feature", feature.item_number as usize))
                    .w_full()
                    .pl(px(10.0 + visual_indent_px(row.depth)))
                    .pr_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(if is_selected {
                        cx.theme().primary.opacity(0.45)
                    } else {
                        cx.theme().transparent
                    })
                    .bg(if is_selected {
                        cx.theme().accent
                    } else {
                        cx.theme().transparent
                    })
                    .cursor_pointer()
                    .when(!is_selected, |this| {
                        this.hover(|s| s.bg(cx.theme().list_hover))
                    })
                    .on_click({
                        let store = self.store.clone();
                        move |_, _, cx| {
                            store.update(cx, |s, cx| {
                                s.select_item(Some(feat_id));
                                cx.notify();
                            });
                        }
                    })
                    .child(if has_outline_children {
                        let chevron = if is_expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        };
                        div()
                            .id(("outline-chevron", feature.item_number as usize))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(18.))
                            .flex_shrink_0()
                            .rounded(px(4.))
                            .hover(|s| s.bg(cx.theme().muted))
                            .on_click({
                                let store = self.store.clone();
                                move |_, _, cx| {
                                    cx.stop_propagation();
                                    store.update(cx, |s, cx| {
                                        s.toggle_feature_expanded(feat_id);
                                        cx.notify();
                                    });
                                }
                            })
                            .child(
                                Icon::new(chevron)
                                    .xsmall()
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .into_any_element()
                    } else {
                        div().w(px(18.)).flex_shrink_0().into_any_element()
                    })
                    .child(
                        Icon::new(IconName::Folder)
                            .small()
                            .text_color(cx.theme().primary),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(cx.theme().muted_foreground)
                            .child(feature.item_key.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_sm()
                            .font_semibold()
                            .child(feature.title.clone()),
                    )
                    .when_some(depth_badge, |this, badge| {
                        this.child(Tag::secondary().xsmall().child(badge))
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .children(tags.into_iter().map(|tag| domain_tag_chip(tag, cx))),
                    )
                    .child(progress_pill(done, total, cx))
                    .when_some(assignee, |this, a| {
                        this.child(assignee_pill(a, false, cx))
                    })
                    .child(status_badge(feature.status, cx)),
            );

            if is_expanded {
                for wi in attached_wis {
                    let wi_id = wi.id;
                    let wi_selected = selected_item_id == Some(wi_id);
                    let shared_count = snapshot.features_for_work_item(wi_id).len();
                    let wi_tags = snapshot.tags_for_item(wi_id);
                    let wi_assignee = wi.assignee_id.and_then(|aid| snapshot.assignee(aid));
                    let wi_indent = visual_indent_px(row.depth + 1);

                    list = list.child(
                        h_flex()
                            .id((
                                "outline-wi",
                                (feature.item_number as usize) * 10_000 + (wi.item_number as usize),
                            ))
                            .w_full()
                            .pl(px(10.0 + wi_indent))
                            .pr_3()
                            .py_1()
                            .gap_2()
                            .items_center()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(if wi_selected {
                                cx.theme().primary.opacity(0.45)
                            } else {
                                cx.theme().transparent
                            })
                            .bg(if wi_selected {
                                cx.theme().accent
                            } else {
                                cx.theme().transparent
                            })
                            .cursor_pointer()
                            .when(!wi_selected, |this| {
                                this.hover(|s| s.bg(cx.theme().list_hover))
                            })
                            .on_click({
                                let store = self.store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.select_item(Some(wi_id));
                                        cx.notify();
                                    });
                                }
                            })
                            .child(div().w(px(18.)).flex_shrink_0())
                            .child(
                                Icon::new(status_icon(wi.status))
                                    .xsmall()
                                    .text_color(status_color(wi.status, cx)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_color(cx.theme().muted_foreground)
                                    .child(wi.item_key.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_sm()
                                    .child(wi.title.clone()),
                            )
                            .when(shared_count > 1, |this| {
                                let extra = shared_count - 1;
                                let label = if extra == 1 {
                                    "Shared: +1 feature".to_string()
                                } else {
                                    format!("Shared: +{extra} features")
                                };
                                this.child(Tag::info().xsmall().outline().child(label))
                            })
                            .child(
                                h_flex()
                                    .gap_1()
                                    .children(wi_tags.into_iter().map(|tag| domain_tag_chip(tag, cx))),
                            )
                            .child(priority_badge(wi.priority, cx))
                            .when_some(wi_assignee, |this, a| {
                                this.child(assignee_pill(a, false, cx))
                            })
                            .child(status_badge(wi.status, cx)),
                    );
                }
            }
        }

        list
    }

    fn render_table_body(
        &self,
        snapshot: &ProjectSnapshot,
        selected_item_id: Option<ItemId>,
        cx: &App,
    ) -> impl IntoElement {
        let active_items = snapshot.active_items();

        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                // Header strip
                h_flex()
                    .w_full()
                    .px_4()
                    .py_2()
                    .gap_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().table_head)
                    .text_size(px(11.))
                    .font_semibold()
                    .text_color(cx.theme().table_head_foreground)
                    .child(div().w(px(64.)).child("Key"))
                    .child(div().w(px(88.)).child("Kind"))
                    .child(div().flex_1().min_w_0().child("Title"))
                    .child(div().w(px(200.)).child("Features / Parent"))
                    .child(div().w(px(108.)).child("Status"))
                    .child(div().w(px(76.)).child("Priority"))
                    .child(div().w(px(48.)).child("Who")),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .id("center-table-scroll")
                    .children(active_items.into_iter().map(|item| {
                        let item_id = item.id;
                        let is_selected = selected_item_id == Some(item_id);
                        let assignee = item.assignee_id.and_then(|aid| snapshot.assignee(aid));
                        let item_tags = snapshot.tags_for_item(item.id);
                        let context_label = match item.kind {
                            ItemKind::Feature => {
                                let path = snapshot.feature_path(item.id);
                                let ancestors: Vec<_> = path
                                    .into_iter()
                                    .filter(|f| f.id != item.id)
                                    .map(|f| f.title.clone())
                                    .collect();
                                if ancestors.is_empty() {
                                    "Root Feature".to_string()
                                } else {
                                    ancestors.join(" › ")
                                }
                            }
                            ItemKind::WorkItem => {
                                let feats = snapshot.features_for_work_item(item.id);
                                if feats.is_empty() {
                                    "Unallocated".to_string()
                                } else {
                                    feats
                                        .into_iter()
                                        .map(|f| f.title.clone())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                }
                            }
                        };
                        let is_unallocated_wi = item.kind == ItemKind::WorkItem
                            && snapshot.features_for_work_item(item.id).is_empty();

                        h_flex()
                            .id(("table-row", item.item_number as usize))
                            .w_full()
                            .px_4()
                            .py_2()
                            .gap_3()
                            .items_center()
                            .border_b_1()
                            .border_color(cx.theme().table_row_border)
                            .bg(if is_selected {
                                cx.theme().table_active
                            } else {
                                cx.theme().transparent
                            })
                            .cursor_pointer()
                            .when(!is_selected, |this| {
                                this.hover(|s| s.bg(cx.theme().table_hover))
                            })
                            .on_click({
                                let store = self.store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.select_item(Some(item_id));
                                        cx.notify();
                                    });
                                }
                            })
                            .child(
                                div()
                                    .w(px(64.))
                                    .text_xs()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_color(cx.theme().muted_foreground)
                                    .child(item.item_key.clone()),
                            )
                            .child(div().w(px(88.)).child(kind_badge(item.kind, cx)))
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1p5()
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .text_sm()
                                            .font_medium()
                                            .child(item.title.clone()),
                                    )
                                    .when(!item_tags.is_empty(), |this| {
                                        this.child(
                                            h_flex().gap_1().flex_shrink_0().children(
                                                item_tags
                                                    .into_iter()
                                                    .map(|tag| domain_tag_chip(tag, cx)),
                                            ),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .w(px(200.))
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_xs()
                                    .text_color(if is_unallocated_wi {
                                        cx.theme().warning
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(context_label),
                            )
                            .child(div().w(px(108.)).child(status_badge(item.status, cx)))
                            .child(div().w(px(76.)).child(priority_badge(item.priority, cx)))
                            .child(div().w(px(48.)).child(match assignee {
                                Some(a) => assignee_pill(a, false, cx).into_any_element(),
                                None => div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("—")
                                    .into_any_element(),
                            }))
                    })),
            )
    }

    fn render_trash_body(&self, cx: &App) -> impl IntoElement {
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .p_8()
            .gap_2()
            .child(
                Icon::new(IconName::Delete)
                    .large()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(div().text_sm().font_semibold().child("Trash is empty"))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Full-cascade Feature Trash & Restore will arrive in Phase 10."),
            )
    }
}

impl Render for WorkspaceShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_pref = self.store.read(cx).theme_preference();
        if theme_pref != self.last_applied_theme {
            self.last_applied_theme = theme_pref;
            theme_pref.apply(Some(window), cx);
        }

        let (needs_modal, is_loading) = {
            let app = self.app_state.read(cx);
            (app.needs_welcome_modal(), app.is_loading())
        };
        if needs_modal && !is_loading && !window.has_active_dialog(cx) {
            self.app_state.update(cx, |app, _| app.clear_welcome_flag());
            let app_state = self.app_state.clone();
            window.defer(cx, move |window, cx| {
                open_workspace_modal(app_state, window, cx);
            });
        }

        let sidebar_open = self.store.read(cx).sidebar_open();
        let inspector_open = self.store.read(cx).inspector_open();
        let shell_weak = cx.weak_entity();

        let status_label = if is_loading {
            "Opening workspace…".to_string()
        } else if let Some(meta) = self.store.read(cx).workspace_meta() {
            format!("SQLite · Schema v{}", meta.schema_version)
        } else {
            "No Workspace Open".to_string()
        };

        let panes = h_resizable("workspace-panes")
            .with_state(&self.resizable_state)
            .on_resize(move |state, _, cx| {
                let sizes = state.read(cx).sizes().clone();
                let _ = shell_weak.update(cx, |this, cx| {
                    let s_open = this.store.read(cx).sidebar_open();
                    let i_open = this.store.read(cx).inspector_open();
                    let expected = 1 + usize::from(s_open) + usize::from(i_open);
                    if sizes.len() == expected {
                        if s_open && sizes[0] >= px(200.) {
                            this.sidebar_width = sizes[0];
                        }
                        if i_open {
                            let last = sizes[sizes.len() - 1];
                            if last >= px(300.) {
                                this.inspector_width = last;
                            }
                        }
                        let sw = f32::from(this.sidebar_width);
                        let iw = f32::from(this.inspector_width);
                        this.app_state.update(cx, |app, _| {
                            app.set_panel_widths(sw, iw);
                        });
                    }
                });
            })
            .when(sidebar_open, |group| {
                group.child(
                    resizable_panel()
                        .size(self.sidebar_width)
                        .size_range(px(200.)..px(400.))
                        .flex_none()
                        .child(render_sidebar(&self.app_state, &self.store, window, cx)),
                )
            })
            .child(
                resizable_panel()
                    .size_range(px(320.)..Pixels::MAX)
                    .child(self.render_center_workspace(window, cx)),
            )
            .when(inspector_open, |group| {
                group.child(
                    resizable_panel()
                        .size(self.inspector_width)
                        .size_range(px(300.)..px(540.))
                        .flex_none()
                        .child(self.inspector.clone()),
                )
            });

        v_flex()
            .id("workspace-shell")
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::toggle_inspector_action))
            .on_action(cx.listener(Self::open_workspace_action))
            .child(
                TitleBar::new().child(
                    h_flex()
                        .w_full()
                        .pr_3()
                        .items_center()
                        .justify_between()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    Icon::new(IconName::FolderOpen)
                                        .xsmall()
                                        .text_color(cx.theme().primary),
                                )
                                .child(div().text_xs().font_semibold().child("ProTrack")),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(cx.theme().muted_foreground)
                                .child(status_label),
                        ),
                ),
            )
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().w_full().child(panes))
    }
}
