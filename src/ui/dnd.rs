use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    notification::Notification, v_flex,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Point, Render, Styled as _, TestSupportExt as _,
    Window, div, prelude::FluentBuilder as _, px,
};

use crate::commands::{
    AttachWorkItemToFeature, DetachWorkItemFromFeature, MoveWorkItemBetweenFeatures,
    ReparentFeature, SetWorkItemFeatures, UndoableCommand,
};
use crate::domain::{ItemId, ItemKind, MAX_FEATURE_DEPTH, ProjectId, ProjectSnapshot};
use crate::state::{AppState, ProjectStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragPayload {
    Feature {
        project_id: ProjectId,
        feature_id: ItemId,
        parent_feature_id: Option<ItemId>,
        item_key: String,
        title: String,
    },
    WorkItem {
        project_id: ProjectId,
        work_item_id: ItemId,
        source_feature_id: Option<ItemId>,
        item_key: String,
        title: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropEvaluation {
    Valid,
    Invalid(String),
    NoOp,
}

pub struct DragGhostView {
    payload: DragPayload,
}

impl DragGhostView {
    pub fn new(payload: DragPayload) -> Self {
        Self { payload }
    }
}

impl Render for DragGhostView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let alt_held = window.modifiers().alt;
        let (icon, item_key, title, show_also_attach) = match &self.payload {
            DragPayload::Feature {
                item_key, title, ..
            } => (IconName::Folder, item_key.clone(), title.clone(), false),
            DragPayload::WorkItem {
                item_key,
                title,
                source_feature_id,
                ..
            } => (
                IconName::CircleCheck,
                item_key.clone(),
                title.clone(),
                source_feature_id.is_some() && alt_held,
            ),
        };

        h_flex()
            .px_2p5()
            .py_1()
            .gap_1p5()
            .items_center()
            .max_w(px(280.))
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .border_1()
            .border_color(if show_also_attach {
                cx.theme().info
            } else {
                cx.theme().primary.opacity(0.65)
            })
            .shadow_md()
            .child(
                Icon::new(icon)
                    .xsmall()
                    .text_color(cx.theme().primary),
            )
            .child(
                div()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(cx.theme().muted_foreground)
                    .child(item_key),
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
                    .text_color(cx.theme().foreground)
                    .child(title),
            )
            .when(show_also_attach, |this| {
                this.child(Tag::info().xsmall().child("+ Also Attach"))
            })
    }
}

pub fn make_drag_ghost(
    payload: &DragPayload,
    _offset: Point<Pixels>,
    _window: &mut Window,
    cx: &mut App,
) -> Entity<DragGhostView> {
    let payload = payload.clone();
    cx.new(|_| DragGhostView::new(payload))
}

pub fn evaluate_feature_drop(
    snapshot: &ProjectSnapshot,
    payload: &DragPayload,
    target_feature_id: ItemId,
    alt_held: bool,
) -> DropEvaluation {
    let Some(target) = snapshot.item(target_feature_id) else {
        return DropEvaluation::NoOp;
    };
    if target.project_id != snapshot.project.id
        || !target.is_active()
        || target.kind != ItemKind::Feature
    {
        return DropEvaluation::NoOp;
    }

    match payload {
        DragPayload::Feature {
            project_id,
            feature_id,
            parent_feature_id,
            ..
        } => {
            if *project_id != snapshot.project.id || *feature_id == target_feature_id {
                return DropEvaluation::NoOp;
            }
            if *parent_feature_id == Some(target_feature_id) {
                return DropEvaluation::NoOp;
            }
            if snapshot
                .descendant_feature_ids(*feature_id)
                .contains(&target_feature_id)
            {
                return DropEvaluation::Invalid(
                    "Cannot move a Feature under one of its own descendants".to_string(),
                );
            }
            let target_depth = snapshot.feature_depth(target_feature_id);
            let subtree_height = snapshot.feature_subtree_height(*feature_id);
            if target_depth + 1 + subtree_height >= MAX_FEATURE_DEPTH {
                return DropEvaluation::Invalid(format!(
                    "Maximum Feature nesting depth ({MAX_FEATURE_DEPTH}) would be exceeded"
                ));
            }
            if let Some(reason) = snapshot.reparent_blocker_conflict(*feature_id, target_feature_id)
            {
                return DropEvaluation::Invalid(format!(
                    "Cannot reparent Feature: {reason} (INV-05)"
                ));
            }
            DropEvaluation::Valid
        }
        DragPayload::WorkItem {
            project_id,
            work_item_id,
            source_feature_id,
            ..
        } => {
            if *project_id != snapshot.project.id {
                return DropEvaluation::NoOp;
            }
            if *source_feature_id == Some(target_feature_id) {
                return DropEvaluation::NoOp;
            }
            if alt_held
                && snapshot
                    .features_for_work_item(*work_item_id)
                    .iter()
                    .any(|f| f.id == target_feature_id)
            {
                return DropEvaluation::NoOp;
            }
            if let Some(reason) =
                snapshot.attach_work_item_blocker_conflict(*work_item_id, target_feature_id)
            {
                return DropEvaluation::Invalid(format!("Cannot attach Work Item: {reason} (INV-05)"));
            }
            DropEvaluation::Valid
        }
    }
}

pub fn evaluate_root_drop(snapshot: &ProjectSnapshot, payload: &DragPayload) -> DropEvaluation {
    match payload {
        DragPayload::Feature {
            project_id,
            parent_feature_id: Some(_),
            ..
        } if *project_id == snapshot.project.id => DropEvaluation::Valid,
        _ => DropEvaluation::NoOp,
    }
}

pub fn evaluate_unallocated_drop(
    snapshot: &ProjectSnapshot,
    payload: &DragPayload,
) -> DropEvaluation {
    match payload {
        DragPayload::WorkItem {
            project_id,
            source_feature_id: Some(_),
            ..
        } if *project_id == snapshot.project.id => DropEvaluation::Valid,
        _ => DropEvaluation::NoOp,
    }
}

pub fn is_valid_feature_sibling_gap(
    payload: &DragPayload,
    project_id: ProjectId,
    parent_feature_id: Option<ItemId>,
    before_sibling_id: Option<ItemId>,
    after_sibling_id: Option<ItemId>,
) -> bool {
    matches!(
        payload,
        DragPayload::Feature {
            project_id: pid,
            feature_id,
            parent_feature_id: src_parent,
            ..
        } if *pid == project_id
            && *src_parent == parent_feature_id
            && before_sibling_id != Some(*feature_id)
            && after_sibling_id != Some(*feature_id)
    )
}

pub fn is_valid_feature_wi_gap(
    payload: &DragPayload,
    project_id: ProjectId,
    feature_id: ItemId,
    before_wi_id: Option<ItemId>,
    after_wi_id: Option<ItemId>,
) -> bool {
    matches!(
        payload,
        DragPayload::WorkItem {
            project_id: pid,
            work_item_id,
            source_feature_id: Some(src_fid),
            ..
        } if *pid == project_id
            && *src_fid == feature_id
            && before_wi_id != Some(*work_item_id)
            && after_wi_id != Some(*work_item_id)
    )
}

pub fn is_valid_unallocated_gap(
    payload: &DragPayload,
    project_id: ProjectId,
    before_wi_id: Option<ItemId>,
    after_wi_id: Option<ItemId>,
) -> bool {
    matches!(
        payload,
        DragPayload::WorkItem {
            project_id: pid,
            work_item_id,
            source_feature_id: None,
            ..
        } if *pid == project_id
            && before_wi_id != Some(*work_item_id)
            && after_wi_id != Some(*work_item_id)
    )
}

pub fn dispatch_dnd_command(
    app_state: Entity<AppState>,
    command: Box<dyn UndoableCommand>,
    window: &mut Window,
    cx: &mut App,
) {
    let task = app_state.update(cx, |app, cx| app.execute_command(command, cx));
    window
        .spawn(cx, async move |cx| {
            let res = task.await;
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

pub fn handle_feature_drop(
    app_state: &Entity<AppState>,
    store: &Entity<ProjectStore>,
    payload: &DragPayload,
    target_feature_id: ItemId,
    alt_held: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(snapshot) = store.read(cx).snapshot().cloned() else {
        return;
    };

    match evaluate_feature_drop(&snapshot, payload, target_feature_id, alt_held) {
        DropEvaluation::NoOp => {}
        DropEvaluation::Invalid(reason) => {
            window.push_notification(Notification::error(reason), cx);
        }
        DropEvaluation::Valid => match payload {
            DragPayload::Feature { feature_id, .. } => {
                store.update(cx, |s, cx| {
                    s.expand_feature(target_feature_id);
                    cx.notify();
                });
                dispatch_dnd_command(
                    app_state.clone(),
                    Box::new(ReparentFeature::new(*feature_id, Some(target_feature_id))),
                    window,
                    cx,
                );
            }
            DragPayload::WorkItem {
                work_item_id,
                source_feature_id,
                ..
            } => {
                store.update(cx, |s, cx| {
                    s.expand_feature(target_feature_id);
                    cx.notify();
                });
                let command: Box<dyn UndoableCommand> = match source_feature_id {
                    None => Box::new(AttachWorkItemToFeature::new(
                        *work_item_id,
                        target_feature_id,
                    )),
                    Some(_) if alt_held => Box::new(AttachWorkItemToFeature::new(
                        *work_item_id,
                        target_feature_id,
                    )),
                    Some(from_fid) => Box::new(MoveWorkItemBetweenFeatures::new(
                        *work_item_id,
                        *from_fid,
                        target_feature_id,
                    )),
                };
                dispatch_dnd_command(app_state.clone(), command, window, cx);
            }
        },
    }
}

pub fn handle_root_drop(
    app_state: &Entity<AppState>,
    store: &Entity<ProjectStore>,
    payload: &DragPayload,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(snapshot) = store.read(cx).snapshot().cloned() else {
        return;
    };
    if evaluate_root_drop(&snapshot, payload) != DropEvaluation::Valid {
        return;
    }
    if let DragPayload::Feature { feature_id, .. } = payload {
        dispatch_dnd_command(
            app_state.clone(),
            Box::new(ReparentFeature::new(*feature_id, None)),
            window,
            cx,
        );
    }
}

pub fn handle_unallocated_drop(
    app_state: &Entity<AppState>,
    store: &Entity<ProjectStore>,
    payload: &DragPayload,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(snapshot) = store.read(cx).snapshot().cloned() else {
        return;
    };
    if evaluate_unallocated_drop(&snapshot, payload) != DropEvaluation::Valid {
        return;
    }

    let DragPayload::WorkItem {
        work_item_id,
        source_feature_id: Some(from_fid),
        item_key,
        ..
    } = payload
    else {
        return;
    };

    let attached = snapshot.features_for_work_item(*work_item_id);
    if attached.len() <= 1 {
        dispatch_dnd_command(
            app_state.clone(),
            Box::new(DetachWorkItemFromFeature::new(*work_item_id, *from_fid)),
            window,
            cx,
        );
        return;
    }

    let wi_id = *work_item_id;
    let from_id = *from_fid;
    let wi_key = item_key.clone();
    let from_key = snapshot
        .item(from_id)
        .map(|f| f.item_key.clone())
        .unwrap_or_else(|| "Feature".to_string());
    let total_count = attached.len();
    let app_for_single = app_state.clone();
    let app_for_all = app_state.clone();

    if window.has_active_dialog(cx) {
        return;
    }

    window.open_dialog(cx, move |dialog, _, _cx| {
        let wi_key = wi_key.clone();
        let from_key = from_key.clone();
        let app_single = app_for_single.clone();
        let app_all = app_for_all.clone();

        dialog
            .width(px(440.))
            .title(format!("Unallocate {wi_key}?"))
            .content(move |content, _, cx| {
                let app_single_click = app_single.clone();
                let app_all_click = app_all.clone();
                content.child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().foreground)
                                .child(format!(
                                    "{wi_key} is currently attached to {total_count} Features. Would you like to detach it only from {from_key}, or remove all Feature attachments so it enters Unallocated?"
                                )),
                        )
                        .child(
                            DialogFooter::new()
                                .pt_1()
                                .child(
                                    Button::new("dnd-unalloc-cancel")
                                        .outline()
                                        .small()
                                        .label("Cancel")
                                        .on_click(|_, window, cx| {
                                            window.close_dialog(cx);
                                        }),
                                )
                                .child(
                                    Button::new("dnd-unalloc-single")
                                        .outline()
                                        .small()
                                        .label(format!("Detach from {from_key}"))
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            dispatch_dnd_command(
                                                app_single_click.clone(),
                                                Box::new(DetachWorkItemFromFeature::new(
                                                    wi_id, from_id,
                                                )),
                                                window,
                                                cx,
                                            );
                                        }),
                                )
                                .child(
                                    Button::new("dnd-unalloc-all")
                                        .warning()
                                        .small()
                                        .label(format!("Unallocate All ({total_count})"))
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            dispatch_dnd_command(
                                                app_all_click.clone(),
                                                Box::new(SetWorkItemFeatures::new(
                                                    wi_id,
                                                    Vec::new(),
                                                )),
                                                window,
                                                cx,
                                            );
                                        }),
                                ),
                        ),
                )
            })
    });
}

