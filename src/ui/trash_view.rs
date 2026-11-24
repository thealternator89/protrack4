use chrono::{DateTime, Utc};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    WindowExt as _, h_flex, notification::Notification, v_flex,
};
use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, TestSupportExt as _, Window, div, prelude::FluentBuilder as _, px,
};

use crate::commands::{EmptyTrash, PurgeTrashGroup, RestoreTrashGroup, UndoableCommand};
use crate::domain::{ItemKind, ProjectId, TrashGroup};
use crate::state::{AppState, ProjectStore};
use crate::ui::kind_badge;

pub fn format_relative_deleted_at(deleted_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let delta = now.signed_duration_since(deleted_at);
    let secs = delta.num_seconds();
    if secs < 60 {
        return "just now".to_string();
    }
    let mins = delta.num_minutes();
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = delta.num_hours();
    if hours < 24 {
        return format!(
            "{hours} {} ago",
            if hours == 1 { "hour" } else { "hours" }
        );
    }
    let days = delta.num_days();
    if days < 30 {
        return format!("{days} {} ago", if days == 1 { "day" } else { "days" });
    }
    deleted_at.format("%b %d, %Y").to_string()
}

pub fn format_trash_subtree_summary(group: &TrashGroup) -> Option<String> {
    match (
        group.descendant_feature_count,
        group.descendant_work_item_count,
    ) {
        (0, 0) => None,
        (f, 0) => Some(format!(
            "Includes {f} sub-feature{}",
            if f == 1 { "" } else { "s" }
        )),
        (0, w) => Some(format!(
            "Includes {w} work item{}",
            if w == 1 { "" } else { "s" }
        )),
        (f, w) => Some(format!(
            "Includes {f} sub-feature{} & {w} work item{}",
            if f == 1 { "" } else { "s" },
            if w == 1 { "" } else { "s" }
        )),
    }
}

