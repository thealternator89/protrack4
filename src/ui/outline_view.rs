use std::collections::{HashMap, HashSet};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    StyledExt as _, WindowExt as _, h_flex, notification::Notification, v_flex,
};
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, StatefulInteractiveElement as _,
    Styled as _, Subscription, TestSupportExt as _, Window, div, prelude::FluentBuilder as _, px,
};

use crate::commands::{
    ConvertFeatureToWorkItem, ConvertWorkItemToFeature, CreateFeature, CreateWorkItem,
    DetachWorkItemFromFeature, ReorderFeatureWorkItems, ReorderSiblingFeatures, TrashItemSubtree,
    UndoableCommand, UpdateItemFields,
};
use crate::domain::{
    ConversionEligibility, FeatureProgress, Item, ItemId, ItemKind, ItemStatus, MAX_FEATURE_DEPTH,
    Priority, ProjectSnapshot, clamped_depth_badge, visual_indent_px,
};
use crate::state::{AppState, ProjectStore};
use crate::ui::dnd::{
    DragPayload, DropEvaluation, dispatch_dnd_command, evaluate_feature_drop, handle_feature_drop,
    is_valid_feature_sibling_gap, is_valid_feature_wi_gap, make_drag_ghost, render_drop_gap,
};
use crate::ui::modals::open_create_feature_modal;
use crate::ui::{
    assignee_pill, block_state_badge, blocker_warning_pill, domain_tag_chip, parse_hex_or,
    priority_badge, priority_color, status_badge, status_color, status_icon,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutlineScope {
    #[default]
    AllFeatures,
    SelectedSubtree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutlineRow {
    Feature {
        feature_id: ItemId,
        depth: usize,
        actual_depth: usize,
        has_children: bool,
        is_expanded: bool,
    },
    WorkItem {
        work_item_id: ItemId,
        parent_feature_id: ItemId,
        depth: usize,
        shared_feature_ids: Vec<ItemId>,
    },
    InlineAdd {
        feature_id: ItemId,
        parent_actual_depth: usize,
        depth: usize,
    },
}

fn feature_has_open_descendants(
    snapshot: &ProjectSnapshot,
    feature_id: ItemId,
    visited: &mut HashSet<ItemId>,
) -> bool {
    if !visited.insert(feature_id) {
        return false;
    }
    if snapshot
        .work_items_for_feature(feature_id)
        .into_iter()
        .any(|wi| !wi.status.is_closed())
    {
        return true;
    }
    for child in snapshot.child_features(Some(feature_id)) {
        if !child.status.is_closed() || feature_has_open_descendants(snapshot, child.id, visited) {
            return true;
        }
    }
    false
}

fn is_feature_visible_in_outline(
    snapshot: &ProjectSnapshot,
    feature: &Item,
    hide_closed: bool,
) -> bool {
    if !hide_closed || !feature.status.is_closed() {
        return true;
    }
    let mut visited = HashSet::new();
    feature_has_open_descendants(snapshot, feature.id, &mut visited)
}

pub fn build_outline_rows(
    snapshot: &ProjectSnapshot,
    expanded: &HashSet<ItemId>,
    scope: OutlineScope,
    focused_feature_id: Option<ItemId>,
    hide_closed: bool,
) -> Vec<OutlineRow> {
    let root_features: Vec<&Item> = match scope {
        OutlineScope::SelectedSubtree => {
            if let Some(fid) = focused_feature_id
                && let Some(feat) = snapshot.item(fid)
                && feat.project_id == snapshot.project.id
                && feat.is_active()
                && feat.kind == ItemKind::Feature
            {
                vec![feat]
            } else {
                snapshot.child_features(None)
            }
        }
        OutlineScope::AllFeatures => snapshot.child_features(None),
    };

    let mut rows = Vec::new();
    let mut visited = HashSet::new();
    for feature in root_features {
        collect_outline_feature(
            snapshot,
            feature,
            0,
            expanded,
            hide_closed,
            &mut visited,
            &mut rows,
        );
    }
    rows
}

fn collect_outline_feature(
    snapshot: &ProjectSnapshot,
    feature: &Item,
    depth: usize,
    expanded: &HashSet<ItemId>,
    hide_closed: bool,
    visited: &mut HashSet<ItemId>,
    out: &mut Vec<OutlineRow>,
) {
    if depth >= MAX_FEATURE_DEPTH || !visited.insert(feature.id) {
        return;
    }
    if !is_feature_visible_in_outline(snapshot, feature, hide_closed) {
        return;
    }

    let visible_wis: Vec<&Item> = snapshot
        .work_items_for_feature(feature.id)
        .into_iter()
        .filter(|wi| !hide_closed || !wi.status.is_closed())
        .collect();

    let visible_children: Vec<&Item> = snapshot
        .child_features(Some(feature.id))
        .into_iter()
        .filter(|child| is_feature_visible_in_outline(snapshot, child, hide_closed))
        .collect();

    let has_children = !visible_wis.is_empty() || !visible_children.is_empty();
    let is_expanded = expanded.contains(&feature.id);
    let actual_depth = snapshot.feature_depth(feature.id);

    out.push(OutlineRow::Feature {
        feature_id: feature.id,
        depth,
        actual_depth,
        has_children,
        is_expanded,
    });

    if !is_expanded {
        return;
    }

    for wi in visible_wis {
        let shared_feature_ids = snapshot
            .features_for_work_item(wi.id)
            .into_iter()
            .map(|f| f.id)
            .collect();
        out.push(OutlineRow::WorkItem {
            work_item_id: wi.id,
            parent_feature_id: feature.id,
            depth: depth + 1,
            shared_feature_ids,
        });
    }

    for child in visible_children {
        collect_outline_feature(
            snapshot,
            child,
            depth + 1,
            expanded,
            hide_closed,
            visited,
            out,
        );
    }

    out.push(OutlineRow::InlineAdd {
        feature_id: feature.id,
        parent_actual_depth: actual_depth,
        depth: depth + 1,
    });
}

fn feature_in_subtree(snapshot: &ProjectSnapshot, root_id: ItemId, target_id: ItemId) -> bool {
    root_id == target_id || snapshot.descendant_feature_ids(root_id).contains(&target_id)
}

pub struct OutlineView {
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    scope: OutlineScope,
    hide_closed: bool,
    inline_add_feature_id: Option<ItemId>,
    inline_add_kind: ItemKind,
    inline_add_input: Entity<InputState>,
    shared_popover_key: Option<(ItemId, ItemId)>,
    _subscriptions: Vec<Subscription>,
}

impl OutlineView {
    pub fn new(
        app_state: Entity<AppState>,
        store: Entity<ProjectStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let inline_add_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("New Work Item title…"));

        let store_sub = cx.observe(&store, |this, store, cx| {
            let st = store.read(cx);
            let snap_opt = st.snapshot();

            if let Some(fid) = this.inline_add_feature_id {
                let valid = snap_opt.and_then(|s| s.item(fid)).is_some_and(|item| {
                    item.is_active() && item.kind == ItemKind::Feature
                });
                if !valid {
                    this.inline_add_feature_id = None;
                }
            }

            if let Some((pfid, wid)) = this.shared_popover_key {
                let valid = snap_opt.is_some_and(|s| {
                    s.item(pfid).is_some_and(|f| f.is_active())
                        && s.item(wid).is_some_and(|w| w.is_active())
                        && s.features_for_work_item(wid).len() > 1
                });
                if !valid {
                    this.shared_popover_key = None;
                }
            }

            if this.scope == OutlineScope::SelectedSubtree && st.focused_feature_id().is_none() {
                this.scope = OutlineScope::AllFeatures;
            }
            cx.notify();
        });

        let input_sub = cx.subscribe_in(
            &inline_add_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.submit_inline_add(window, cx);
                }
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );

        Self {
            app_state,
            store,
            scope: OutlineScope::AllFeatures,
            hide_closed: false,
            inline_add_feature_id: None,
            inline_add_kind: ItemKind::WorkItem,
            inline_add_input,
            shared_popover_key: None,
            _subscriptions: vec![store_sub, input_sub],
        }
    }

    pub fn scope(&self) -> OutlineScope {
        self.scope
    }

    pub fn set_scope(&mut self, scope: OutlineScope, cx: &mut Context<Self>) {
        if scope == OutlineScope::SelectedSubtree
            && self.store.read(cx).focused_feature_id().is_none()
        {
            return;
        }
        self.scope = scope;
        cx.notify();
    }

    pub fn hide_closed(&self) -> bool {
        self.hide_closed
    }

    pub fn set_hide_closed(&mut self, hide_closed: bool, cx: &mut Context<Self>) {
        self.hide_closed = hide_closed;
        cx.notify();
    }

    pub fn inline_add_input(&self) -> &Entity<InputState> {
        &self.inline_add_input
    }

    pub fn inline_add_feature_id(&self) -> Option<ItemId> {
        self.inline_add_feature_id
    }

    pub fn inline_add_kind(&self) -> ItemKind {
        self.inline_add_kind
    }

    pub fn start_inline_add(
        &mut self,
        feature_id: ItemId,
        kind: ItemKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self.store.read(cx).snapshot() else {
            return;
        };
        let Some(feature) = snapshot.item(feature_id) else {
            return;
        };
        if !feature.is_active() || feature.kind != ItemKind::Feature {
            return;
        }
        let actual_depth = snapshot.feature_depth(feature_id);
        let effective_kind = if kind == ItemKind::Feature && actual_depth + 1 >= MAX_FEATURE_DEPTH {
            ItemKind::WorkItem
        } else {
            kind
        };

        self.store.update(cx, |s, cx| {
            s.expand_feature(feature_id);
            cx.notify();
        });

        self.inline_add_feature_id = Some(feature_id);
        self.inline_add_kind = effective_kind;
        let placeholder = match effective_kind {
            ItemKind::WorkItem => "New Work Item title…",
            ItemKind::Feature => "New Sub-Feature title…",
        };
        self.inline_add_input.update(cx, |inp, cx| {
            inp.set_placeholder(placeholder, window, cx);
            inp.set_value("", window, cx);
            inp.focus(window, cx);
        });
        cx.notify();
    }

    pub fn set_inline_add_kind(
        &mut self,
        kind: ItemKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(fid) = self.inline_add_feature_id
            && kind == ItemKind::Feature
            && let Some(snapshot) = self.store.read(cx).snapshot()
            && snapshot.feature_depth(fid) + 1 >= MAX_FEATURE_DEPTH
        {
            return;
        }
        self.inline_add_kind = kind;
        let placeholder = match kind {
            ItemKind::WorkItem => "New Work Item title…",
            ItemKind::Feature => "New Sub-Feature title…",
        };
        self.inline_add_input.update(cx, |inp, cx| {
            inp.set_placeholder(placeholder, window, cx);
            inp.focus(window, cx);
        });
        cx.notify();
    }

    pub fn cancel_inline_add(&mut self, cx: &mut Context<Self>) {
        if self.inline_add_feature_id.take().is_some() {
            cx.notify();
        }
    }

    fn submit_inline_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(feature_id) = self.inline_add_feature_id else {
            return;
        };
        let title = self.inline_add_input.read(cx).value().trim().to_string();
        if title.is_empty() {
            return;
        }
        let Some(project_id) = self.store.read(cx).snapshot().map(|s| s.project.id) else {
            return;
        };

        let kind = self.inline_add_kind;
        self.inline_add_input.update(cx, |inp, cx| {
            inp.set_value("", window, cx);
            inp.focus(window, cx);
        });
        cx.notify();

        let command: Box<dyn UndoableCommand> = match kind {
            ItemKind::WorkItem => Box::new(CreateWorkItem::new(
                project_id,
                title,
                String::new(),
                ItemStatus::Todo,
                Priority::None,
                vec![feature_id],
            )),
            ItemKind::Feature => Box::new(CreateFeature::new(
                project_id,
                Some(feature_id),
                title,
                String::new(),
                ItemStatus::Todo,
                Priority::None,
            )),
        };

        let store = self.store.clone();
        let task = self
            .app_state
            .update(cx, |app, cx| app.execute_command(command, cx));

        window
            .spawn(cx, async move |cx| {
                let res = task.await;
                let _ = cx.update(|window, cx| match res {
                    Ok(effect) => {
                        store.update(cx, |s, cx| {
                            s.expand_feature(feature_id);
                            cx.notify();
                        });
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

    fn dispatch_command(
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

    #[allow(clippy::too_many_arguments)]
    fn build_item_context_menu(
        mut menu: PopupMenu,
        item: Item,
        parent_feature: Item,
        parent_actual_depth: usize,
        snapshot: ProjectSnapshot,
        app_state: Entity<AppState>,
        store: Entity<ProjectStore>,
        outline_view: Entity<OutlineView>,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let item_id = item.id;
        let is_feature = item.kind == ItemKind::Feature;
        let header = format!("{} · {}", item.item_key, item.kind.label());

        menu = menu.min_w(px(210.)).label(header);

        // Status submenu
        let current_status = item.status;
        let app_status = app_state.clone();
        menu = menu.submenu("Status", window, cx, move |mut sub, _, _| {
            for status in [
                ItemStatus::Backlog,
                ItemStatus::Todo,
                ItemStatus::InProgress,
                ItemStatus::Done,
                ItemStatus::Cancelled,
            ] {
                let app_click = app_status.clone();
                sub = sub.item(
                    PopupMenuItem::new(status.label())
                        .icon(status_icon(status))
                        .checked(current_status == status)
                        .on_click(move |_, window, cx| {
                            OutlineView::dispatch_command(
                                app_click.clone(),
                                Box::new(UpdateItemFields {
                                    item_id,
                                    title: None,
                                    description: None,
                                    status: Some(status),
                                    priority: None,
                                    assignee_id: None,
                                }),
                                window,
                                cx,
                            );
                        }),
                );
            }
            sub
        });

        // Priority submenu
        let current_priority = item.priority;
        let app_priority = app_state.clone();
        menu = menu.submenu("Priority", window, cx, move |mut sub, _, _| {
            for priority in [
                Priority::None,
                Priority::Low,
                Priority::Medium,
                Priority::High,
                Priority::Urgent,
            ] {
                let app_click = app_priority.clone();
                sub = sub.item(
                    PopupMenuItem::element(move |_, cx| {
                        let color = priority_color(priority, cx);
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().size(px(7.)).rounded(px(999.)).bg(color))
                            .child(div().text_xs().child(priority.label()))
                    })
                    .checked(current_priority == priority)
                    .on_click(move |_, window, cx| {
                        OutlineView::dispatch_command(
                            app_click.clone(),
                            Box::new(UpdateItemFields {
                                item_id,
                                title: None,
                                description: None,
                                status: None,
                                priority: Some(priority),
                                assignee_id: None,
                            }),
                            window,
                            cx,
                        );
                    }),
                );
            }
            sub
        });

        // Assignee submenu
        let current_assignee_id = item.assignee_id;
        let selectable_assignees: Vec<_> = snapshot
            .selectable_assignees(current_assignee_id)
            .into_iter()
            .cloned()
            .collect();
        let app_assign = app_state.clone();
        menu = menu.submenu("Assign", window, cx, move |mut sub, _, _| {
            let app_unassign = app_assign.clone();
            sub = sub.item(
                PopupMenuItem::new("Unassigned")
                    .checked(current_assignee_id.is_none())
                    .on_click(move |_, window, cx| {
                        OutlineView::dispatch_command(
                            app_unassign.clone(),
                            Box::new(UpdateItemFields {
                                item_id,
                                title: None,
                                description: None,
                                status: None,
                                priority: None,
                                assignee_id: Some(None),
                            }),
                            window,
                            cx,
                        );
                    }),
            );

            if !selectable_assignees.is_empty() {
                sub = sub.separator();
            }

            for assignee in &selectable_assignees {
                let aid = assignee.id;
                let checked = current_assignee_id == Some(aid);
                let handle = assignee.handle.clone();
                let name = assignee.name.clone();
                let color_hex = assignee.color.clone();
                let archived = assignee.is_archived();
                let app_pick = app_assign.clone();

                sub = sub.item(
                    PopupMenuItem::element(move |_, cx| {
                        let color = parse_hex_or(&color_hex, cx.theme().primary);
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .px_1()
                                    .rounded(px(999.))
                                    .bg(color.opacity(0.16))
                                    .text_size(px(10.))
                                    .font_semibold()
                                    .text_color(color)
                                    .child(handle.clone()),
                            )
                            .child(div().text_xs().child(if archived {
                                format!("{name} (Archived)")
                            } else {
                                name.clone()
                            }))
                    })
                    .checked(checked)
                    .disabled(archived && !checked)
                    .on_click(move |_, window, cx| {
                        OutlineView::dispatch_command(
                            app_pick.clone(),
                            Box::new(UpdateItemFields {
                                item_id,
                                title: None,
                                description: None,
                                status: None,
                                priority: None,
                                assignee_id: Some(Some(aid)),
                            }),
                            window,
                            cx,
                        );
                    }),
                );
            }
            sub
        });

        let target_feature_id = if is_feature {
            item.id
        } else {
            parent_feature.id
        };
        let target_depth = if is_feature {
            snapshot.feature_depth(item.id)
        } else {
            parent_actual_depth
        };
        let at_max_depth = target_depth + 1 >= MAX_FEATURE_DEPTH;

        let view_add_wi = outline_view.clone();
        let view_add_feat = outline_view.clone();

        menu = menu
            .separator()
            .item(
                PopupMenuItem::new("Add Work Item")
                    .icon(IconName::Plus)
                    .on_click(move |_, window, cx| {
                        view_add_wi.update(cx, |this, cx| {
                            this.start_inline_add(target_feature_id, ItemKind::WorkItem, window, cx);
                        });
                    }),
            )
            .item(
                PopupMenuItem::new("Add Sub-Feature")
                    .icon(IconName::Folder)
                    .disabled(at_max_depth)
                    .on_click(move |_, window, cx| {
                        view_add_feat.update(cx, |this, cx| {
                            this.start_inline_add(target_feature_id, ItemKind::Feature, window, cx);
                        });
                    }),
            );

        let can_convert = snapshot
            .conversion_eligibility(item_id)
            .as_ref()
            .is_some_and(ConversionEligibility::is_allowed);
        let convert_label = if is_feature {
            "Convert to Work Item"
        } else {
            "Convert to Feature"
        };
        let app_convert = app_state.clone();
        let app_trash = app_state.clone();

        if is_feature {
            let store_focus = store;
            let view_focus = outline_view;
            menu = menu
                .item(
                    PopupMenuItem::new("Focus Subtree")
                        .icon(IconName::FolderOpen)
                        .on_click(move |_, _, cx| {
                            store_focus.update(cx, |s, cx| {
                                s.select_item(Some(item_id));
                                s.set_focused_feature(Some(item_id));
                                s.expand_feature(item_id);
                                cx.notify();
                            });
                            view_focus.update(cx, |this, cx| {
                                this.set_scope(OutlineScope::SelectedSubtree, cx);
                            });
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new(convert_label)
                        .icon(IconName::Replace)
                        .disabled(!can_convert)
                        .on_click(move |_, window, cx| {
                            OutlineView::dispatch_command(
                                app_convert.clone(),
                                Box::new(ConvertFeatureToWorkItem::new(item_id)),
                                window,
                                cx,
                            );
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Move to Trash")
                        .icon(IconName::Delete)
                        .on_click(move |_, window, cx| {
                            OutlineView::dispatch_command(
                                app_trash.clone(),
                                Box::new(TrashItemSubtree::new(item_id)),
                                window,
                                cx,
                            );
                        }),
                );
        } else {
            let pfid = parent_feature.id;
            let pkey = parent_feature.item_key.clone();
            let app_detach = app_state;
            menu = menu
                .separator()
                .item(
                    PopupMenuItem::new(convert_label)
                        .icon(IconName::Replace)
                        .disabled(!can_convert)
                        .on_click(move |_, window, cx| {
                            OutlineView::dispatch_command(
                                app_convert.clone(),
                                Box::new(ConvertWorkItemToFeature::new(item_id)),
                                window,
                                cx,
                            );
                        }),
                )
                .item(
                    PopupMenuItem::new(format!("Detach from {pkey}"))
                        .icon(IconName::Close)
                        .on_click(move |_, window, cx| {
                            OutlineView::dispatch_command(
                                app_detach.clone(),
                                Box::new(DetachWorkItemFromFeature::new(item_id, pfid)),
                                window,
                                cx,
                            );
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Move to Trash")
                        .icon(IconName::Delete)
                        .on_click(move |_, window, cx| {
                            OutlineView::dispatch_command(
                                app_trash.clone(),
                                Box::new(TrashItemSubtree::new(item_id)),
                                window,
                                cx,
                            );
                        }),
                );
        }

        menu
    }

    fn render_toolbar(
        &self,
        snapshot: &ProjectSnapshot,
        focused_feature: Option<&Item>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_subtree = self.scope == OutlineScope::SelectedSubtree && focused_feature.is_some();

        let breadcrumb = focused_feature.map(|feat| {
            let path = snapshot.feature_path(feat.id);
            if path.is_empty() {
                format!("{} {}", feat.item_key, feat.title)
            } else {
                path.into_iter()
                    .map(|f| format!("{} {}", f.item_key, f.title))
                    .collect::<Vec<_>>()
                    .join(" › ")
            }
        });

        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Button::new("outline-scope-all")
                            .outline()
                            .xsmall()
                            .selected(!is_subtree)
                            .label("All Features")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_scope(OutlineScope::AllFeatures, cx);
                            })),
                    )
                    .child(
                        Button::new("outline-scope-subtree")
                            .outline()
                            .xsmall()
                            .selected(is_subtree)
                            .disabled(focused_feature.is_none())
                            .label("Selected Subtree")
                            .tooltip(if focused_feature.is_some() {
                                "Scope Outline to the selected Feature subtree"
                            } else {
                                "Select a Feature in the Sidebar or Outline first"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_scope(OutlineScope::SelectedSubtree, cx);
                            })),
                    )
                    .when(is_subtree, |this| {
                        this.when_some(breadcrumb, |this, crumb| {
                            this.child(
                                h_flex()
                                    .min_w_0()
                                    .max_w(px(360.))
                                    .px_2()
                                    .py_0p5()
                                    .gap_1()
                                    .items_center()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().muted)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(crumb),
                                    )
                                    .child(
                                        Button::new("outline-scope-clear")
                                            .ghost()
                                            .xsmall()
                                            .compact()
                                            .icon(IconName::Close)
                                            .tooltip("Show All Features")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.set_scope(OutlineScope::AllFeatures, cx);
                                            })),
                                    ),
                            )
                        })
                    }),
            )
            .child(
                h_flex()
                    .gap_1p5()
                    .flex_shrink_0()
                    .items_center()
                    .child(
                        Button::new("outline-toggle-hide-closed")
                            .outline()
                            .xsmall()
                            .selected(self.hide_closed)
                            .icon(IconName::EyeOff)
                            .label("Hide Done/Cancelled")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_hide_closed(!this.hide_closed, cx);
                            })),
                    )
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
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_feature_row(
        &self,
        snapshot: &ProjectSnapshot,
        feature: &Item,
        depth: usize,
        actual_depth: usize,
        has_children: bool,
        is_expanded: bool,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let feat_id = feature.id;
        let feat_num = feature.item_number as usize;
        let progress: FeatureProgress = snapshot.feature_progress_detail(feat_id);
        let depth_badge = clamped_depth_badge(actual_depth);
        let tags = snapshot.tags_for_item(feat_id);
        let assignee = feature.assignee_id.and_then(|aid| snapshot.assignee(aid));
        let block_state = snapshot.effective_block_state(feat_id);
        let primary_blocker_key = snapshot.primary_active_direct_blocker_key(feat_id);
        let block_badge = block_state_badge(block_state, primary_blocker_key, cx);
        let first_warning = if !block_state.has_any_block() {
            snapshot.blocker_warnings(feat_id).first()
        } else {
            None
        };
        let bar_color = if progress.is_complete() {
            cx.theme().success
        } else if progress.is_empty() {
            cx.theme().muted_foreground.opacity(0.35)
        } else {
            cx.theme().primary
        };
        let progress_label_color = if progress.is_complete() {
            cx.theme().success
        } else {
            cx.theme().muted_foreground
        };

        let chevron_icon = if is_expanded {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };

        let item_clone = feature.clone();
        let snapshot_clone = snapshot.clone();
        let snapshot_for_over = snapshot.clone();
        let app_state = self.app_state.clone();
        let app_for_drop = self.app_state.clone();
        let store_for_menu = self.store.clone();
        let store_for_drop = self.store.clone();
        let view_entity = cx.entity();

        let drag_payload = DragPayload::Feature {
            project_id: feature.project_id,
            feature_id: feat_id,
            parent_feature_id: feature.parent_feature_id,
            item_key: feature.item_key.clone(),
            title: feature.title.clone(),
        };

        h_flex()
            .id(("outline-feature", feat_num))
            .test_support()
            .w_full()
            .pl(px(10.0 + visual_indent_px(depth)))
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
            .on_drag(drag_payload, make_drag_ghost)
            .drag_over::<DragPayload>(move |style, payload, window, cx| {
                let alt_held = window.modifiers().alt;
                match evaluate_feature_drop(&snapshot_for_over, payload, feat_id, alt_held) {
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
                            .bg(accent.opacity(if cx.theme().is_dark() { 0.22 } else { 0.14 }))
                            .border_color(accent)
                    }
                    DropEvaluation::Invalid(_) => {
                        let danger = cx.theme().danger;
                        style
                            .bg(danger.opacity(if cx.theme().is_dark() { 0.20 } else { 0.12 }))
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
                    feat_id,
                    alt_held,
                    window,
                    cx,
                );
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
            .child(
                div()
                    .id(("outline-chevron", feat_num))
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
                    .child(Icon::new(chevron_icon).xsmall().text_color(if has_children {
                        cx.theme().muted_foreground
                    } else {
                        cx.theme().muted_foreground.opacity(0.45)
                    })),
            )
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
                    .when(feature.status.is_closed(), |this| {
                        this.text_color(cx.theme().muted_foreground)
                    })
                    .child(feature.title.clone()),
            )
            .when_some(depth_badge, |this, badge| {
                this.child(Tag::secondary().xsmall().child(badge))
            })
            .when_some(block_badge, |this, badge| {
                this.child(
                    div()
                        .id(("outline-feature-block", feat_num))
                        .test_support()
                        .flex_shrink_0()
                        .child(badge),
                )
            })
            .when_some(first_warning, |this, warning| {
                this.child(
                    div()
                        .id(("outline-feature-block-warn", feat_num))
                        .test_support()
                        .flex_shrink_0()
                        .child(blocker_warning_pill(warning, true, cx)),
                )
            })
            .when(!tags.is_empty(), |this| {
                this.child(
                    h_flex()
                        .gap_1()
                        .flex_shrink_0()
                        .children(tags.into_iter().map(|tag| domain_tag_chip(tag, cx))),
                )
            })
            .child(
                h_flex()
                    .gap_1p5()
                    .flex_shrink_0()
                    .items_center()
                    .child(
                        div().w(px(64.)).child(
                            Progress::new(("outline-progress", feat_num))
                                .xsmall()
                                .value(progress.progress_bar_value())
                                .color(bar_color)
                                .accessibility_label(format!(
                                    "{} progress: {}",
                                    feature.item_key,
                                    progress.detailed_label()
                                )),
                        ),
                    )
                    .child(
                        div()
                            .id(("outline-progress-label", feat_num))
                            .test_support()
                            .min_w(px(64.))
                            .text_right()
                            .text_size(px(11.))
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(progress_label_color)
                            .child(progress.detailed_label()),
                    ),
            )
            .child(priority_badge(feature.priority, cx))
            .when_some(assignee, |this, a| {
                this.child(assignee_pill(a, false, cx))
            })
            .child(status_badge(feature.status, cx))
            .context_menu(move |menu, window, cx| {
                Self::build_item_context_menu(
                    menu,
                    item_clone.clone(),
                    item_clone.clone(),
                    actual_depth,
                    snapshot_clone.clone(),
                    app_state.clone(),
                    store_for_menu.clone(),
                    view_entity.clone(),
                    window,
                    cx,
                )
            })
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_work_item_row(
        &self,
        snapshot: &ProjectSnapshot,
        wi: &Item,
        parent_feature: &Item,
        parent_actual_depth: usize,
        depth: usize,
        shared_feature_ids: &[ItemId],
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wi_id = wi.id;
        let parent_feature_id = parent_feature.id;
        let row_key = (parent_feature.item_number as usize) * 100_000 + (wi.item_number as usize);
        let wi_tags = snapshot.tags_for_item(wi_id);
        let wi_assignee = wi.assignee_id.and_then(|aid| snapshot.assignee(aid));
        let is_shared = shared_feature_ids.len() > 1;
        let popover_open = self.shared_popover_key == Some((parent_feature_id, wi_id));
        let block_state = snapshot.effective_block_state(wi_id);
        let primary_blocker_key = snapshot.primary_active_direct_blocker_key(wi_id);
        let block_badge = block_state_badge(block_state, primary_blocker_key, cx);
        let first_warning = if !block_state.has_any_block() {
            snapshot.blocker_warnings(wi_id).first()
        } else {
            None
        };

        let shared_label = if shared_feature_ids.len() > 1 {
            let extra = shared_feature_ids.len() - 1;
            if extra == 1 {
                "Shared: +1 feature".to_string()
            } else {
                format!("Shared: +{extra} features")
            }
        } else {
            String::new()
        };

        let view_for_change = cx.entity();
        let view_for_content = cx.entity();
        let shared_ids_owned = shared_feature_ids.to_vec();
        let snapshot_for_popover = snapshot.clone();
        let store_for_popover = self.store.clone();

        let item_clone = wi.clone();
        let parent_clone = parent_feature.clone();
        let snapshot_clone = snapshot.clone();
        let app_state = self.app_state.clone();
        let store_for_menu = self.store.clone();
        let view_for_menu = cx.entity();

        let drag_payload = DragPayload::WorkItem {
            project_id: wi.project_id,
            work_item_id: wi_id,
            source_feature_id: Some(parent_feature_id),
            item_key: wi.item_key.clone(),
            title: wi.title.clone(),
        };

        h_flex()
            .id(("outline-wi", row_key))
            .test_support()
            .w_full()
            .pl(px(10.0 + visual_indent_px(depth)))
            .pr_3()
            .py_1()
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
            .on_drag(drag_payload, make_drag_ghost)
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
                    .when(wi.status.is_closed(), |this| {
                        this.text_color(cx.theme().muted_foreground)
                    })
                    .when(wi.status.is_cancelled(), |this| this.line_through())
                    .child(wi.title.clone()),
            )
            .when_some(block_badge, |this, badge| {
                this.child(
                    div()
                        .id(("outline-wi-block", row_key))
                        .test_support()
                        .flex_shrink_0()
                        .child(badge),
                )
            })
            .when_some(first_warning, |this, warning| {
                this.child(
                    div()
                        .id(("outline-wi-block-warn", row_key))
                        .test_support()
                        .flex_shrink_0()
                        .child(blocker_warning_pill(warning, true, cx)),
                )
            })
            .when(is_shared, |this| {
                let info = cx.theme().info;
                let shared_count = shared_ids_owned.len();

                this.child(
                    Popover::new(("outline-shared-popover", row_key))
                        .anchor(Anchor::TopRight)
                        .open(popover_open)
                        .on_open_change(move |is_open, _, cx| {
                            view_for_change.update(cx, |this, cx| {
                                if *is_open {
                                    this.shared_popover_key = Some((parent_feature_id, wi_id));
                                } else if this.shared_popover_key
                                    == Some((parent_feature_id, wi_id))
                                {
                                    this.shared_popover_key = None;
                                }
                                cx.notify();
                            });
                        })
                        .trigger(
                            Button::new(("outline-shared-btn", row_key))
                                .outline()
                                .xsmall()
                                .label(shared_label)
                                .text_color(info)
                                .tooltip("View all attached Features & jump"),
                        )
                        .content(move |_, _, cx| {
                            let popover_state = cx.entity();
                            let snap = &snapshot_for_popover;
                            let items: Vec<(ItemId, String, String, bool)> = shared_ids_owned
                                .iter()
                                .filter_map(|fid| {
                                    let f = snap.item(*fid)?;
                                    let path = snap.feature_path(*fid);
                                    let path_str = if path.is_empty() {
                                        f.title.clone()
                                    } else {
                                        path.into_iter()
                                            .map(|p| p.title.clone())
                                            .collect::<Vec<_>>()
                                            .join(" › ")
                                    };
                                    Some((
                                        *fid,
                                        f.item_key.clone(),
                                        path_str,
                                        *fid == parent_feature_id,
                                    ))
                                })
                                .collect();

                            v_flex()
                                .w(px(300.))
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_semibold()
                                        .child(format!("Attached Features ({shared_count})")),
                                )
                                .child(
                                    v_flex()
                                        .w_full()
                                        .max_h(px(200.))
                                        .overflow_y_scrollbar()
                                        .id(("outline-shared-list", row_key))
                                        .gap_0p5()
                                        .children(items.into_iter().enumerate().map(
                                            |(idx, (fid, key, path_str, is_current))| {
                                                let store_jump = store_for_popover.clone();
                                                let view_jump = view_for_content.clone();
                                                let snap_jump = snapshot_for_popover.clone();
                                                let popover_dismiss = popover_state.clone();

                                                ListItem::new((
                                                    "outline-shared-jump-item",
                                                    row_key * 100 + idx,
                                                ))
                                                .w_full()
                                                .px_2()
                                                .py_1p5()
                                                .rounded(cx.theme().radius * 0.75)
                                                .selected(is_current)
                                                .on_click(move |_, window, cx| {
                                                    store_jump.update(cx, |s, cx| {
                                                        s.expand_ancestors_of(fid);
                                                        s.expand_feature(fid);
                                                        s.select_item(Some(fid));
                                                        s.set_focused_feature(Some(fid));
                                                        cx.notify();
                                                    });
                                                    view_jump.update(cx, |this, cx| {
                                                        if this.scope
                                                            == OutlineScope::SelectedSubtree
                                                            && let Some(focused) = this
                                                                .store
                                                                .read(cx)
                                                                .focused_feature_id()
                                                            && !feature_in_subtree(
                                                                &snap_jump,
                                                                focused,
                                                                fid,
                                                            )
                                                        {
                                                            this.store.update(cx, |s, _| {
                                                                s.set_focused_feature(Some(fid));
                                                            });
                                                        }
                                                        this.shared_popover_key = None;
                                                        cx.notify();
                                                    });
                                                    popover_dismiss.update(cx, |st, cx| {
                                                        st.dismiss(window, cx);
                                                    });
                                                })
                                                .child(
                                                    h_flex()
                                                        .id((
                                                            "outline-shared-jump",
                                                            row_key * 100 + idx,
                                                        ))
                                                        .test_support()
                                                        .w_full()
                                                        .gap_2()
                                                        .items_center()
                                                        .child(
                                                            Icon::new(IconName::Folder)
                                                                .xsmall()
                                                                .text_color(cx.theme().primary),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .font_family(
                                                                    cx.theme()
                                                                        .mono_font_family
                                                                        .clone(),
                                                                )
                                                                .text_color(
                                                                    cx.theme().muted_foreground,
                                                                )
                                                                .child(key),
                                                        )
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .overflow_hidden()
                                                                .whitespace_nowrap()
                                                                .text_ellipsis()
                                                                .text_xs()
                                                                .child(path_str),
                                                        )
                                                        .when(is_current, |this| {
                                                            this.child(
                                                                Tag::secondary()
                                                                    .xsmall()
                                                                    .child("current"),
                                                            )
                                                        }),
                                                )
                                            },
                                        )),
                                )
                        }),
                )
            })
            .when(!wi_tags.is_empty(), |this| {
                this.child(
                    h_flex()
                        .gap_1()
                        .flex_shrink_0()
                        .children(wi_tags.into_iter().map(|tag| domain_tag_chip(tag, cx))),
                )
            })
            .child(priority_badge(wi.priority, cx))
            .when_some(wi_assignee, |this, a| {
                this.child(assignee_pill(a, false, cx))
            })
            .child(status_badge(wi.status, cx))
            .context_menu(move |menu, window, cx| {
                Self::build_item_context_menu(
                    menu,
                    item_clone.clone(),
                    parent_clone.clone(),
                    parent_actual_depth,
                    snapshot_clone.clone(),
                    app_state.clone(),
                    store_for_menu.clone(),
                    view_for_menu.clone(),
                    window,
                    cx,
                )
            })
            .into_any_element()
    }

    fn render_inline_add_row(
        &self,
        feature: &Item,
        parent_actual_depth: usize,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let feat_id = feature.id;
        let feat_num = feature.item_number as usize;
        let is_active = self.inline_add_feature_id == Some(feat_id);
        let at_max_depth = parent_actual_depth + 1 >= MAX_FEATURE_DEPTH;
        let indent = px(10.0 + visual_indent_px(depth));

        if !is_active {
            return h_flex()
                .id(("outline-inline-trigger", feat_num))
                .test_support()
                .w_full()
                .pl(indent)
                .pr_3()
                .py_1()
                .gap_2()
                .items_center()
                .rounded(cx.theme().radius)
                .cursor_pointer()
                .hover(|s| s.bg(cx.theme().list_hover))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.start_inline_add(feat_id, ItemKind::WorkItem, window, cx);
                }))
                .child(div().w(px(18.)).flex_shrink_0())
                .child(
                    Icon::new(IconName::Plus)
                        .xsmall()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Add Work Item or Sub-Feature to {}…",
                            feature.item_key
                        )),
                )
                .into_any_element();
        }

        let blank = self.inline_add_input.read(cx).value().trim().is_empty();
        let kind = self.inline_add_kind;

        h_flex()
            .id(("outline-inline-editor", feat_num))
            .w_full()
            .pl(indent)
            .pr_3()
            .py_1p5()
            .gap_1p5()
            .items_center()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().primary.opacity(0.45))
            .bg(cx.theme().accent.opacity(0.45))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel_inline_add(cx);
                    cx.stop_propagation();
                }
            }))
            .child(div().w(px(18.)).flex_shrink_0())
            .child(
                Button::new("outline-inline-kind-wi")
                    .outline()
                    .xsmall()
                    .selected(kind == ItemKind::WorkItem)
                    .label("Work Item")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_inline_add_kind(ItemKind::WorkItem, window, cx);
                    })),
            )
            .child(
                Button::new("outline-inline-kind-feature")
                    .outline()
                    .xsmall()
                    .selected(kind == ItemKind::Feature)
                    .disabled(at_max_depth)
                    .label("Sub-Feature")
                    .tooltip(if at_max_depth {
                        "Maximum Feature nesting depth (32) reached"
                    } else {
                        "Create a child Sub-Feature"
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_inline_add_kind(ItemKind::Feature, window, cx);
                    })),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.inline_add_input)
                        .id("outline-inline-input")
                        .small(),
                ),
            )
            .child(
                Button::new("outline-inline-submit")
                    .primary()
                    .xsmall()
                    .label("Add")
                    .disabled(blank)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.submit_inline_add(window, cx);
                    })),
            )
            .child(
                Button::new("outline-inline-cancel")
                    .ghost()
                    .xsmall()
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cancel_inline_add(cx);
                    })),
            )
            .into_any_element()
    }
}