pub fn render_drop_gap(
    id: impl Into<ElementId>,
    indent_px: f32,
    can_accept: impl Fn(&DragPayload) -> bool + 'static,
    on_accept: impl Fn(&DragPayload, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let can_accept = Arc::new(can_accept);
    let can_accept_drop = Arc::clone(&can_accept);

    h_flex()
        .id(id.into())
        .test_support()
        .w_full()
        .h(px(4.))
        .pl(px(indent_px))
        .pr_2()
        .rounded(px(2.))
        .drag_over::<DragPayload>(move |style, payload, _, cx| {
            if can_accept(payload) {
                style.bg(cx.theme().primary.opacity(0.85))
            } else {
                style
            }
        })
        .on_drop::<DragPayload>(move |payload, window, cx| {
            if can_accept_drop(payload) {
                on_accept(payload, window, cx);
            }
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{DateTime, TimeZone, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::domain::{
        FeatureWorkItem, Item, ItemBlocker, ItemStatus, Priority, Project, ProjectStatus,
    };

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 24, 12, 0, 0).unwrap()
    }

    fn make_item(
        pid: ProjectId,
        id: ItemId,
        num: i64,
        kind: ItemKind,
        parent: Option<ItemId>,
    ) -> Item {
        Item {
            id,
            project_id: pid,
            item_number: num,
            item_key: format!("DND-{num}"),
            kind,
            parent_feature_id: parent,
            title: format!("Item {num}"),
            description: String::new(),
            status: ItemStatus::Todo,
            priority: Priority::None,
            assignee_id: None,
            link: None,
            sort_order: format!("a{num}"),
            deleted_at: None,
            trash_root_id: None,
            created_at: ts(),
            updated_at: ts(),
        }
    }

    #[test]
    fn evaluate_feature_drop_checks_cycles_depth_blockers_and_alt_attach() {
        let pid = ProjectId::from_uuid(Uuid::from_u128(1));
        let f1 = ItemId::from_uuid(Uuid::from_u128(10));
        let f2 = ItemId::from_uuid(Uuid::from_u128(11));
        let f3 = ItemId::from_uuid(Uuid::from_u128(12));
        let f4 = ItemId::from_uuid(Uuid::from_u128(13));
        let wi = ItemId::from_uuid(Uuid::from_u128(20));

        let project = Project {
            id: pid,
            name: "DnD".to_string(),
            key_prefix: "DND".to_string(),
            next_item_seq: 6,
            description: String::new(),
            color: "#6366F1".to_string(),
            status: ProjectStatus::Active,
            sort_order: "a0".to_string(),
            created_at: ts(),
            updated_at: ts(),
        };

        let snapshot = ProjectSnapshot {
            project: project.clone(),
            projects: vec![project],
            items: vec![
                make_item(pid, f1, 1, ItemKind::Feature, None),
                make_item(pid, f2, 2, ItemKind::Feature, Some(f1)),
                make_item(pid, f3, 3, ItemKind::Feature, None),
                make_item(pid, f4, 4, ItemKind::Feature, None),
                make_item(pid, wi, 5, ItemKind::WorkItem, None),
            ],
            assignees: Vec::new(),
            tags: Vec::new(),
            feature_work_items: vec![FeatureWorkItem {
                project_id: pid,
                feature_id: f2,
                work_item_id: wi,
                sort_order: "a0".to_string(),
                created_at: ts(),
            }],
            item_tags: HashMap::new(),
            progress_by_feature: HashMap::new(),
            item_blockers: vec![ItemBlocker {
                project_id: pid,
                blocked_item_id: wi,
                blocker_feature_id: f3,
                created_at: ts(),
            }],
            direct_blockers_by_item: HashMap::new(),
            inherited_blockers_by_item: HashMap::new(),
            effective_block_state: HashMap::new(),
            blocker_warnings: HashMap::new(),
            trash_count: 0,
            trash_groups: Vec::new(),
        };

        let f1_payload = DragPayload::Feature {
            project_id: pid,
            feature_id: f1,
            parent_feature_id: None,
            item_key: "DND-1".to_string(),
            title: "Item 1".to_string(),
        };
        assert_eq!(
            evaluate_feature_drop(&snapshot, &f1_payload, f1, false),
            DropEvaluation::NoOp
        );
        assert!(matches!(
            evaluate_feature_drop(&snapshot, &f1_payload, f2, false),
            DropEvaluation::Invalid(_)
        ));
        // Reparenting f1 under f3 would place blocked WI (under f2) inside its own blocker f3 (INV-05).
        assert!(matches!(
            evaluate_feature_drop(&snapshot, &f1_payload, f3, false),
            DropEvaluation::Invalid(_)
        ));
        assert_eq!(
            evaluate_feature_drop(&snapshot, &f1_payload, f4, false),
            DropEvaluation::Valid
        );

        let f2_payload = DragPayload::Feature {
            project_id: pid,
            feature_id: f2,
            parent_feature_id: Some(f1),
            item_key: "DND-2".to_string(),
            title: "Item 2".to_string(),
        };
        assert_eq!(
            evaluate_root_drop(&snapshot, &f2_payload),
            DropEvaluation::Valid
        );
        assert_eq!(
            evaluate_root_drop(&snapshot, &f1_payload),
            DropEvaluation::NoOp
        );

        // Moving WI onto f3 fails INV-05 because WI is blocked by f3; moving onto f1 succeeds.
        let wi_payload = DragPayload::WorkItem {
            project_id: pid,
            work_item_id: wi,
            source_feature_id: Some(f2),
            item_key: "DND-4".to_string(),
            title: "Item 4".to_string(),
        };
        assert!(matches!(
            evaluate_feature_drop(&snapshot, &wi_payload, f3, false),
            DropEvaluation::Invalid(_)
        ));
        assert_eq!(
            evaluate_feature_drop(&snapshot, &wi_payload, f1, true),
            DropEvaluation::Valid
        );
        assert_eq!(
            evaluate_feature_drop(&snapshot, &wi_payload, f2, true),
            DropEvaluation::NoOp
        );
        assert_eq!(
            evaluate_unallocated_drop(&snapshot, &wi_payload),
            DropEvaluation::Valid
        );
    }
}
