use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex,
    v_flex,
};
use gpui_kit::{
    App, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use crate::domain::{clamped_depth_badge, visual_indent_px};
use crate::state::{CenterViewTab, ProjectStore};
use crate::ui::{parse_hex_or, progress_pill, status_color, status_icon};

pub fn render_sidebar(
    store: &Entity<ProjectStore>,
    _window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let state = store.read(cx);
    let snapshot = state.snapshot();
    let has_workspace = state.has_workspace();
    let active_tab = state.active_tab();
    let selected_item_id = state.selected_item_id();
    let unallocated_count = snapshot.map_or(0, |s| s.unallocated_work_items().len());
    let trash_count = snapshot.map_or(0, |s| s.trash_count);
    let assignee_count = state.assignees().len();
    let tag_count = snapshot.map_or(0, |s| s.tags.len());
    let tree_rows = snapshot
        .map(|s| s.visible_feature_tree_rows(state.expanded_features()))
        .unwrap_or_default();
    let any_expanded = !state.expanded_features().is_empty();

    let amber = cx.theme().warning;
    let amber_bg = amber.opacity(if cx.theme().is_dark() { 0.22 } else { 0.14 });
    let amber_border = amber.opacity(if cx.theme().is_dark() { 0.42 } else { 0.30 });

    let project_header = if let Some(snap) = snapshot {
        let project = &snap.project;
        let project_color = parse_hex_or(&project.color, cx.theme().primary);
        h_flex()
            .px_3()
            .py_2p5()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                div()
                    .size(px(10.))
                    .flex_shrink_0()
                    .rounded(px(3.))
                    .bg(project_color),
            )
            .child(
                div()
                    .px_1p5()
                    .py_0p5()
                    .rounded(px(4.))
                    .bg(cx.theme().muted)
                    .text_size(px(10.))
                    .font_semibold()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(cx.theme().muted_foreground)
                    .child(project.key_prefix.clone()),
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
                    .child(project.name.clone()),
            )
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(cx.theme().muted_foreground)
                    .child(project.status.label()),
            )
    } else {
        h_flex()
            .px_3()
            .py_2p5()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                div()
                    .size(px(10.))
                    .flex_shrink_0()
                    .rounded(px(3.))
                    .bg(cx.theme().muted_foreground.opacity(0.4)),
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
                    .text_color(cx.theme().muted_foreground)
                    .child(if has_workspace {
                        "No Active Project"
                    } else {
                        "No Workspace"
                    }),
            )
    };

    v_flex()
        .size_full()
        .overflow_hidden()
        .bg(cx.theme().sidebar)
        .text_color(cx.theme().sidebar_foreground)
        // Project header
        .child(project_header)
        // Pinned Unallocated row
        .child(
            div().px_2().pt_2().pb_1().child(
                h_flex()
                    .id("sidebar-unallocated-row")
                    .w_full()
                    .px_2()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .cursor_pointer()
                    .when(active_tab == CenterViewTab::Unallocated, |this| {
                        this.bg(cx.theme().sidebar_accent)
                            .text_color(cx.theme().sidebar_accent_foreground)
                    })
                    .when(active_tab != CenterViewTab::Unallocated, |this| {
                        this.hover(|s| s.bg(cx.theme().sidebar_accent.opacity(0.6)))
                    })
                    .on_click({
                        let store = store.clone();
                        move |_, _, cx| {
                            store.update(cx, |s, cx| {
                                s.set_active_tab(CenterViewTab::Unallocated);
                                cx.notify();
                            });
                        }
                    })
                    .child(
                        Icon::new(IconName::Inbox)
                            .small()
                            .text_color(if unallocated_count > 0 {
                                amber
                            } else {
                                cx.theme().muted_foreground
                            }),
                    )
                    .child(div().flex_1().text_xs().font_medium().child("Unallocated"))
                    .child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded(px(999.))
                            .border_1()
                            .text_size(px(10.))
                            .font_semibold()
                            .when(unallocated_count > 0, |this| {
                                this.bg(amber_bg).border_color(amber_border).text_color(amber)
                            })
                            .when(unallocated_count == 0, |this| {
                                this.bg(cx.theme().muted)
                                    .border_color(cx.theme().border)
                                    .text_color(cx.theme().muted_foreground)
                            })
                            .child(unallocated_count.to_string()),
                    ),
            ),
        )
        // FEATURES header
        .child(
            h_flex()
                .px_3()
                .pt_2()
                .pb_1()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(10.))
                        .font_semibold()
                        .text_color(cx.theme().muted_foreground)
                        .child("FEATURES"),
                )
                .child(
                    Button::new("sidebar-toggle-expand-all")
                        .ghost()
                        .xsmall()
                        .compact()
                        .disabled(snapshot.is_none())
                        .icon(IconName::ChevronsUpDown)
                        .tooltip(if any_expanded {
                            "Collapse all Features"
                        } else {
                            "Expand all Features"
                        })
                        .on_click({
                            let store = store.clone();
                            move |_, _, cx| {
                                store.update(cx, |s, cx| {
                                    if s.expanded_features().is_empty() {
                                        s.expand_all_features();
                                    } else {
                                        s.collapse_all_features();
                                    }
                                    cx.notify();
                                });
                            }
                        }),
                ),
        )
        // Nested Feature tree
        .child(if let Some(snap) = snapshot {
            if tree_rows.is_empty() {
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .px_3()
                    .py_4()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("No features in this project yet."),
                    )
                    .into_any_element()
            } else {
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .px_1p5()
                    .pb_2()
                    .gap_0p5()
                    .overflow_y_scrollbar()
                    .id("sidebar-feature-tree")
                    .children(tree_rows.into_iter().filter_map(|row| {
                        let item = snap.item(row.item_id)?;
                        let is_selected = selected_item_id == Some(row.item_id);
                        let (done, total) = snap.feature_progress(row.item_id);
                        let depth_badge = clamped_depth_badge(row.depth);
                        let indent = visual_indent_px(row.depth);
                        let item_id = row.item_id;
                        let s_color = status_color(item.status, cx);

                        Some(
                            h_flex()
                                .id(("sidebar-feature", item.item_number as usize))
                                .w_full()
                                .pl(px(6.0 + indent))
                                .pr_2()
                                .py_1()
                                .gap_1()
                                .items_center()
                                .rounded(cx.theme().radius)
                                .cursor_pointer()
                                .when(is_selected, |this| {
                                    this.bg(cx.theme().sidebar_accent)
                                        .text_color(cx.theme().sidebar_accent_foreground)
                                })
                                .when(!is_selected, |this| {
                                    this.hover(|s| s.bg(cx.theme().sidebar_accent.opacity(0.6)))
                                })
                                .on_click({
                                    let store = store.clone();
                                    move |_, _, cx| {
                                        store.update(cx, |s, cx| {
                                            s.select_item(Some(item_id));
                                            if s.active_tab() == CenterViewTab::Trash {
                                                s.set_active_tab(CenterViewTab::Outline);
                                            }
                                            cx.notify();
                                        });
                                    }
                                })
                                .child(if row.has_children {
                                    let chevron = if row.is_expanded {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    };
                                    div()
                                        .id(("sidebar-chevron", item.item_number as usize))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .size(px(16.))
                                        .flex_shrink_0()
                                        .rounded(px(4.))
                                        .hover(|s| s.bg(cx.theme().muted))
                                        .on_click({
                                            let store = store.clone();
                                            move |_, _, cx| {
                                                cx.stop_propagation();
                                                store.update(cx, |s, cx| {
                                                    s.toggle_feature_expanded(item_id);
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
                                    div().w(px(16.)).flex_shrink_0().into_any_element()
                                })
                                .child(
                                    Icon::new(status_icon(item.status))
                                        .xsmall()
                                        .text_color(s_color),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_xs()
                                        .font_medium()
                                        .child(item.title.clone()),
                                )
                                .when_some(depth_badge, |this, badge| {
                                    this.child(
                                        div()
                                            .px_1()
                                            .rounded(px(3.))
                                            .bg(cx.theme().muted)
                                            .text_size(px(9.))
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_color(cx.theme().muted_foreground)
                                            .child(badge),
                                    )
                                })
                                .child(progress_pill(done, total, cx)),
                        )
                    }))
                    .into_any_element()
            }
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .px_3()
                .py_4()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(if has_workspace {
                            "No projects in this workspace yet."
                        } else {
                            "Open or create a workspace to browse Features."
                        }),
                )
                .into_any_element()
        })
        // Bottom navigation placeholders
        .child(
            v_flex()
                .p_2()
                .gap_1()
                .border_t_1()
                .border_color(cx.theme().sidebar_border)
                .child(
                    h_flex()
                        .id("sidebar-trash-nav")
                        .w_full()
                        .px_2()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .rounded(cx.theme().radius)
                        .cursor_pointer()
                        .when(active_tab == CenterViewTab::Trash, |this| {
                            this.bg(cx.theme().sidebar_accent)
                                .text_color(cx.theme().sidebar_accent_foreground)
                        })
                        .when(active_tab != CenterViewTab::Trash, |this| {
                            this.hover(|s| s.bg(cx.theme().sidebar_accent.opacity(0.6)))
                        })
                        .on_click({
                            let store = store.clone();
                            move |_, _, cx| {
                                store.update(cx, |s, cx| {
                                    s.set_active_tab(CenterViewTab::Trash);
                                    cx.notify();
                                });
                            }
                        })
                        .child(
                            Icon::new(IconName::Delete)
                                .xsmall()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(div().flex_1().text_xs().child("Trash"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("({trash_count})")),
                        ),
                )
                .child(
                    Button::new("sidebar-assignees")
                        .ghost()
                        .xsmall()
                        .w_full()
                        .disabled(true)
                        .tooltip("Global Assignees Manager — Available in Phase 5")
                        .child(
                            h_flex()
                                .flex_1()
                                .justify_between()
                                .items_center()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(Icon::new(IconName::User).xsmall())
                                        .child("Assignees"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(assignee_count.to_string()),
                                ),
                        ),
                )
                .child(
                    Button::new("sidebar-tags")
                        .ghost()
                        .xsmall()
                        .w_full()
                        .disabled(true)
                        .tooltip("Project Tags Manager — Available in Phase 5")
                        .child(
                            h_flex()
                                .flex_1()
                                .justify_between()
                                .items_center()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(Icon::new(IconName::Asterisk).xsmall())
                                        .child("Tags"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(tag_count.to_string()),
                                ),
                        ),
                ),
        )
}
