use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex,
    v_flex,
};
use gpui_kit::{
    AnyElement, App, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::commands::ReorderSiblingFeatures;
use crate::domain::{
    EffectiveBlockState, ItemId, MAX_FEATURE_DEPTH, clamped_depth_badge, visual_indent_px,
};
use crate::state::{AppState, CenterViewTab, ProjectStore};
use crate::ui::dnd::{
    DragPayload, DropEvaluation, dispatch_dnd_command, evaluate_feature_drop, evaluate_root_drop,
    evaluate_unallocated_drop, handle_feature_drop, handle_root_drop, handle_unallocated_drop,
    is_valid_feature_sibling_gap, make_drag_ghost, render_drop_gap,
};
use crate::ui::modals::{
    open_assignees_manager_modal, open_create_feature_modal, open_project_settings_modal,
    open_tags_manager_modal,
};
use crate::ui::{
    build_project_switcher_menu, parse_hex_or, progress_pill, status_color, status_icon,
};

pub fn render_sidebar(
    app_state: &Entity<AppState>,
    store: &Entity<ProjectStore>,
    _window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let state = store.read(cx);
    let snapshot = state.snapshot();
    let has_workspace = state.has_workspace();
    let projects = state.projects().to_vec();
    let active_project = snapshot.map(|s| s.project.clone());
    let active_item_count = snapshot.map_or(0, |s| s.items.len());
    let active_tab = state.active_tab();
    let selected_item_id = state.selected_item_id();
    let unallocated_count = snapshot.map_or(0, |s| s.unallocated_work_items().len());
    let trash_count = snapshot.map_or(0, |s| s.trash_count);
    let assignee_count = state
        .assignees()
        .iter()
        .filter(|a| !a.is_archived())
        .count();
    let tag_count = snapshot.map_or(0, |s| s.tags.len());
    let tree_rows = snapshot
        .map(|s| s.visible_feature_tree_rows(state.expanded_features()))
        .unwrap_or_default();
    let any_expanded = !state.expanded_features().is_empty();

    let amber = cx.theme().warning;
    let amber_bg = amber.opacity(if cx.theme().is_dark() { 0.22 } else { 0.14 });
    let amber_border = amber.opacity(if cx.theme().is_dark() { 0.42 } else { 0.30 });

    let switcher_label = if let Some(project) = &active_project {
        let project_color = parse_hex_or(&project.color, cx.theme().primary);
        h_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .items_center()
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
            .into_any_element()
    } else {
        h_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .items_center()
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
            .into_any_element()
    };

    let project_header = h_flex()
        .px_2()
        .py_1p5()
        .gap_1()
        .items_center()
        .border_b_1()
        .border_color(cx.theme().sidebar_border)
        .child(
            div().flex_1().min_w_0().child(
                Button::new("sidebar-project-switcher")
                    .ghost()
                    .small()
                    .w_full()
                    .dropdown_caret(true)
                    .disabled(!has_workspace)
                    .tooltip(if has_workspace {
                        "Switch Project or create a new Project"
                    } else {
                        "Open or create a workspace first"
                    })
                    .child(switcher_label)
                    .dropdown_menu({
                        let projects = projects.clone();
                        let active_project = active_project.clone();
                        let app_state = app_state.clone();
                        move |menu, _, cx| {
                            build_project_switcher_menu(
                                menu,
                                projects.clone(),
                                active_project.clone(),
                                active_item_count,
                                app_state.clone(),
                                cx,
                            )
                        }
                    }),
            ),
        )
        .child(
            Button::new("sidebar-project-settings")
                .ghost()
                .xsmall()
                .compact()
                .accessibility_label("Project Settings")
                .icon(IconName::Settings)
                .disabled(active_project.is_none())
                .tooltip("Project Settings…")
                .on_click({
                    let app_state = app_state.clone();
                    let active_project = active_project.clone();
                    move |_, window, cx| {
                        if let Some(project) = active_project.clone() {
                            open_project_settings_modal(
                                app_state.clone(),
                                project,
                                active_item_count,
                                window,
                                cx,
                            );
                        }
                    }
                }),
        );

    let active_project_id = active_project.as_ref().map(|p| p.id);

    v_flex()
        .size_full()
        .overflow_hidden()
        .bg(cx.theme().sidebar)
        .text_color(cx.theme().sidebar_foreground)
        // Project header / switcher
        .child(project_header)
        // Pinned Unallocated row
        .child(
            div().px_2().pt_2().pb_1().child({
                let snap_for_unalloc_over = snapshot.cloned();
                let app_for_unalloc_drop = app_state.clone();
                let store_for_unalloc_drop = store.clone();
                h_flex()
                    .id("sidebar-unallocated-row")
                    .test_support()
                    .w_full()
                    .px_2()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().transparent)
                    .cursor_pointer()
                    .when(active_tab == CenterViewTab::Unallocated, |this| {
                        this.bg(cx.theme().sidebar_accent)
                            .text_color(cx.theme().sidebar_accent_foreground)
                    })
                    .when(active_tab != CenterViewTab::Unallocated, |this| {
                        this.hover(|s| s.bg(cx.theme().sidebar_accent.opacity(0.6)))
                    })
                    .drag_over::<DragPayload>(move |style, payload, _, cx| {
                        if let Some(snap) = snap_for_unalloc_over.as_ref()
                            && evaluate_unallocated_drop(snap, payload) == DropEvaluation::Valid
                        {
                            let warn = cx.theme().warning;
                            style
                                .bg(warn.opacity(if cx.theme().is_dark() { 0.22 } else { 0.14 }))
                                .border_color(warn)
                        } else {
                            style
                        }
                    })
                    .on_drop::<DragPayload>(move |payload, window, cx| {
                        handle_unallocated_drop(
                            &app_for_unalloc_drop,
                            &store_for_unalloc_drop,
                            payload,
                            window,
                            cx,
                        );
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
                            .id("sidebar-unallocated-badge")
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
                    )
            }),
        )
        // FEATURES header (also acts as Root Drop Zone to promote nested Features to root)
        .child({
            let snap_for_root_over = snapshot.cloned();
            let app_for_root_drop = app_state.clone();
            let store_for_root_drop = store.clone();
            h_flex()
                .id("sidebar-root-drop-zone")
                .test_support()
                .mx_1p5()
                .px_1p5()
                .pt_1p5()
                .pb_1()
                .items_center()
                .justify_between()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().transparent)
                .drag_over::<DragPayload>(move |style, payload, _, cx| {
                    if let Some(snap) = snap_for_root_over.as_ref()
                        && evaluate_root_drop(snap, payload) == DropEvaluation::Valid
                    {
                        let primary = cx.theme().primary;
                        style
                            .bg(primary.opacity(if cx.theme().is_dark() { 0.22 } else { 0.14 }))
                            .border_color(primary)
                    } else {
                        style
                    }
                })
                .on_drop::<DragPayload>(move |payload, window, cx| {
                    handle_root_drop(&app_for_root_drop, &store_for_root_drop, payload, window, cx);
                })
                .child(
                    div()
                        .text_size(px(10.))
                        .font_semibold()
                        .text_color(cx.theme().muted_foreground)
                        .child("FEATURES"),
                )
                .child(
                    h_flex()
                        .gap_0p5()
                        .items_center()
                        .child(
                            Button::new("sidebar-add-root-feature")
                                .ghost()
                                .xsmall()
                                .compact()
                                .accessibility_label("New Root Feature")
                                .disabled(active_project_id.is_none())
                                .icon(IconName::Plus)
                                .tooltip("New Root Feature")
                                .on_click({
                                    let app_state = app_state.clone();
                                    move |_, window, cx| {
                                        if let Some(pid) = active_project_id {
                                            open_create_feature_modal(
                                                app_state.clone(),
                                                pid,
                                                None,
                                                window,
                                                cx,
                                            );
                                        }
                                    }
                                }),
                        )
                        .child(
                            Button::new("sidebar-toggle-expand-all")
                                .ghost()
                                .xsmall()
                                .compact()
                                .accessibility_label(if any_expanded {
                                    "Collapse all Features"
                                } else {
                                    "Expand all Features"
                                })
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
        })
        // Nested Feature tree
        .child(if let Some(snap) = snapshot {
            if tree_rows.is_empty() {
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .px_3()
                    .py_4()
                    .gap_2()
                    .items_start()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("No features in this project yet."),
                    )
                    .child(
                        Button::new("sidebar-empty-add-feature")
                            .outline()
                            .xsmall()
                            .icon(IconName::Plus)
                            .label("New Feature")
                            .on_click({
                                let app_state = app_state.clone();
                                let pid = snap.project.id;
                                move |_, window, cx| {
                                    open_create_feature_modal(
                                        app_state.clone(),
                                        pid,
                                        None,
                                        window,
                                        cx,
                                    );
                                }
                            }),
                    )
                    .into_any_element()
            } else {
                let mut tree_elements: Vec<AnyElement> = Vec::with_capacity(tree_rows.len() * 2);
                let mut open_last_by_depth: HashMap<usize, (ItemId, Option<ItemId>, usize)> =
                    HashMap::new();
                let mut prev_by_depth: HashMap<usize, (Option<ItemId>, ItemId)> = HashMap::new();

                for (idx, row) in tree_rows.iter().enumerate() {
                    let Some(item) = snap.item(row.item_id) else {
                        continue;
                    };
                    let is_selected = selected_item_id == Some(row.item_id);
                    let (done, total) = snap.feature_progress(row.item_id);
                    let depth_badge = clamped_depth_badge(row.depth);
                    let indent = visual_indent_px(row.depth);
                    let item_id = row.item_id;
                    let item_num = item.item_number as usize;
                    let project_id = item.project_id;
                    let parent_feature_id = item.parent_feature_id;
                    let parent_label = format!("{} {}", item.item_key, item.title);
                    let at_max_depth = row.depth + 1 >= MAX_FEATURE_DEPTH;
                    let s_color = status_color(item.status, cx);
                    let block_state = snap.effective_block_state(row.item_id);
                    let block_dot_color = match block_state {
                        EffectiveBlockState::DirectlyBlocked { .. } => Some(cx.theme().danger),
                        EffectiveBlockState::InheritedBlocked { .. } => Some(cx.theme().warning),
                        EffectiveBlockState::Unblocked
                        | EffectiveBlockState::PartiallyBlocked { .. } => None,
                    };

                    let prev_sib_id = prev_by_depth
                        .get(&row.depth)
                        .filter(|(p, _)| *p == parent_feature_id)
                        .map(|(_, prev_id)| *prev_id);

                    let app_gap_before = app_state.clone();
                    tree_elements.push(render_drop_gap(
                        ("sidebar-feature-gap-before", item_num),
                        6.0 + indent,
                        move |payload| {
                            is_valid_feature_sibling_gap(
                                payload,
                                project_id,
                                parent_feature_id,
                                prev_sib_id,
                                Some(item_id),
                            )
                        },
                        move |payload, window, cx| {
                            if let DragPayload::Feature { feature_id, .. } = payload {
                                dispatch_dnd_command(
                                    app_gap_before.clone(),
                                    Box::new(ReorderSiblingFeatures::new(
                                        project_id,
                                        *feature_id,
                                        prev_sib_id,
                                        Some(item_id),
                                    )),
                                    window,
                                    cx,
                                );
                            }
                        },
                    ));

                    prev_by_depth.insert(row.depth, (parent_feature_id, item_id));
                    open_last_by_depth.insert(row.depth, (item_id, parent_feature_id, item_num));

                    let drag_payload = DragPayload::Feature {
                        project_id,
                        feature_id: item_id,
                        parent_feature_id,
                        item_key: item.item_key.clone(),
                        title: item.title.clone(),
                    };
                    let snap_for_over = snap.clone();
                    let app_for_drop = app_state.clone();
                    let store_for_drop = store.clone();

                    tree_elements.push(
                        h_flex()
                            .id(("sidebar-feature", item_num))
                            .test_support()
                            .w_full()
                            .pl(px(6.0 + indent))
                            .pr_1p5()
                            .py_1()
                            .gap_1()
                            .items_center()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().transparent)
                            .cursor_pointer()
                            .when(is_selected, |this| {
                                this.bg(cx.theme().sidebar_accent)
                                    .text_color(cx.theme().sidebar_accent_foreground)
                            })
                            .when(!is_selected, |this| {
                                this.hover(|s| s.bg(cx.theme().sidebar_accent.opacity(0.6)))
                            })
                            .on_drag(drag_payload, make_drag_ghost)
                            .drag_over::<DragPayload>(move |style, payload, window, cx| {
                                let alt_held = window.modifiers().alt;
                                match evaluate_feature_drop(&snap_for_over, payload, item_id, alt_held) {
                                    DropEvaluation::Valid => {
                                        let accent = if alt_held
                                            && matches!(
                                                payload,
                                                DragPayload::WorkItem {
                                                    source_feature_id: Some(_),
                                                    ..
                                                }
                                            ) {
                                            cx.theme().info
                                        } else {
                                            cx.theme().primary
                                        };
                                        style
                                            .bg(accent.opacity(if cx.theme().is_dark() {
                                                0.22
                                            } else {
                                                0.14
                                            }))
                                            .border_color(accent)
                                    }
                                    DropEvaluation::Invalid(_) => {
                                        let danger = cx.theme().danger;
                                        style
                                            .bg(danger.opacity(if cx.theme().is_dark() {
                                                0.20
                                            } else {
                                                0.12
                                            }))
                                            .border_color(danger)
                                    }
                                    DropEvaluation::NoOp => style,
                                }
                            })
                            .on_drop::<DragPayload>(move |payload, window, cx| {
                                let alt_held = window.modifiers().alt;
                                handle_feature_drop(
                                    &app_for_drop,
                                    &store_for_drop,
                                    payload,
                                    item_id,
                                    alt_held,
                                    window,
                                    cx,
                                );
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
                                    .id(("sidebar-chevron", item_num))
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
                            .when_some(block_dot_color, |this, dot_color| {
                                this.child(
                                    div()
                                        .id(("sidebar-block-dot", item_num))
                                        .test_support()
                                        .size(px(7.))
                                        .flex_shrink_0()
                                        .rounded(px(999.))
                                        .bg(dot_color),
                                )
                            })
                            .child(progress_pill(done, total, cx))
                            .child(
                                Button::new(("sidebar-add-subfeature", item_num))
                                    .ghost()
                                    .xsmall()
                                    .compact()
                                    .accessibility_label("New Sub-Feature")
                                    .disabled(at_max_depth)
                                    .icon(IconName::Plus)
                                    .tooltip(if at_max_depth {
                                        "Maximum Feature nesting depth (32) reached"
                                    } else {
                                        "New Sub-Feature"
                                    })
                                    .on_click({
                                        let app_state = app_state.clone();
                                        let parent_label = parent_label.clone();
                                        let depth = row.depth;
                                        move |_, window, cx| {
                                            cx.stop_propagation();
                                            open_create_feature_modal(
                                                app_state.clone(),
                                                project_id,
                                                Some((item_id, parent_label.clone(), depth)),
                                                window,
                                                cx,
                                            );
                                        }
                                    }),
                            )
                            .into_any_element(),
                    );

                    let close_from_depth = if idx + 1 == tree_rows.len() {
                        Some(0usize)
                    } else {
                        let next_depth = tree_rows[idx + 1].depth;
                        if next_depth < row.depth {
                            Some(next_depth + 1)
                        } else {
                            None
                        }
                    };

                    if let Some(min_close_depth) = close_from_depth {
                        for d in (min_close_depth..=row.depth).rev() {
                            if let Some((last_id, last_parent, last_num)) =
                                open_last_by_depth.remove(&d)
                            {
                                prev_by_depth.remove(&d);
                                let app_gap_after = app_state.clone();
                                tree_elements.push(render_drop_gap(
                                    ("sidebar-feature-gap-after", last_num),
                                    6.0 + visual_indent_px(d),
                                    move |payload| {
                                        is_valid_feature_sibling_gap(
                                            payload,
                                            project_id,
                                            last_parent,
                                            Some(last_id),
                                            None,
                                        )
                                    },
                                    move |payload, window, cx| {
                                        if let DragPayload::Feature { feature_id, .. } = payload {
                                            dispatch_dnd_command(
                                                app_gap_after.clone(),
                                                Box::new(ReorderSiblingFeatures::new(
                                                    project_id,
                                                    *feature_id,
                                                    Some(last_id),
                                                    None,
                                                )),
                                                window,
                                                cx,
                                            );
                                        }
                                    },
                                ));
                            }
                        }
                    }
                }

                v_flex()
                    .flex_1()
                    .min_h_0()
                    .px_1p5()
                    .pb_2()
                    .gap_0()
                    .overflow_y_scrollbar()
                    .id("sidebar-feature-tree")
                    .children(tree_elements)
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
                        .test_support()
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
                                .id("sidebar-trash-count")
                                .test_support()
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
                        .disabled(!has_workspace)
                        .tooltip(if has_workspace {
                            "Manage global workspace Assignees"
                        } else {
                            "Open or create a workspace first"
                        })
                        .on_click({
                            let app_state = app_state.clone();
                            let store = store.clone();
                            move |_, window, cx| {
                                open_assignees_manager_modal(
                                    app_state.clone(),
                                    store.clone(),
                                    window,
                                    cx,
                                );
                            }
                        })
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
                        .disabled(active_project.is_none())
                        .tooltip(if active_project.is_some() {
                            "Manage Tags for the active Project"
                        } else {
                            "Select or create a Project first"
                        })
                        .on_click({
                            let app_state = app_state.clone();
                            let store = store.clone();
                            let active_project = active_project.clone();
                            move |_, window, cx| {
                                if let Some(proj) = active_project.clone() {
                                    open_tags_manager_modal(
                                        app_state.clone(),
                                        store.clone(),
                                        proj.id,
                                        proj.name,
                                        proj.key_prefix,
                                        window,
                                        cx,
                                    );
                                }
                            }
                        })
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
