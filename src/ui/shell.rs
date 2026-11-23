use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    StyledExt as _, Theme, TitleBar, h_flex, v_flex,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Render, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, div, prelude::FluentBuilder as _, px,
};

use crate::domain::{ItemKind, clamped_depth_badge, visual_indent_px};
use crate::state::{CenterViewTab, ProjectStore, ThemePreference};
use crate::ui::inspector::render_inspector;
use crate::ui::sidebar::render_sidebar;
use crate::ui::{
    ToggleInspector, ToggleSidebar, assignee_pill, domain_tag_chip, kind_badge, parse_hex_or,
    priority_badge, progress_pill, status_badge, status_color, status_icon,
};

const DEFAULT_SIDEBAR_WIDTH: Pixels = px(260.);
const DEFAULT_INSPECTOR_WIDTH: Pixels = px(380.);

pub struct WorkspaceShell {
    store: Entity<ProjectStore>,
    focus_handle: FocusHandle,
    resizable_state: Entity<ResizableState>,
    sidebar_width: Pixels,
    inspector_width: Pixels,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = cx.new(|_| ProjectStore::new_with_seed());
        let initial_pref = store.read(cx).theme_preference();
        initial_pref.apply(Some(window), cx);

        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        let store_sub = cx.observe(&store, |_, _, cx| cx.notify());
        let appearance_sub = cx.observe_window_appearance(window, |this, window, cx| {
            if this.store.read(cx).theme_preference() == ThemePreference::System {
                Theme::sync_system_appearance(Some(window), cx);
                cx.notify();
            }
        });

        let resizable_state = cx.new(|_| ResizableState::default());