pub struct TrashView {
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl TrashView {
    pub fn new(
        app_state: Entity<AppState>,
        store: Entity<ProjectStore>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store_sub = cx.observe(&store, |_, _, cx| cx.notify());
        Self {
            app_state,
            store,
            busy: false,
            _subscriptions: vec![store_sub],
        }
    }

    fn dispatch_trash_command(
        &mut self,
        command: Box<dyn UndoableCommand>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        cx.notify();

        let task = self
            .app_state
            .update(cx, |app, cx| app.execute_command(command, cx));
        let view = cx.entity().downgrade();

        window
            .spawn(cx, async move |cx| {
                let res = task.await;
                let _ = view.update(cx, |this, cx| {
                    this.busy = false;
                    cx.notify();
                });
                let _ = cx.update(|window, cx| match res {
                    Ok(effect) => {
                        if let Some(warn) = effect.warning_toast {
                            window.push_notification(Notification::warning(warn), cx);
                        }
                        if let Some(msg) = effect.toast {
                            window.push_notification(Notification::success(msg), cx);
                        }
                    }
                    Err(err) => {
                        window.push_notification(Notification::error(err.to_string()), cx);
                    }
                });
            })
            .detach();
    }

    fn open_purge_confirmation(
        &self,
        group: TrashGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) || self.busy {
            return;
        }

        let trash_view = cx.entity();
        let root_id = group.root.id;
        let item_key = group.root.item_key.clone();
        let title = group.root.title.clone();
        let summary = format_trash_subtree_summary(&group);
        let is_feature = group.root.kind == ItemKind::Feature;

        window.open_dialog(cx, move |dialog, _, _| {
            let trash_view = trash_view.clone();
            let item_key = item_key.clone();
            let title = title.clone();
            let summary = summary.clone();

            dialog
                .width(px(440.))
                .title(format!("Delete {item_key} Forever?"))
                .content(move |content, _, cx| {
                    content.child(
                        v_flex()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().foreground)
                                    .child(format!(
                                        "Permanently delete \"{title}\" ({item_key}) from Trash?"
                                    )),
                            )
                            .when_some(summary.clone(), |this, text| {
                                this.child(
                                    div()
                                        .px_2p5()
                                        .py_1p5()
                                        .rounded(cx.theme().radius)
                                        .bg(cx.theme().muted)
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(text),
                                )
                            })
                            .when(is_feature, |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            "Any shared Work Items still attached to other active Features will remain intact.",
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .font_medium()
                                    .text_color(cx.theme().danger)
                                    .child("This action cannot be undone."),
                            )
                            .child(
                                DialogFooter::new()
                                    .pt_1()
                                    .child(
                                        Button::new("trash-confirm-cancel")
                                            .outline()
                                            .small()
                                            .label("Cancel")
                                            .on_click(|_, window, cx| {
                                                window.close_dialog(cx);
                                            }),
                                    )
                                    .child(
                                        Button::new("trash-confirm-purge")
                                            .danger()
                                            .small()
                                            .icon(IconName::Delete)
                                            .label("Delete Forever")
                                            .on_click({
                                                let trash_view = trash_view.clone();
                                                move |_, window, cx| {
                                                    window.close_dialog(cx);
                                                    trash_view.update(cx, |this, cx| {
                                                        this.dispatch_trash_command(
                                                            Box::new(PurgeTrashGroup::new(root_id)),
                                                            window,
                                                            cx,
                                                        );
                                                    });
                                                }
                                            }),
                                    ),
                            ),
                    )
                })
        });
    }

    fn open_empty_trash_confirmation(
        &self,
        project_id: ProjectId,
        project_name: String,
        group_count: usize,
        total_items: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) || self.busy || group_count == 0 {
            return;
        }

        let trash_view = cx.entity();

        window.open_dialog(cx, move |dialog, _, _| {
            let trash_view = trash_view.clone();
            let project_name = project_name.clone();

            dialog
                .width(px(440.))
                .title("Empty Trash?")
                .content(move |content, _, cx| {
                    content.child(
                        v_flex()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().foreground)
                                    .child(format!(
                                        "Permanently delete all {group_count} Trash entry(ies) ({total_items} total item(s)) in {project_name}?"
                                    )),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "Shared Work Items attached to active Features outside Trash will remain completely safe.",
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_medium()
                                    .text_color(cx.theme().danger)
                                    .child("This action cannot be undone and clears permanent history."),
                            )
                            .child(
                                DialogFooter::new()
                                    .pt_1()
                                    .child(
                                        Button::new("trash-confirm-cancel")
                                            .outline()
                                            .small()
                                            .label("Cancel")
                                            .on_click(|_, window, cx| {
                                                window.close_dialog(cx);
                                            }),
                                    )
                                    .child(
                                        Button::new("trash-confirm-empty")
                                            .danger()
                                            .small()
                                            .icon(IconName::Delete)
                                            .label("Empty Trash")
                                            .on_click({
                                                let trash_view = trash_view.clone();
                                                move |_, window, cx| {
                                                    window.close_dialog(cx);
                                                    trash_view.update(cx, |this, cx| {
                                                        this.dispatch_trash_command(
                                                            Box::new(EmptyTrash::new(project_id)),
                                                            window,
                                                            cx,
                                                        );
                                                    });
                                                }
                                            }),
                                    ),
                            ),
                    )
                })
        });
    }
}