impl Render for OutlineView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (snapshot, selected_item_id, focused_feature_id, expanded) = {
            let state = self.store.read(cx);
            (
                state.snapshot().cloned(),
                state.selected_item_id(),
                state.focused_feature_id(),
                state.expanded_features().clone(),
            )
        };

        let Some(snapshot) = snapshot else {
            return v_flex().flex_1().into_any_element();
        };

        let focused_feature = focused_feature_id
            .and_then(|fid| snapshot.item(fid))
            .filter(|f| f.is_active() && f.kind == ItemKind::Feature);

        let all_project_features_empty = snapshot.child_features(None).is_empty();
        let rows = build_outline_rows(
            &snapshot,
            &expanded,
            self.scope,
            focused_feature_id,
            self.hide_closed,
        );

        let toolbar = self.render_toolbar(&snapshot, focused_feature, cx);

        let body = if all_project_features_empty {
            let pid = snapshot.project.id;
            let app_state = self.app_state.clone();
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
                        .child("No Features in this Project yet"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Create a root Feature to organize work items into a nested outline."),
                )
                .child(
                    Button::new("outline-empty-add-feature")
                        .primary()
                        .small()
                        .icon(IconName::Plus)
                        .label("New Root Feature")
                        .on_click(move |_, window, cx| {
                            open_create_feature_modal(app_state.clone(), pid, None, window, cx);
                        }),
                )
                .into_any_element()
        } else if rows.is_empty() {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .p_8()
                .gap_2()
                .child(
                    Icon::new(IconName::EyeOff)
                        .large()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .child("All matching items are Done or Cancelled"),
                )
                .child(
                    Button::new("outline-empty-show-closed")
                        .outline()
                        .small()
                        .label("Show Done/Cancelled")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_hide_closed(false, cx);
                        })),
                )
                .into_any_element()
        } else {
            let mut list = v_flex()
                .flex_1()
                .min_h_0()
                .p_3()
                .gap_0()
                .overflow_y_scrollbar()
                .id("center-outline-scroll");

            let project_id = snapshot.project.id;
            let mut last_visible_child_feature: HashMap<Option<ItemId>, (ItemId, usize)> =
                HashMap::new();

            for idx in 0..rows.len() {
                match rows[idx] {
                    OutlineRow::Feature {
                        feature_id,
                        depth,
                        actual_depth,
                        has_children,
                        is_expanded,
                    } => {
                        let Some(feature) = snapshot.item(feature_id) else {
                            continue;
                        };
                        let feat_num = feature.item_number as usize;
                        let parent_id = feature.parent_feature_id;
                        let siblings = snapshot.child_features(parent_id);
                        let prev_sib_id = siblings
                            .iter()
                            .position(|s| s.id == feature_id)
                            .and_then(|pos| pos.checked_sub(1).map(|p| siblings[p].id));

                        let app_before = self.app_state.clone();
                        list = list.child(render_drop_gap(
                            ("outline-feature-gap-before", feat_num),
                            10.0 + visual_indent_px(depth),
                            move |payload| {
                                is_valid_feature_sibling_gap(
                                    payload,
                                    project_id,
                                    parent_id,
                                    prev_sib_id,
                                    Some(feature_id),
                                )
                            },
                            move |payload, window, cx| {
                                if let DragPayload::Feature {
                                    feature_id: moving_id,
                                    ..
                                } = payload
                                {
                                    dispatch_dnd_command(
                                        app_before.clone(),
                                        Box::new(ReorderSiblingFeatures::new(
                                            project_id,
                                            *moving_id,
                                            prev_sib_id,
                                            Some(feature_id),
                                        )),
                                        window,
                                        cx,
                                    );
                                }
                            },
                        ));
                        last_visible_child_feature.insert(parent_id, (feature_id, feat_num));

                        let is_selected = selected_item_id == Some(feature_id);
                        list = list.child(self.render_feature_row(
                            &snapshot,
                            feature,
                            depth,
                            actual_depth,
                            has_children,
                            is_expanded,
                            is_selected,
                            cx,
                        ));
                    }
                    OutlineRow::WorkItem {
                        work_item_id,
                        parent_feature_id,
                        depth,
                        ref shared_feature_ids,
                    } => {
                        let Some(wi) = snapshot.item(work_item_id) else {
                            continue;
                        };
                        let Some(parent_feature) = snapshot.item(parent_feature_id) else {
                            continue;
                        };
                        let row_key = (parent_feature.item_number as usize) * 100_000
                            + (wi.item_number as usize);
                        let wis = snapshot.work_items_for_feature(parent_feature_id);
                        let prev_wi_id = wis
                            .iter()
                            .position(|w| w.id == work_item_id)
                            .and_then(|pos| pos.checked_sub(1).map(|p| wis[p].id));

                        let app_wi_before = self.app_state.clone();
                        list = list.child(render_drop_gap(
                            ("outline-wi-gap-before", row_key),
                            10.0 + visual_indent_px(depth),
                            move |payload| {
                                is_valid_feature_wi_gap(
                                    payload,
                                    project_id,
                                    parent_feature_id,
                                    prev_wi_id,
                                    Some(work_item_id),
                                )
                            },
                            move |payload, window, cx| {
                                if let DragPayload::WorkItem {
                                    work_item_id: moving_wi,
                                    ..
                                } = payload
                                {
                                    dispatch_dnd_command(
                                        app_wi_before.clone(),
                                        Box::new(ReorderFeatureWorkItems::new(
                                            project_id,
                                            parent_feature_id,
                                            *moving_wi,
                                            prev_wi_id,
                                            Some(work_item_id),
                                        )),
                                        window,
                                        cx,
                                    );
                                }
                            },
                        ));

                        let parent_actual_depth = snapshot.feature_depth(parent_feature_id);
                        let is_selected = selected_item_id == Some(work_item_id);
                        list = list.child(self.render_work_item_row(
                            &snapshot,
                            wi,
                            parent_feature,
                            parent_actual_depth,
                            depth,
                            shared_feature_ids,
                            is_selected,
                            cx,
                        ));

                        let next_is_same_parent_wi = matches!(
                            rows.get(idx + 1),
                            Some(OutlineRow::WorkItem {
                                parent_feature_id: next_pfid,
                                ..
                            }) if *next_pfid == parent_feature_id
                        );
                        if !next_is_same_parent_wi {
                            let app_wi_after = self.app_state.clone();
                            list = list.child(render_drop_gap(
                                ("outline-wi-gap-after", row_key),
                                10.0 + visual_indent_px(depth),
                                move |payload| {
                                    is_valid_feature_wi_gap(
                                        payload,
                                        project_id,
                                        parent_feature_id,
                                        Some(work_item_id),
                                        None,
                                    )
                                },
                                move |payload, window, cx| {
                                    if let DragPayload::WorkItem {
                                        work_item_id: moving_wi,
                                        ..
                                    } = payload
                                    {
                                        dispatch_dnd_command(
                                            app_wi_after.clone(),
                                            Box::new(ReorderFeatureWorkItems::new(
                                                project_id,
                                                parent_feature_id,
                                                *moving_wi,
                                                Some(work_item_id),
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
                    OutlineRow::InlineAdd {
                        feature_id,
                        parent_actual_depth,
                        depth,
                    } => {
                        if let Some((last_child_id, last_child_num)) =
                            last_visible_child_feature.get(&Some(feature_id)).copied()
                        {
                            let app_child_after = self.app_state.clone();
                            list = list.child(render_drop_gap(
                                ("outline-feature-gap-after", last_child_num),
                                10.0 + visual_indent_px(depth),
                                move |payload| {
                                    is_valid_feature_sibling_gap(
                                        payload,
                                        project_id,
                                        Some(feature_id),
                                        Some(last_child_id),
                                        None,
                                    )
                                },
                                move |payload, window, cx| {
                                    if let DragPayload::Feature {
                                        feature_id: moving_id,
                                        ..
                                    } = payload
                                    {
                                        dispatch_dnd_command(
                                            app_child_after.clone(),
                                            Box::new(ReorderSiblingFeatures::new(
                                                project_id,
                                                *moving_id,
                                                Some(last_child_id),
                                                None,
                                            )),
                                            window,
                                            cx,
                                        );
                                    }
                                },
                            ));
                        }

                        let Some(feature) = snapshot.item(feature_id) else {
                            continue;
                        };
                        list = list.child(self.render_inline_add_row(
                            feature,
                            parent_actual_depth,
                            depth,
                            cx,
                        ));
                    }
                }
            }

            if self.scope == OutlineScope::AllFeatures
                && let Some((last_root_id, last_root_num)) =
                    last_visible_child_feature.get(&None).copied()
            {
                let app_root_after = self.app_state.clone();
                list = list.child(render_drop_gap(
                    ("outline-feature-gap-after", last_root_num),
                    10.0,
                    move |payload| {
                        is_valid_feature_sibling_gap(
                            payload,
                            project_id,
                            None,
                            Some(last_root_id),
                            None,
                        )
                    },
                    move |payload, window, cx| {
                        if let DragPayload::Feature {
                            feature_id: moving_id,
                            ..
                        } = payload
                        {
                            dispatch_dnd_command(
                                app_root_after.clone(),
                                Box::new(ReorderSiblingFeatures::new(
                                    project_id,
                                    *moving_id,
                                    Some(last_root_id),
                                    None,
                                )),
                                window,
                                cx,
                            );
                        }
                    },
                ));
            }

            list.into_any_element()
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(toolbar)
            .child(body)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use chrono::{DateTime, TimeZone, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::domain::{FeatureWorkItem, Project, ProjectId, ProjectStatus};

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 23, 12, 0, 0).unwrap()
    }

    fn make_test_snapshot() -> (
        ProjectSnapshot,
        ItemId,
        ItemId,
        ItemId,
        ItemId,
        ItemId,
    ) {
        let pid = ProjectId::from_uuid(Uuid::from_u128(1));
        let f1 = ItemId::from_uuid(Uuid::from_u128(10));
        let f2 = ItemId::from_uuid(Uuid::from_u128(11));
        let f3 = ItemId::from_uuid(Uuid::from_u128(12));
        let w_shared = ItemId::from_uuid(Uuid::from_u128(20));
        let w_done = ItemId::from_uuid(Uuid::from_u128(21));

        let project = Project {
            id: pid,
            name: "Outline Test".to_string(),
            key_prefix: "OUT".to_string(),
            next_item_seq: 6,
            description: String::new(),
            color: "#6366F1".to_string(),
            status: ProjectStatus::Active,
            sort_order: "a0".to_string(),
            created_at: ts(),
            updated_at: ts(),
        };

        let make_item = |id, num, kind, parent, status| Item {
            id,
            project_id: pid,
            item_number: num,
            item_key: format!("OUT-{num}"),
            kind,
            parent_feature_id: parent,
            title: format!("Item {num}"),
            description: String::new(),
            status,
            priority: Priority::None,
            assignee_id: None,
            sort_order: format!("a{num}"),
            deleted_at: None,
            trash_root_id: None,
            created_at: ts(),
            updated_at: ts(),
        };

        let mut snap = ProjectSnapshot {
            project: project.clone(),
            projects: vec![project],
            items: vec![
                make_item(f1, 1, ItemKind::Feature, None, ItemStatus::Done),
                make_item(f2, 2, ItemKind::Feature, Some(f1), ItemStatus::Todo),
                make_item(f3, 3, ItemKind::Feature, None, ItemStatus::Done),
                make_item(w_shared, 4, ItemKind::WorkItem, None, ItemStatus::Todo),
                make_item(w_done, 5, ItemKind::WorkItem, None, ItemStatus::Done),
            ],
            assignees: Vec::new(),
            tags: Vec::new(),
            feature_work_items: vec![
                FeatureWorkItem {
                    project_id: pid,
                    feature_id: f2,
                    work_item_id: w_shared,
                    sort_order: "a0".to_string(),
                    created_at: ts(),
                },
                FeatureWorkItem {
                    project_id: pid,
                    feature_id: f3,
                    work_item_id: w_shared,
                    sort_order: "a0".to_string(),
                    created_at: ts(),
                },
                FeatureWorkItem {
                    project_id: pid,
                    feature_id: f3,
                    work_item_id: w_done,
                    sort_order: "a1".to_string(),
                    created_at: ts(),
                },
            ],
            item_tags: HashMap::new(),
            progress_by_feature: HashMap::new(),
            item_blockers: Vec::new(),
            direct_blockers_by_item: HashMap::new(),
            inherited_blockers_by_item: HashMap::new(),
            effective_block_state: HashMap::new(),
            blocker_warnings: HashMap::new(),
            trash_count: 0,
            trash_groups: Vec::new(),
        };
        snap.progress_by_feature = snap.compute_progress_map();
        (snap, f1, f2, f3, w_shared, w_done)
    }

    #[test]
    fn outline_projection_handles_shared_items_subtree_scope_and_hide_closed() {
        let (snap, f1, f2, f3, w_shared, w_done) = make_test_snapshot();
        let expanded = HashSet::from([f1, f2, f3]);

        // AllFeatures + show closed: w_shared appears under both f2 and f3, InlineAdd at bottom of each expanded Feature
        let rows = build_outline_rows(&snap, &expanded, OutlineScope::AllFeatures, Some(f1), false);
        assert_eq!(
            rows,
            vec![
                OutlineRow::Feature {
                    feature_id: f1,
                    depth: 0,
                    actual_depth: 0,
                    has_children: true,
                    is_expanded: true,
                },
                OutlineRow::Feature {
                    feature_id: f2,
                    depth: 1,
                    actual_depth: 1,
                    has_children: true,
                    is_expanded: true,
                },
                OutlineRow::WorkItem {
                    work_item_id: w_shared,
                    parent_feature_id: f2,
                    depth: 2,
                    shared_feature_ids: vec![f2, f3],
                },
                OutlineRow::InlineAdd {
                    feature_id: f2,
                    parent_actual_depth: 1,
                    depth: 2,
                },
                OutlineRow::InlineAdd {
                    feature_id: f1,
                    parent_actual_depth: 0,
                    depth: 1,
                },
                OutlineRow::Feature {
                    feature_id: f3,
                    depth: 0,
                    actual_depth: 0,
                    has_children: true,
                    is_expanded: true,
                },
                OutlineRow::WorkItem {
                    work_item_id: w_shared,
                    parent_feature_id: f3,
                    depth: 1,
                    shared_feature_ids: vec![f2, f3],
                },
                OutlineRow::WorkItem {
                    work_item_id: w_done,
                    parent_feature_id: f3,
                    depth: 1,
                    shared_feature_ids: vec![f3],
                },
                OutlineRow::InlineAdd {
                    feature_id: f3,
                    parent_actual_depth: 0,
                    depth: 1,
                },
            ]
        );

        // Hide Done/Cancelled: f1 (Done) stays visible because f2 (Todo) is open; w_done under f3 is hidden; f3 stays visible because w_shared is Todo.
        let filtered =
            build_outline_rows(&snap, &expanded, OutlineScope::AllFeatures, Some(f1), true);
        assert!(!filtered.iter().any(|r| matches!(
            r,
            OutlineRow::WorkItem { work_item_id, .. } if *work_item_id == w_done
        )));
        assert!(filtered.iter().any(|r| matches!(
            r,
            OutlineRow::Feature { feature_id, .. } if *feature_id == f1
        )));

        // SelectedSubtree scoped to f2 starts f2 at visual depth 0 while keeping actual_depth = 1
        let scoped = build_outline_rows(
            &snap,
            &expanded,
            OutlineScope::SelectedSubtree,
            Some(f2),
            false,
        );
        assert_eq!(
            scoped,
            vec![
                OutlineRow::Feature {
                    feature_id: f2,
                    depth: 0,
                    actual_depth: 1,
                    has_children: true,
                    is_expanded: true,
                },
                OutlineRow::WorkItem {
                    work_item_id: w_shared,
                    parent_feature_id: f2,
                    depth: 1,
                    shared_feature_ids: vec![f2, f3],
                },
                OutlineRow::InlineAdd {
                    feature_id: f2,
                    parent_actual_depth: 1,
                    depth: 1,
                },
            ]
        );

        // Collapsing f2 hides its children and InlineAdd row
        let collapsed = HashSet::from([f1]);
        let collapsed_rows = build_outline_rows(
            &snap,
            &collapsed,
            OutlineScope::SelectedSubtree,
            Some(f2),
            false,
        );
        assert_eq!(
            collapsed_rows,
            vec![OutlineRow::Feature {
                feature_id: f2,
                depth: 0,
                actual_depth: 1,
                has_children: true,
                is_expanded: false,
            }]
        );
    }
}