        Self {
            store,
            focus_handle,
            resizable_state,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            inspector_width: DEFAULT_INSPECTOR_WIDTH,
            _subscriptions: vec![store_sub, appearance_sub],
        }
    }

    pub fn store(&self) -> &Entity<ProjectStore> {
        &self.store
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
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

    fn toggle_sidebar_inner(&mut self, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.toggle_sidebar();
            cx.notify();
        });
        // Reset pane layout state when panel count changes so sizes re-seed cleanly.
        self.resizable_state = cx.new(|_| ResizableState::default());
        cx.notify();
    }

    fn toggle_inspector_inner(&mut self, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.toggle_inspector();
            cx.notify();
        });
        self.resizable_state = cx.new(|_| ResizableState::default());
        cx.notify();
    }

    fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.store.update(cx, |store, cx| {
            let pref = store.cycle_theme_preference();
            cx.notify();
            pref
        });
        next.apply(Some(window), cx);
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.store.read(cx);
        let snapshot = state.snapshot();
        let active_tab = state.active_tab();
        let sidebar_open = state.sidebar_open();
        let inspector_open = state.inspector_open();
        let theme_pref = state.theme_preference();
        let unallocated_count = snapshot.unallocated_work_items().len();
        let project = &snapshot.project;
        let project_color = parse_hex_or(&project.color, cx.theme().primary);

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
            // Left group: Sidebar toggle + Workspace pill + Project dropdown stub
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
                        Button::new("toolbar-workspace-stub")
                            .ghost()
                            .small()
                            .icon(IconName::HardDrive)
                            .label(state.workspace_name().to_string())
                            .dropdown_caret(true)
                            .disabled(true)
                            .tooltip("Workspace switcher & .protrack files — Available in Phase 2"),
                    )
                    .child(
                        Button::new("toolbar-project-stub")
                            .outline()
                            .small()
                            .dropdown_caret(true)
                            .disabled(true)
                            .tooltip("Project switcher & settings — Available in Phase 3")
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .items_center()
                                    .child(
                                        div().size(px(8.)).rounded(px(2.)).bg(project_color),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_medium()
                                            .child(format!("{} · {}", project.key_prefix, project.name)),
                                    ),
                            ),
                    ),
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
        let snapshot = state.snapshot();
        let active_tab = state.active_tab();
        let selected_item_id = state.selected_item_id();

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
            CenterViewTab::Outline => self.render_outline_body(selected_item_id, cx).into_any_element(),
            CenterViewTab::Table => self.render_table_body(selected_item_id, cx).into_any_element(),
            CenterViewTab::Unallocated => {
                self.render_unallocated_body(selected_item_id, cx).into_any_element()
            }
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
        selected_item_id: Option<crate::domain::ItemId>,
        cx: &App,
    ) -> impl IntoElement {
        let state = self.store.read(cx);
        let snapshot = state.snapshot();
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
        selected_item_id: Option<crate::domain::ItemId>,
        cx: &App,
    ) -> impl IntoElement {
        let snapshot = self.store.read(cx).snapshot();
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

    fn render_unallocated_body(
        &self,
        selected_item_id: Option<crate::domain::ItemId>,
        cx: &App,
    ) -> impl IntoElement {
        let snapshot = self.store.read(cx).snapshot();
        let unallocated = snapshot.unallocated_work_items();

        v_flex()
            .flex_1()
            .min_h_0()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Things we need to do but I haven't figured out when yet"),
                    )
                    .child(Tag::warning().xsmall().outline().child(format!(
                        "{} unallocated",
                        unallocated.len()
                    ))),
            )
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted.opacity(0.4))
                    .child(
                        Icon::new(IconName::Plus)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Quick-capture unallocated Work Items — Available in Phase 4"),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_1p5()
                    .overflow_y_scrollbar()
                    .id("center-unallocated-scroll")
                    .children(unallocated.into_iter().map(|wi| {
                        let wi_id = wi.id;
                        let is_selected = selected_item_id == Some(wi_id);
                        let assignee = wi.assignee_id.and_then(|aid| snapshot.assignee(aid));
                        let tags = snapshot.tags_for_item(wi_id);

                        h_flex()
                            .id(("unallocated-row", wi.item_number as usize))
                            .w_full()
                            .px_3()
                            .py_2()
                            .gap_2p5()
                            .items_center()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(if is_selected {
                                cx.theme().primary.opacity(0.45)
                            } else {
                                cx.theme().border
                            })
                            .bg(if is_selected {
                                cx.theme().accent
                            } else {
                                cx.theme().popover
                            })
                            .cursor_pointer()
                            .when(!is_selected, |this| {
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
                            .child(
                                Icon::new(status_icon(wi.status))
                                    .small()
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
                                    .font_medium()
                                    .child(wi.title.clone()),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .children(tags.into_iter().map(|tag| domain_tag_chip(tag, cx))),
                            )
                            .child(priority_badge(wi.priority, cx))
                            .when_some(assignee, |this, a| {
                                this.child(assignee_pill(a, false, cx))
                            })
                            .child(
                                Button::new(("unallocated-assign-stub", wi.item_number as usize))
                                    .outline()
                                    .xsmall()
                                    .label("+ Assign Features")
                                    .disabled(true)
                                    .tooltip("Feature attachment popover — Available in Phase 4"),
                            )
                            .child(
                                Button::new(("unallocated-convert-stub", wi.item_number as usize))
                                    .ghost()
                                    .xsmall()
                                    .label("→ Feature")
                                    .disabled(true)
                                    .tooltip("Convert to Feature — Available in Phase 8"),
                            )
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
        let sidebar_open = self.store.read(cx).sidebar_open();
        let inspector_open = self.store.read(cx).inspector_open();
        let shell_weak = cx.weak_entity();

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
                    }
                });
            })
            .when(sidebar_open, |group| {
                group.child(
                    resizable_panel()
                        .size(self.sidebar_width)
                        .size_range(px(200.)..px(400.))
                        .flex_none()
                        .child(render_sidebar(&self.store, window, cx)),
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
                        .child(render_inspector(&self.store, window, cx)),
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
                                .child("Phase 1 · In-Memory Preview"),
                        ),
                ),
            )
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().w_full().child(panes))
    }
}
