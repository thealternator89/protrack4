use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex,
    v_flex,
};
use gpui_kit::{
    App, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use crate::domain::ItemKind;
use crate::state::ProjectStore;
use crate::ui::{assignee_pill, domain_tag_chip, kind_badge, priority_badge, status_badge};

pub fn render_inspector(
    store: &Entity<ProjectStore>,
    _window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let state = store.read(cx);
    let snapshot = state.snapshot();
    let has_workspace = state.has_workspace();
    let selected = state.selected_item();

    v_flex()
        .size_full()
        .overflow_hidden()
        .bg(cx.theme().background)
        .text_color(cx.theme().foreground)
        // Inspector Header
        .child(
            h_flex()
                .h(px(40.))
                .px_3()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(
                            Icon::new(IconName::Inspector)
                                .small()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(div().text_sm().font_semibold().child("Inspector")),
                )
                .child(Tag::secondary().xsmall().child("Read-only")),
        )
        // Inspector Body
        .child(match (snapshot, selected) {
            (None, _) => empty_inspector_message(
                if has_workspace {
                    "This workspace has no projects yet."
                } else {
                    "Open or create a .protrack workspace to inspect items."
                },
                cx,
            )
            .into_any_element(),
            (Some(_), None) => empty_inspector_message(
                "Select a Feature or Work Item to inspect its details.",
                cx,
            )
            .into_any_element(),
            (Some(snapshot), Some(item)) => {
                let tags = snapshot.tags_for_item(item.id);
                let assignee = item.assignee_id.and_then(|aid| snapshot.assignee(aid));
                let convert_label = match item.kind {
                    ItemKind::Feature => "Convert to Work Item",
                    ItemKind::WorkItem => "Convert to Feature",
                };

                let created = item.created_at.format("%Y-%m-%d %H:%M UTC").to_string();
                let updated = item.updated_at.format("%Y-%m-%d %H:%M UTC").to_string();

                let relation_section = match item.kind {
                    ItemKind::Feature => {
                        let path = snapshot.feature_path(item.id);
                        let ancestors: Vec<_> =
                            path.into_iter().filter(|f| f.id != item.id).collect();
                        let (done, total) = snapshot.feature_progress(item.id);
                        let pct = if total > 0 {
                            (done * 100) / total
                        } else {
                            0
                        };
                        let child_features = snapshot.child_features(Some(item.id)).len();
                        let attached_wis = snapshot.work_items_for_feature(item.id).len();

                        v_flex()
                            .gap_2()
                            .p_3()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().muted.opacity(0.45))
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_semibold()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("HIERARCHY & PROGRESS"),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .flex_wrap()
                                    .child(
                                        div()
                                            .w(px(76.))
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Parent"),
                                    )
                                    .child(if ancestors.is_empty() {
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Root Feature")
                                            .into_any_element()
                                    } else {
                                        h_flex()
                                            .gap_1()
                                            .flex_wrap()
                                            .items_center()
                                            .children(ancestors.into_iter().enumerate().map(
                                                |(ix, anc)| {
                                                    let anc_id = anc.id;
                                                    h_flex()
                                                        .gap_1()
                                                        .items_center()
                                                        .when(ix > 0, |this| {
                                                            this.child(
                                                                div()
                                                                    .text_xs()
                                                                    .text_color(
                                                                        cx.theme()
                                                                            .muted_foreground,
                                                                    )
                                                                    .child("›"),
                                                            )
                                                        })
                                                        .child(
                                                            Button::new((
                                                                "inspector-ancestor",
                                                                anc.item_number as usize,
                                                            ))
                                                            .ghost()
                                                            .xsmall()
                                                            .compact()
                                                            .label(format!(
                                                                "{} {}",
                                                                anc.item_key, anc.title
                                                            ))
                                                            .on_click({
                                                                let store = store.clone();
                                                                move |_, _, cx| {
                                                                    store.update(cx, |s, cx| {
                                                                        s.select_item(Some(anc_id));
                                                                        cx.notify();
                                                                    });
                                                                }
                                                            }),
                                                        )
                                                },
                                            ))
                                            .into_any_element()
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .w(px(76.))
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Progress"),
                                    )
                                    .child(
                                        h_flex()
                                            .flex_1()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .h(px(6.))
                                                    .rounded(px(999.))
                                                    .bg(cx.theme().muted)
                                                    .overflow_hidden()
                                                    .child(
                                                        div()
                                                            .h_full()
                                                            .w(px(if total == 0 {
                                                                0.0
                                                            } else {
                                                                120.0 * (done as f32 / total as f32)
                                                            }))
                                                            .bg(if total > 0 && done == total {
                                                                cx.theme().success
                                                            } else {
                                                                cx.theme().primary
                                                            }),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_family(cx.theme().mono_font_family.clone())
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(if total == 0 {
                                                        "0/0 (—)".to_string()
                                                    } else {
                                                        format!("{done}/{total} ({pct}%)")
                                                    }),
                                            ),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .w(px(76.))
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Contents"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().foreground)
                                            .child(format!(
                                                "{child_features} sub-feature(s) · {attached_wis} work item(s)"
                                            )),
                                    ),
                            )
                    }
                    ItemKind::WorkItem => {
                        let attached = snapshot.features_for_work_item(item.id);
                        let attached_len = attached.len();

                        v_flex()
                            .gap_2()
                            .p_3()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().muted.opacity(0.45))
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_semibold()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("ATTACHED FEATURES ({attached_len})")),
                            )
                            .child(if attached.is_empty() {
                                h_flex()
                                    .gap_1p5()
                                    .items_center()
                                    .child(
                                        Tag::warning()
                                            .xsmall()
                                            .outline()
                                            .child("Unallocated"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Not yet assigned to a Feature"),
                                    )
                                    .into_any_element()
                            } else {
                                v_flex()
                                    .gap_1()
                                    .children(attached.into_iter().map(|feat| {
                                        let feat_id = feat.id;
                                        h_flex()
                                            .id(("inspector-feature-link", feat.item_number as usize))
                                            .px_2()
                                            .py_1()
                                            .gap_1p5()
                                            .items_center()
                                            .rounded(cx.theme().radius * 0.75)
                                            .bg(cx.theme().background)
                                            .border_1()
                                            .border_color(cx.theme().border)
                                            .cursor_pointer()
                                            .hover(|s| s.bg(cx.theme().accent))
                                            .on_click({
                                                let store = store.clone();
                                                move |_, _, cx| {
                                                    store.update(cx, |s, cx| {
                                                        s.select_item(Some(feat_id));
                                                        cx.notify();
                                                    });
                                                }
                                            })
                                            .child(
                                                Icon::new(IconName::Folder)
                                                    .xsmall()
                                                    .text_color(cx.theme().primary),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_family(
                                                        cx.theme().mono_font_family.clone(),
                                                    )
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(feat.item_key.clone()),
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
                                                    .child(feat.title.clone()),
                                            )
                                    }))
                                    .into_any_element()
                            })
                    }
                };

                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .gap_4()
                    .overflow_y_scrollbar()
                    .id("inspector-scroll")
                    // Identity + disabled Convert action
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .px_2()
                                                    .py_0p5()
                                                    .rounded(cx.theme().radius * 0.75)
                                                    .bg(cx.theme().muted)
                                                    .text_xs()
                                                    .font_semibold()
                                                    .font_family(
                                                        cx.theme().mono_font_family.clone(),
                                                    )
                                                    .child(item.item_key.clone()),
                                            )
                                            .child(kind_badge(item.kind, cx)),
                                    )
                                    .child(
                                        Button::new("inspector-convert-stub")
                                            .outline()
                                            .xsmall()
                                            .icon(IconName::ArrowRight)
                                            .label(convert_label)
                                            .disabled(true)
                                            .tooltip(
                                                "Feature ↔ Work Item conversion — Available in Phase 8",
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .text_base()
                                    .font_semibold()
                                    .line_height(px(22.))
                                    .child(item.title.clone()),
                            ),
                    )
                    // Metadata rows
                    .child(
                        v_flex()
                            .gap_2p5()
                            .p_3()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .w(px(76.))
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Status"),
                                    )
                                    .child(status_badge(item.status, cx)),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .w(px(76.))
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Priority"),
                                    )
                                    .child(priority_badge(item.priority, cx)),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .w(px(76.))
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Assignee"),
                                    )
                                    .child(match assignee {
                                        Some(a) => assignee_pill(a, true, cx).into_any_element(),
                                        None => div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Unassigned")
                                            .into_any_element(),
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_start()
                                    .child(
                                        div()
                                            .w(px(76.))
                                            .pt_0p5()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Tags"),
                                    )
                                    .child(if tags.is_empty() {
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("No tags")
                                            .into_any_element()
                                    } else {
                                        h_flex()
                                            .flex_1()
                                            .gap_1p5()
                                            .flex_wrap()
                                            .children(
                                                tags.into_iter()
                                                    .map(|tag| domain_tag_chip(tag, cx)),
                                            )
                                            .into_any_element()
                                    }),
                            ),
                    )
                    // Hierarchy or Attached Features
                    .child(relation_section)
                    // Description (Markdown Preview)
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(
                                h_flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .font_semibold()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("DESCRIPTION"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Markdown Preview"),
                                    ),
                            )
                            .child(
                                div()
                                    .p_3()
                                    .rounded(cx.theme().radius)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().popover)
                                    .text_sm()
                                    .child(if item.description.trim().is_empty() {
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("No description provided.")
                                            .into_any_element()
                                    } else {
                                        TextView::markdown(
                                            ("inspector-markdown", item.item_number as usize),
                                            item.description.clone(),
                                        )
                                        .selectable(true)
                                        .into_any_element()
                                    }),
                            ),
                    )
                    // Timestamps footer
                    .child(
                        div()
                            .pt_1()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Created {created} · Updated {updated}")),
                    )
                    .into_any_element()
            }
        })
}

fn empty_inspector_message(message: &'static str, cx: &App) -> impl IntoElement {
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .p_6()
        .gap_2()
        .child(
            Icon::new(IconName::Inspector)
                .large()
                .text_color(cx.theme().muted_foreground),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(message),
        )
}