impl Render for TrashView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(snapshot) = self.store.read(cx).snapshot().cloned() else {
            return v_flex().flex_1().into_any_element();
        };

        let project_id = snapshot.project.id;
        let project_name = snapshot.project.name.clone();
        let trash_groups = snapshot.trash_groups.clone();
        let group_count = trash_groups.len();
        let total_items: usize = trash_groups.iter().map(|g| g.total_item_count).sum();
        let now = Utc::now();
        let busy = self.busy;

        v_flex()
            .flex_1()
            .min_h_0()
            .p_4()
            .gap_3()
            // Top header bar
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "Deleted Feature subtrees and Work Items. Restoring returns items and links in place.",
                                    ),
                            )
                            .when(total_items > 0, |this| {
                                this.child(
                                    div().flex_shrink_0().child(
                                        Tag::secondary()
                                            .xsmall()
                                            .child(format!("{total_items} trashed item(s)")),
                                    ),
                                )
                            }),
                    )
                    .child(
                        div().flex_shrink_0().child(
                            Button::new("trash-empty-btn")
                                .danger()
                                .xsmall()
                                .icon(IconName::Delete)
                                .label("Empty Trash")
                                .disabled(trash_groups.is_empty() || busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_empty_trash_confirmation(
                                        project_id,
                                        project_name.clone(),
                                        group_count,
                                        total_items,
                                        window,
                                        cx,
                                    );
                                })),
                        ),
                    ),
            )
            // Body
            .child(if trash_groups.is_empty() {
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
                            .child(
                                "Items moved to Trash can be restored or permanently purged here.",
                            ),
                    )
                    .into_any_element()
            } else {
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_2()
                    .overflow_y_scrollbar()
                    .id("trash-list-scroll")
                    .children(trash_groups.into_iter().map(|group| {
                        let root_id = group.root.id;
                        let root_num = group.root.item_number as usize;
                        let kind = group.root.kind;
                        let key = group.root.item_key.clone();
                        let title = group.root.title.clone();
                        let deleted_rel = format_relative_deleted_at(group.deleted_at, now);
                        let subtree_summary = format_trash_subtree_summary(&group);
                        let kind_icon = match kind {
                            ItemKind::Feature => IconName::Folder,
                            ItemKind::WorkItem => IconName::CircleCheck,
                        };
                        let kind_color = match kind {
                            ItemKind::Feature => cx.theme().primary,
                            ItemKind::WorkItem => cx.theme().info,
                        };
                        let group_for_purge = group.clone();

                        h_flex()
                            .id(("trash-group-row", root_num))
                            .test_support()
                            .flex_shrink_0()
                            .w_full()
                            .px_3()
                            .py_2p5()
                            .gap_3()
                            .items_center()
                            .justify_between()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().popover)
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_2p5()
                                    .items_start()
                                    .child(
                                        div().pt_0p5().child(
                                            Icon::new(kind_icon).small().text_color(kind_color),
                                        ),
                                    )
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap_1()
                                            .child(
                                                h_flex()
                                                    .gap_1p5()
                                                    .items_center()
                                                    .min_w_0()
                                                    .child(
                                                        div()
                                                            .px_1p5()
                                                            .py_0p5()
                                                            .rounded(cx.theme().radius * 0.75)
                                                            .bg(cx.theme().muted)
                                                            .text_size(px(10.))
                                                            .font_semibold()
                                                            .font_family(
                                                                cx.theme().mono_font_family.clone(),
                                                            )
                                                            .text_color(
                                                                cx.theme().muted_foreground,
                                                            )
                                                            .child(key),
                                                    )
                                                    .child(kind_badge(kind, cx))
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .overflow_hidden()
                                                            .whitespace_nowrap()
                                                            .text_ellipsis()
                                                            .text_sm()
                                                            .font_medium()
                                                            .child(title),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_1p5()
                                                    .items_center()
                                                    .flex_wrap()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(
                                                                cx.theme().muted_foreground,
                                                            )
                                                            .child(format!(
                                                                "Deleted {deleted_rel}"
                                                            )),
                                                    )
                                                    .when_some(subtree_summary, |this, text| {
                                                        this.child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(
                                                                    cx.theme().muted_foreground,
                                                                )
                                                                .child(format!("· {text}")),
                                                        )
                                                    }),
                                            ),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .flex_shrink_0()
                                    .items_center()
                                    .child(
                                        Button::new(("trash-restore-btn", root_num))
                                            .outline()
                                            .xsmall()
                                            .icon(IconName::Undo2)
                                            .label("Restore")
                                            .disabled(busy)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.dispatch_trash_command(
                                                    Box::new(RestoreTrashGroup::new(root_id)),
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    )
                                    .child(
                                        Button::new(("trash-purge-btn", root_num))
                                            .danger()
                                            .xsmall()
                                            .icon(IconName::Delete)
                                            .label("Delete Forever")
                                            .disabled(busy)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.open_purge_confirmation(
                                                    group_for_purge.clone(),
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    ),
                            )
                    }))
                    .into_any_element()
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;
    use uuid::Uuid;

    use super::*;
    use crate::domain::{Item, ItemId, ItemStatus, Priority};

    fn make_dummy_item(kind: ItemKind) -> Item {
        let now = Utc.with_ymd_and_hms(2026, 11, 24, 12, 0, 0).unwrap();
        Item {
            id: ItemId::from_uuid(Uuid::nil()),
            project_id: ProjectId::from_uuid(Uuid::nil()),
            item_number: 1,
            item_key: "PRO-1".to_string(),
            kind,
            parent_feature_id: None,
            title: "Sample".to_string(),
            description: String::new(),
            status: ItemStatus::Todo,
            priority: Priority::None,
            assignee_id: None,
            sort_order: "a0".to_string(),
            deleted_at: Some(now),
            trash_root_id: Some(ItemId::from_uuid(Uuid::nil())),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn relative_deleted_timestamp_formats_human_intervals() {
        let now = Utc.with_ymd_and_hms(2026, 11, 24, 12, 0, 0).unwrap();
        assert_eq!(
            format_relative_deleted_at(now - chrono::Duration::seconds(25), now),
            "just now"
        );
        assert_eq!(
            format_relative_deleted_at(now - chrono::Duration::minutes(12), now),
            "12m ago"
        );
        assert_eq!(
            format_relative_deleted_at(now - chrono::Duration::hours(1), now),
            "1 hour ago"
        );
        assert_eq!(
            format_relative_deleted_at(now - chrono::Duration::hours(3), now),
            "3 hours ago"
        );
        assert_eq!(
            format_relative_deleted_at(now - chrono::Duration::days(1), now),
            "1 day ago"
        );
        assert_eq!(
            format_relative_deleted_at(now - chrono::Duration::days(5), now),
            "5 days ago"
        );
        assert_eq!(
            format_relative_deleted_at(now - chrono::Duration::days(45), now),
            "Oct 10, 2026"
        );
    }

    #[test]
    fn subtree_summary_handles_singular_plural_and_empty() {
        let now = Utc.with_ymd_and_hms(2026, 11, 24, 12, 0, 0).unwrap();
        let single_wi = TrashGroup {
            root: make_dummy_item(ItemKind::WorkItem),
            deleted_at: now,
            descendant_feature_count: 0,
            descendant_work_item_count: 0,
            total_item_count: 1,
        };
        assert_eq!(format_trash_subtree_summary(&single_wi), None);

        let one_sub = TrashGroup {
            root: make_dummy_item(ItemKind::Feature),
            deleted_at: now,
            descendant_feature_count: 1,
            descendant_work_item_count: 0,
            total_item_count: 2,
        };
        assert_eq!(
            format_trash_subtree_summary(&one_sub).as_deref(),
            Some("Includes 1 sub-feature")
        );

        let two_wis = TrashGroup {
            root: make_dummy_item(ItemKind::Feature),
            deleted_at: now,
            descendant_feature_count: 0,
            descendant_work_item_count: 2,
            total_item_count: 3,
        };
        assert_eq!(
            format_trash_subtree_summary(&two_wis).as_deref(),
            Some("Includes 2 work items")
        );

        let mixed = TrashGroup {
            root: make_dummy_item(ItemKind::Feature),
            deleted_at: now,
            descendant_feature_count: 3,
            descendant_work_item_count: 7,
            total_item_count: 11,
        };
        assert_eq!(
            format_trash_subtree_summary(&mixed).as_deref(),
            Some("Includes 3 sub-features & 7 work items")
        );
    }
}
