use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _, StyledExt as _,
    WindowExt as _, h_flex, notification::Notification, v_flex,
};
use gpui_kit::{
    Anchor, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};

use crate::commands::{
    AttachWorkItemToFeature, DetachWorkItemFromFeature, ReparentFeature, ReorderSiblingFeatures,
    SetWorkItemFeatures, UndoableCommand, UpdateItemFields,
};
use crate::domain::{
    Item, ItemId, ItemKind, ItemStatus, MAX_FEATURE_DEPTH, Priority, ProjectId, ProjectSnapshot,
    clamped_depth_badge, visual_indent_px,
};
use crate::state::{AppState, ProjectStore};
use crate::ui::modals::open_create_feature_modal;
use crate::ui::{
    PrioritySelectOption, StatusSelectOption, all_priority_options, all_status_options,
    assignee_pill, domain_tag_chip, kind_badge,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptionMode {
    Edit,
    Preview,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParentFeatureOption {
    id: Option<ItemId>,
    label: SharedString,
    display_label: SharedString,
    disabled: bool,
    reason: Option<SharedString>,
}

impl SelectItem for ParentFeatureOption {
    type Value = Option<ItemId>;

    fn title(&self) -> SharedString {
        self.display_label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }

    fn matches(&self, query: &str) -> bool {
        self.label.to_lowercase().contains(&query.to_lowercase())
    }

    fn disabled(&self) -> bool {
        self.disabled
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_1p5()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_xs()
                    .when(self.disabled, |this| {
                        this.text_color(cx.theme().muted_foreground)
                    })
                    .child(self.label.clone()),
            )
            .when_some(self.reason.clone(), |this, reason| {
                this.child(
                    div()
                        .text_size(px(10.))
                        .text_color(cx.theme().muted_foreground)
                        .child(reason),
                )
            })
    }
}

pub struct InspectorView {
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    loaded_item_id: Option<ItemId>,
    loaded_project_id: Option<ProjectId>,
    loaded_title: String,
    loaded_description: String,
    loaded_status: ItemStatus,
    loaded_priority: Priority,
    loaded_parent_id: Option<ItemId>,
    parent_options_sig: Vec<(ItemId, String, String, Option<ItemId>)>,
    title_input: Entity<InputState>,
    description_input: Entity<TextareaState>,
    status_select: Entity<SelectState<Vec<StatusSelectOption>>>,
    priority_select: Entity<SelectState<Vec<PrioritySelectOption>>>,
    parent_select: Entity<SelectState<SearchableVec<ParentFeatureOption>>>,
    attach_popover_open: bool,
    attach_search_input: Entity<InputState>,
    description_mode: DescriptionMode,
    preview_revision: u64,
    dirty_title: bool,
    dirty_description: bool,
    saving: bool,
    syncing: bool,
    title_error: Option<String>,
    debounce_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl InspectorView {
    pub fn new(
        app_state: Entity<AppState>,
        store: Entity<ProjectStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title_input = cx.new(|cx| InputState::new(window, cx).placeholder("Item title…"));
        let description_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(8, 18)
                .soft_wrap(true)
                .placeholder("Write Markdown notes, acceptance criteria, or context…")
        });

        let status_options = all_status_options();
        let status_select =
            cx.new(|cx| SelectState::new(status_options, Some(IndexPath::new(1)), window, cx));

        let priority_options = all_priority_options();
        let priority_select =
            cx.new(|cx| SelectState::new(priority_options, Some(IndexPath::new(0)), window, cx));

        let parent_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(vec![root_parent_option()]),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
            .searchable(true)
        });
        let attach_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter Features…"));

        let store_sub = cx.observe(&store, |_, _, cx| cx.notify());
        let attach_search_sub = cx.subscribe_in(
            &attach_search_input,
            window,
            |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        );

        let title_sub = cx.subscribe_in(
            &title_input,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if this.syncing {
                    return;
                }
                match event {
                    InputEvent::Change => {
                        this.dirty_title = true;
                        let val = this.title_input.read(cx).value();
                        if val.trim().is_empty() {
                            this.title_error = Some("Title cannot be empty".to_string());
                            this.debounce_task.take();
                        } else {
                            this.title_error = None;
                            this.schedule_debounced_save(cx);
                        }
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => {
                        this.flush_dirty_current(cx);
                    }
                    InputEvent::Blur => {
                        let empty = this.title_input.read(cx).value().trim().is_empty();
                        if empty {
                            this.syncing = true;
                            let restored = this.loaded_title.clone();
                            this.title_input.update(cx, |inp, cx| {
                                inp.set_value(restored, window, cx);
                            });
                            this.syncing = false;
                            this.dirty_title = false;
                            this.title_error = None;
                            cx.notify();
                        } else {
                            this.flush_dirty_current(cx);
                        }
                    }
                    InputEvent::Focus => {}
                }
            },
        );

        let desc_sub = cx.subscribe_in(
            &description_input,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if this.syncing {
                    return;
                }
                match event {
                    InputEvent::Change => {
                        this.dirty_description = true;
                        this.preview_revision = this.preview_revision.wrapping_add(1);
                        this.schedule_debounced_save(cx);
                        cx.notify();
                    }
                    InputEvent::Blur => {
                        this.flush_dirty_current(cx);
                    }
                    InputEvent::PressEnter { .. } | InputEvent::Focus => {}
                }
            },
        );

        let status_sub = cx.subscribe_in(
            &status_select,
            window,
            |this, _, event: &SelectEvent<Vec<StatusSelectOption>>, _, cx| {
                if this.syncing {
                    return;
                }
                if let SelectEvent::Confirm(Some(new_status)) = event {
                    let Some(item_id) = this.loaded_item_id else {
                        return;
                    };
                    if *new_status == this.loaded_status {
                        return;
                    }
                    this.loaded_status = *new_status;
                    this.dispatch_item_command(
                        Box::new(UpdateItemFields {
                            item_id,
                            title: None,
                            description: None,
                            status: Some(*new_status),
                            priority: None,
                        }),
                        None,
                        cx,
                    );
                }
            },
        );

        let priority_sub = cx.subscribe_in(
            &priority_select,
            window,
            |this, _, event: &SelectEvent<Vec<PrioritySelectOption>>, _, cx| {
                if this.syncing {
                    return;
                }
                if let SelectEvent::Confirm(Some(new_priority)) = event {
                    let Some(item_id) = this.loaded_item_id else {
                        return;
                    };
                    if *new_priority == this.loaded_priority {
                        return;
                    }
                    this.loaded_priority = *new_priority;
                    this.dispatch_item_command(
                        Box::new(UpdateItemFields {
                            item_id,
                            title: None,
                            description: None,
                            status: None,
                            priority: Some(*new_priority),
                        }),
                        None,
                        cx,
                    );
                }
            },
        );

        let parent_sub = cx.subscribe_in(
            &parent_select,
            window,
            |this, _, event: &SelectEvent<SearchableVec<ParentFeatureOption>>, window, cx| {
                if this.syncing {
                    return;
                }
                if let SelectEvent::Confirm(Some(new_parent)) = event {
                    let Some(feature_id) = this.loaded_item_id else {
                        return;
                    };
                    if *new_parent == this.loaded_parent_id {
                        return;
                    }
                    let previous_parent = this.loaded_parent_id;
                    this.loaded_parent_id = *new_parent;
                    let parent_select = this.parent_select.clone();
                    let command = Box::new(ReparentFeature::new(feature_id, *new_parent));
                    this.saving = true;
                    cx.notify();

                    let task = this
                        .app_state
                        .update(cx, |app, cx| app.execute_command(command, cx));
                    let view = cx.entity().downgrade();
                    window
                        .spawn(cx, async move |cx| {
                            let res = task.await;
                            let _ = view.update(cx, |this, cx| {
                                this.saving = false;
                                if let Err(err) = &res {
                                    this.loaded_parent_id = previous_parent;
                                    tracing::warn!(error = %err, "Failed to reparent Feature");
                                }
                                cx.notify();
                            });
                            if let Err(err) = res {
                                let _ = cx.update(|window, cx| {
                                    parent_select.update(cx, |sel, cx| {
                                        sel.set_selected_value(&previous_parent, window, cx);
                                    });
                                    window.push_notification(
                                        Notification::error(err.to_string()),
                                        cx,
                                    );
                                });
                            }
                        })
                        .detach();
                }
            },
        );

        Self {
            app_state,
            store,
            loaded_item_id: None,
            loaded_project_id: None,
            loaded_title: String::new(),
            loaded_description: String::new(),
            loaded_status: ItemStatus::Todo,
            loaded_priority: Priority::None,
            loaded_parent_id: None,
            parent_options_sig: Vec::new(),
            title_input,
            description_input,
            status_select,
            priority_select,
            parent_select,
            attach_popover_open: false,
            attach_search_input,
            description_mode: DescriptionMode::Preview,
            preview_revision: 0,
            dirty_title: false,
            dirty_description: false,
            saving: false,
            syncing: false,
            title_error: None,
            debounce_task: None,
            _subscriptions: vec![
                store_sub,
                attach_search_sub,
                title_sub,
                desc_sub,
                status_sub,
                priority_sub,
                parent_sub,
            ],
        }
    }

    pub fn description_mode(&self) -> DescriptionMode {
        self.description_mode
    }

    pub fn set_description_mode(
        &mut self,
        mode: DescriptionMode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.description_mode == mode {
            return;
        }
        if mode == DescriptionMode::Preview {
            self.flush_dirty_current(cx);
            self.preview_revision = self.preview_revision.wrapping_add(1);
        }
        self.description_mode = mode;
        cx.notify();
    }

    pub fn title_input(&self) -> &Entity<InputState> {
        &self.title_input
    }

    pub fn description_input(&self) -> &Entity<TextareaState> {
        &self.description_input
    }

    pub fn flush_pending_edits_for_shutdown(&mut self, cx: &mut Context<Self>) {
        self.debounce_task.take();
        let Some(item_id) = self.loaded_item_id else {
            return;
        };

        let title = if self.dirty_title {
            let trimmed = self.title_input.read(cx).value().trim().to_string();
            if !trimmed.is_empty() && trimmed != self.loaded_title {
                Some(trimmed)
            } else {
                None
            }
        } else {
            None
        };

        let description = if self.dirty_description {
            let raw = self.description_input.read(cx).value().to_string();
            if raw != self.loaded_description {
                Some(raw)
            } else {
                None
            }
        } else {
            None
        };

        if title.is_none() && description.is_none() {
            return;
        }

        let command = Box::new(UpdateItemFields {
            item_id,
            title: title.clone(),
            description: description.clone(),
            status: None,
            priority: None,
        });
        if self
            .app_state
            .read(cx)
            .execute_command_for_shutdown(command)
            .is_ok()
        {
            if let Some(t) = title {
                self.loaded_title = t;
            }
            if let Some(d) = description {
                self.loaded_description = d;
            }
            self.dirty_title = false;
            self.dirty_description = false;
        }
    }

    fn schedule_debounced_save(&mut self, cx: &mut Context<Self>) {
        self.debounce_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(350))
                .await;
            let _ = this.update(cx, |view, cx| {
                view.flush_dirty_current(cx);
            });
        }));
    }

    fn flush_dirty_current(&mut self, cx: &mut Context<Self>) {
        self.debounce_task.take();
        let Some(item_id) = self.loaded_item_id else {
            return;
        };

        let title = if self.dirty_title {
            let trimmed = self.title_input.read(cx).value().trim().to_string();
            if trimmed.is_empty() {
                None
            } else if trimmed == self.loaded_title {
                self.dirty_title = false;
                self.title_error = None;
                None
            } else {
                Some(trimmed)
            }
        } else {
            None
        };

        let description = if self.dirty_description {
            let raw = self.description_input.read(cx).value().to_string();
            if raw == self.loaded_description {
                self.dirty_description = false;
                None
            } else {
                Some(raw)
            }
        } else {
            None
        };

        if title.is_none() && description.is_none() {
            return;
        }

        let sent_title = title.clone();
        let sent_desc = description.clone();
        if title.is_some() {
            self.dirty_title = false;
        }
        if description.is_some() {
            self.dirty_description = false;
        }

        self.dispatch_item_command(
            Box::new(UpdateItemFields {
                item_id,
                title,
                description,
                status: None,
                priority: None,
            }),
            Some((item_id, sent_title, sent_desc)),
            cx,
        );
    }

    fn flush_dirty_for_previous_item(&mut self, prev_id: ItemId, cx: &mut Context<Self>) {
        self.debounce_task.take();

        let title = if self.dirty_title {
            let trimmed = self.title_input.read(cx).value().trim().to_string();
            if !trimmed.is_empty() && trimmed != self.loaded_title {
                Some(trimmed)
            } else {
                None
            }
        } else {
            None
        };

        let description = if self.dirty_description {
            let raw = self.description_input.read(cx).value().to_string();
            if raw != self.loaded_description {
                Some(raw)
            } else {
                None
            }
        } else {
            None
        };

        self.dirty_title = false;
        self.dirty_description = false;
        self.title_error = None;

        if title.is_none() && description.is_none() {
            return;
        }

        let command = Box::new(UpdateItemFields {
            item_id: prev_id,
            title,
            description,
            status: None,
            priority: None,
        });
        let task = self
            .app_state
            .update(cx, |app, cx| app.execute_command(command, cx));
        task.detach();
    }

    fn dispatch_item_command(
        &mut self,
        command: Box<dyn UndoableCommand>,
        saved_fields: Option<(ItemId, Option<String>, Option<String>)>,
        cx: &mut Context<Self>,
    ) {
        self.saving = true;
        cx.notify();

        let task = self
            .app_state
            .update(cx, |app, cx| app.execute_command(command, cx));
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |view, cx| {
                view.saving = false;
                if let Some((item_id, sent_title, sent_desc)) = saved_fields
                    && view.loaded_item_id == Some(item_id)
                {
                    match &res {
                        Ok(_) => {
                            if let Some(t) = sent_title {
                                view.loaded_title = t;
                            }
                            if let Some(d) = sent_desc {
                                view.loaded_description = d;
                            }
                        }
                        Err(_) => {
                            if sent_title.is_some() {
                                view.dirty_title = true;
                            }
                            if sent_desc.is_some() {
                                view.dirty_description = true;
                            }
                        }
                    }
                }
                if let Err(err) = res {
                    tracing::warn!(error = %err, "Failed to persist item edit");
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn sync_from_store(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (snapshot_opt, current_item): (Option<ProjectSnapshot>, Option<Item>) = {
            let state = self.store.read(cx);
            (state.snapshot().cloned(), state.selected_item().cloned())
        };
        let current_id = current_item.as_ref().map(|i| i.id);

        if current_id != self.loaded_item_id {
            if let Some(prev_id) = self.loaded_item_id {
                self.flush_dirty_for_previous_item(prev_id, cx);
            }

            self.attach_popover_open = false;
            self.syncing = true;
            self.attach_search_input
                .update(cx, |inp, cx| inp.set_value("", window, cx));
            if let (Some(snapshot), Some(item)) = (&snapshot_opt, current_item) {
                self.loaded_item_id = Some(item.id);
                self.loaded_project_id = Some(item.project_id);
                self.loaded_title = item.title.clone();
                self.loaded_description = item.description.clone();
                self.loaded_status = item.status;
                self.loaded_priority = item.priority;
                self.loaded_parent_id = item.parent_feature_id;
                self.dirty_title = false;
                self.dirty_description = false;
                self.title_error = None;
                self.preview_revision = self.preview_revision.wrapping_add(1);

                self.title_input.update(cx, |inp, cx| {
                    inp.set_value(item.title.clone(), window, cx);
                });
                self.description_input.update(cx, |inp, cx| {
                    inp.set_value(item.description.clone(), window, cx);
                });
                self.status_select.update(cx, |sel, cx| {
                    sel.set_selected_value(&item.status, window, cx);
                });
                self.priority_select.update(cx, |sel, cx| {
                    sel.set_selected_value(&item.priority, window, cx);
                });

                if item.kind == ItemKind::Feature {
                    self.rebuild_parent_options(snapshot, &item, window, cx);
                } else {
                    self.parent_options_sig.clear();
                }
            } else {
                self.loaded_item_id = None;
                self.loaded_project_id = None;
                self.loaded_title.clear();
                self.loaded_description.clear();
                self.loaded_parent_id = None;
                self.parent_options_sig.clear();
                self.dirty_title = false;
                self.dirty_description = false;
                self.title_error = None;

                self.title_input
                    .update(cx, |inp, cx| inp.set_value("", window, cx));
                self.description_input
                    .update(cx, |inp, cx| inp.set_value("", window, cx));
            }
            self.syncing = false;
            return;
        }

        let (Some(snapshot), Some(item)) = (snapshot_opt, current_item) else {
            return;
        };

        self.syncing = true;
        if item.status != self.loaded_status {
            self.loaded_status = item.status;
            self.status_select
                .update(cx, |sel, cx| sel.set_selected_value(&item.status, window, cx));
        }
        if item.priority != self.loaded_priority {
            self.loaded_priority = item.priority;
            self.priority_select.update(cx, |sel, cx| {
                sel.set_selected_value(&item.priority, window, cx)
            });
        }

        if item.kind == ItemKind::Feature {
            let new_sig = feature_tree_signature(&snapshot);
            if new_sig != self.parent_options_sig
                || item.parent_feature_id != self.loaded_parent_id
            {
                self.loaded_parent_id = item.parent_feature_id;
                self.rebuild_parent_options(&snapshot, &item, window, cx);
            }
        }

        let title_focused = self.title_input.read(cx).focus_handle(cx).is_focused(window);
        if !self.saving && !self.dirty_title && !title_focused && item.title != self.loaded_title {
            self.loaded_title = item.title.clone();
            self.title_input
                .update(cx, |inp, cx| inp.set_value(item.title.clone(), window, cx));
        }

        let desc_focused = self
            .description_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if !self.saving
            && !self.dirty_description
            && !desc_focused
            && item.description != self.loaded_description
        {
            self.loaded_description = item.description.clone();
            self.description_input.update(cx, |inp, cx| {
                inp.set_value(item.description.clone(), window, cx)
            });
            self.preview_revision = self.preview_revision.wrapping_add(1);
        }
        self.syncing = false;
    }

    fn rebuild_parent_options(
        &mut self,
        snapshot: &ProjectSnapshot,
        item: &Item,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.parent_options_sig = feature_tree_signature(snapshot);
        let options = build_parent_feature_options(snapshot, item.id);
        let selected_parent = item.parent_feature_id;
        self.parent_select.update(cx, |sel, cx| {
            sel.set_items(SearchableVec::new(options), window, cx);
            sel.set_selected_value(&selected_parent, window, cx);
        });
    }

    fn dispatch_feature_membership_command(
        &mut self,
        command: Box<dyn UndoableCommand>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.saving = true;
        cx.notify();

        let task = self
            .app_state
            .update(cx, |app, cx| app.execute_command(command, cx));
        let view = cx.entity().downgrade();

        window
            .spawn(cx, async move |cx| {
                let res = task.await;
                let _ = view.update(cx, |this, cx| {
                    this.saving = false;
                    cx.notify();
                });
                let _ = cx.update(|window, cx| match res {
                    Ok(effect) => {
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
}

fn root_parent_option() -> ParentFeatureOption {
    ParentFeatureOption {
        id: None,
        label: "Root (No Parent)".into(),
        display_label: "Root (No Parent)".into(),
        disabled: false,
        reason: None,
    }
}

fn feature_tree_signature(snapshot: &ProjectSnapshot) -> Vec<(ItemId, String, String, Option<ItemId>)> {
    snapshot
        .active_features_dfs()
        .into_iter()
        .map(|(f, _)| (f.id, f.item_key.clone(), f.title.clone(), f.parent_feature_id))
        .collect()
}

fn build_parent_feature_options(
    snapshot: &ProjectSnapshot,
    feature_id: ItemId,
) -> Vec<ParentFeatureOption> {
    let mut options = vec![root_parent_option()];
    let descendants = snapshot.descendant_feature_ids(feature_id);
    let subtree_height = snapshot.feature_subtree_height(feature_id);

    for (cand, depth) in snapshot.active_features_dfs() {
        if cand.id == feature_id || descendants.contains(&cand.id) {
            continue;
        }
        let exceeds_depth = depth + 1 + subtree_height >= MAX_FEATURE_DEPTH;
        let indent = "  ".repeat(depth.min(6));
        let label = format!("{indent}{} {}", cand.item_key, cand.title);
        let display_label = format!("{} {}", cand.item_key, cand.title);
        options.push(ParentFeatureOption {
            id: Some(cand.id),
            label: label.into(),
            display_label: display_label.into(),
            disabled: exceeds_depth,
            reason: exceeds_depth.then(|| SharedString::from("Max depth 32")),
        });
    }

    options
}

impl Render for InspectorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_from_store(window, cx);

        let state = self.store.read(cx);
        let snapshot = state.snapshot().cloned();
        let has_workspace = state.has_workspace();
        let selected = state.selected_item().cloned();

        let status_pill = if self.saving {
            Tag::info().xsmall().outline().child("Saving…")
        } else if self.dirty_title || self.dirty_description {
            Tag::warning().xsmall().outline().child("Unsaved")
        } else {
            Tag::secondary().xsmall().child("Saved")
        };

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
                    .when(selected.is_some(), |this| this.child(status_pill)),
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
                    "Select a Feature or Work Item to inspect and edit its details.",
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
                            let depth = snapshot.feature_depth(item.id);
                            let at_max_depth = depth + 1 >= MAX_FEATURE_DEPTH;
                            let (done, total) = snapshot.feature_progress(item.id);
                            let pct = (done * 100).checked_div(total).unwrap_or(0);
                            let child_features = snapshot.child_features(Some(item.id)).len();
                            let attached_wis = snapshot.work_items_for_feature(item.id).len();

                            let siblings = snapshot.sibling_features(item.id);
                            let sib_pos = siblings.iter().position(|s| s.id == item.id);
                            let can_move_up = sib_pos.is_some_and(|ix| ix > 0);
                            let can_move_down =
                                sib_pos.is_some_and(|ix| ix + 1 < siblings.len());

                            let move_up_ids = sib_pos.filter(|ix| *ix > 0).map(|ix| {
                                (
                                    ix.checked_sub(2).map(|i| siblings[i].id),
                                    Some(siblings[ix - 1].id),
                                )
                            });
                            let move_down_ids = sib_pos
                                .filter(|ix| *ix + 1 < siblings.len())
                                .map(|ix| {
                                    (
                                        Some(siblings[ix + 1].id),
                                        siblings.get(ix + 2).map(|s| s.id),
                                    )
                                });

                            let feat_id = item.id;
                            let proj_id = item.project_id;
                            let feat_label = format!("{} {}", item.item_key, item.title);

                            v_flex()
                                .gap_2p5()
                                .p_3()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().muted.opacity(0.45))
                                .border_1()
                                .border_color(cx.theme().border)
                                .child(
                                    h_flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_size(px(10.))
                                                .font_semibold()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("HIERARCHY & PROGRESS"),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(10.))
                                                .font_family(cx.theme().mono_font_family.clone())
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!("Depth {}/{}", depth + 1, MAX_FEATURE_DEPTH)),
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
                                                .child("Parent"),
                                        )
                                        .child(
                                            div().flex_1().min_w_0().child(
                                                Select::new(&self.parent_select)
                                                    .small()
                                                    .search_placeholder("Filter parent Features…"),
                                            ),
                                        ),
                                )
                                .when(!ancestors.is_empty(), |this| {
                                    this.child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(div().w(px(76.)))
                                            .child(
                                                h_flex()
                                                    .flex_1()
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
                                                                        let store = self.store.clone();
                                                                        move |_, _, cx| {
                                                                            store.update(
                                                                                cx,
                                                                                |s, cx| {
                                                                                    s.select_item(
                                                                                        Some(anc_id),
                                                                                    );
                                                                                    cx.notify();
                                                                                },
                                                                            );
                                                                        }
                                                                    }),
                                                                )
                                                        },
                                                    )),
                                            ),
                                    )
                                })
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
                                                                    120.0
                                                                        * (done as f32
                                                                            / total as f32)
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
                                                        .font_family(
                                                            cx.theme().mono_font_family.clone(),
                                                        )
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
                                .child(
                                    h_flex()
                                        .pt_1()
                                        .gap_1p5()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            Button::new("inspector-add-subfeature")
                                                .outline()
                                                .xsmall()
                                                .icon(IconName::Plus)
                                                .label("Sub-Feature")
                                                .disabled(at_max_depth)
                                                .tooltip(if at_max_depth {
                                                    "Maximum Feature nesting depth (32) reached"
                                                } else {
                                                    "Create nested Sub-Feature"
                                                })
                                                .on_click({
                                                    let app_state = self.app_state.clone();
                                                    move |_, window, cx| {
                                                        open_create_feature_modal(
                                                            app_state.clone(),
                                                            proj_id,
                                                            Some((
                                                                feat_id,
                                                                feat_label.clone(),
                                                                depth,
                                                            )),
                                                            window,
                                                            cx,
                                                        );
                                                    }
                                                }),
                                        )
                                        .child(
                                            h_flex()
                                                .gap_1()
                                                .child(
                                                    Button::new("inspector-move-up")
                                                        .ghost()
                                                        .xsmall()
                                                        .compact()
                                                        .icon(IconName::ArrowUp)
                                                        .disabled(!can_move_up)
                                                        .tooltip("Move Feature before previous sibling")
                                                        .on_click(cx.listener(move |this, _, _, cx| {
                                                            if let Some((before, after)) = move_up_ids {
                                                                this.dispatch_item_command(
                                                                    Box::new(ReorderSiblingFeatures::new(
                                                                        proj_id,
                                                                        feat_id,
                                                                        before,
                                                                        after,
                                                                    )),
                                                                    None,
                                                                    cx,
                                                                );
                                                            }
                                                        })),
                                                )
                                                .child(
                                                    Button::new("inspector-move-down")
                                                        .ghost()
                                                        .xsmall()
                                                        .compact()
                                                        .icon(IconName::ArrowDown)
                                                        .disabled(!can_move_down)
                                                        .tooltip("Move Feature after next sibling")
                                                        .on_click(cx.listener(move |this, _, _, cx| {
                                                            if let Some((before, after)) = move_down_ids {
                                                                this.dispatch_item_command(
                                                                    Box::new(ReorderSiblingFeatures::new(
                                                                        proj_id,
                                                                        feat_id,
                                                                        before,
                                                                        after,
                                                                    )),
                                                                    None,
                                                                    cx,
                                                                );
                                                            }
                                                        })),
                                                ),
                                        ),
                                )
                        }
                        ItemKind::WorkItem => {
                            let wi_id = item.id;
                            let attached = snapshot.features_for_work_item(wi_id);
                            let attached_len = attached.len();
                            let attached_ids: HashSet<ItemId> =
                                attached.iter().map(|f| f.id).collect();
                            let attach_search_focus =
                                self.attach_search_input.read(cx).focus_handle(cx);
                            let inspector_entity = cx.entity();
                            let inspector_for_change = inspector_entity.clone();
                            let inspector_for_content = inspector_entity.clone();
                            let inspector_for_unallocate = inspector_entity.clone();

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
                                            let feat_num = feat.item_number as usize;
                                            let path_titles: Vec<_> = snapshot
                                                .feature_path(feat_id)
                                                .into_iter()
                                                .map(|f| f.title.clone())
                                                .collect();
                                            let display_label = if path_titles.len() > 1 {
                                                path_titles.join(" › ")
                                            } else {
                                                feat.title.clone()
                                            };
                                            let inspector_for_detach = inspector_entity.clone();

                                            h_flex()
                                                .id(("inspector-feature-link", feat_num))
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
                                                    let store = self.store.clone();
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
                                                        .child(display_label),
                                                )
                                                .child(
                                                    Button::new((
                                                        "inspector-detach-feature",
                                                        feat_num,
                                                    ))
                                                    .ghost()
                                                    .xsmall()
                                                    .compact()
                                                    .icon(IconName::Close)
                                                    .tooltip("Remove from this Feature")
                                                    .on_click(move |_, window, cx| {
                                                        cx.stop_propagation();
                                                        inspector_for_detach.update(
                                                            cx,
                                                            |this, cx| {
                                                                this.dispatch_feature_membership_command(
                                                                    Box::new(
                                                                        DetachWorkItemFromFeature::new(
                                                                            wi_id, feat_id,
                                                                        ),
                                                                    ),
                                                                    window,
                                                                    cx,
                                                                );
                                                            },
                                                        );
                                                    }),
                                                )
                                        }))
                                        .into_any_element()
                                })
                                .child(
                                    h_flex()
                                        .pt_1()
                                        .gap_1p5()
                                        .items_center()
                                        .flex_wrap()
                                        .child(
                                            Popover::new("inspector-attach-feature-popover")
                                                .anchor(Anchor::TopLeft)
                                                .open(self.attach_popover_open)
                                                .track_focus(&attach_search_focus)
                                                .on_open_change(move |is_open, window, cx| {
                                                    inspector_for_change.update(cx, |this, cx| {
                                                        this.attach_popover_open = *is_open;
                                                        if *is_open {
                                                            this.attach_search_input.update(
                                                                cx,
                                                                |inp, cx| {
                                                                    inp.set_value("", window, cx);
                                                                    inp.focus(window, cx);
                                                                },
                                                            );
                                                        }
                                                        cx.notify();
                                                    });
                                                })
                                                .trigger(
                                                    Button::new("inspector-attach-feature-btn")
                                                        .outline()
                                                        .xsmall()
                                                        .icon(IconName::Plus)
                                                        .label("Attach to Feature…")
                                                        .dropdown_caret(true),
                                                )
                                                .content(move |_, _, cx| {
                                                    let view_ref = inspector_for_content.read(cx);
                                                    let search_input =
                                                        view_ref.attach_search_input.clone();
                                                    let query_raw = search_input.read(cx).value();
                                                    let query = query_raw.trim().to_lowercase();
                                                    let snap_opt =
                                                        view_ref.store.read(cx).snapshot().cloned();
                                                    let live_attached: HashSet<ItemId> = snap_opt
                                                        .as_ref()
                                                        .map(|snap| {
                                                            snap.features_for_work_item(wi_id)
                                                                .into_iter()
                                                                .map(|f| f.id)
                                                                .collect()
                                                        })
                                                        .unwrap_or_else(|| attached_ids.clone());

                                                    let features: Vec<_> = snap_opt
                                                        .as_ref()
                                                        .map(|snap| {
                                                            snap.active_features_dfs()
                                                                .into_iter()
                                                                .filter(|(feat, _)| {
                                                                    query.is_empty()
                                                                        || feat
                                                                            .item_key
                                                                            .to_lowercase()
                                                                            .contains(&query)
                                                                        || feat
                                                                            .title
                                                                            .to_lowercase()
                                                                            .contains(&query)
                                                                })
                                                                .map(|(feat, depth)| {
                                                                    (feat.clone(), depth)
                                                                })
                                                                .collect()
                                                        })
                                                        .unwrap_or_default();

                                                    v_flex()
                                                        .w(px(300.))
                                                        .gap_2()
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .font_semibold()
                                                                .child("Attach / Detach Features"),
                                                        )
                                                        .child(
                                                            Input::new(&search_input)
                                                                .id("inspector-attach-feature-search")
                                                                .small()
                                                                .cleanable(true),
                                                        )
                                                        .child(if features.is_empty() {
                                                            div()
                                                                .px_2()
                                                                .py_3()
                                                                .text_xs()
                                                                .text_color(
                                                                    cx.theme().muted_foreground,
                                                                )
                                                                .child(if query.is_empty() {
                                                                    "No Features in this Project."
                                                                } else {
                                                                    "No matching Features."
                                                                })
                                                                .into_any_element()
                                                        } else {
                                                            v_flex()
                                                                .max_h(px(220.))
                                                                .gap_0p5()
                                                                .overflow_y_scrollbar()
                                                                .id("inspector-attach-scroll")
                                                                .children(
                                                                    features.into_iter().map(
                                                                        |(feat, depth)| {
                                                                            let fid = feat.id;
                                                                            let is_attached =
                                                                                live_attached
                                                                                    .contains(&fid);
                                                                            let indent =
                                                                                if query.is_empty() {
                                                                                    visual_indent_px(
                                                                                        depth,
                                                                                    )
                                                                                    .min(64.0)
                                                                                } else {
                                                                                    0.0
                                                                                };
                                                                            let depth_badge =
                                                                                clamped_depth_badge(
                                                                                    depth,
                                                                                );
                                                                            let inspector_toggle =
                                                                                inspector_for_content
                                                                                    .clone();

                                                                            ListItem::new((
                                                                                "inspector-attach-option",
                                                                                feat.item_number
                                                                                    as usize,
                                                                            ))
                                                                            .w_full()
                                                                            .pl(px(8.0 + indent))
                                                                            .pr_2()
                                                                            .py_1p5()
                                                                            .rounded(
                                                                                cx.theme().radius
                                                                                    * 0.75,
                                                                            )
                                                                            .selected(is_attached)
                                                                            .on_click(
                                                                                move |_, window, cx| {
                                                                                    let cmd: Box<dyn UndoableCommand> =
                                                                                        if is_attached {
                                                                                            Box::new(DetachWorkItemFromFeature::new(wi_id, fid))
                                                                                        } else {
                                                                                            Box::new(AttachWorkItemToFeature::new(wi_id, fid))
                                                                                        };
                                                                                    inspector_toggle.update(cx, |this, cx| {
                                                                                        this.dispatch_feature_membership_command(cmd, window, cx);
                                                                                    });
                                                                                },
                                                                            )
                                                                            .child(
                                                                                h_flex()
                                                                                    .w_full()
                                                                                    .gap_2()
                                                                                    .items_center()
                                                                                    .child(
                                                                                        div()
                                                                                            .flex()
                                                                                            .items_center()
                                                                                            .justify_center()
                                                                                            .size(px(14.))
                                                                                            .flex_shrink_0()
                                                                                            .rounded(px(3.))
                                                                                            .border_1()
                                                                                            .border_color(if is_attached {
                                                                                                cx.theme().primary
                                                                                            } else {
                                                                                                cx.theme().border
                                                                                            })
                                                                                            .bg(if is_attached {
                                                                                                cx.theme().primary
                                                                                            } else {
                                                                                                cx.theme().background
                                                                                            })
                                                                                            .when(is_attached, |this| {
                                                                                                this.child(
                                                                                                    Icon::new(IconName::Check)
                                                                                                        .xsmall()
                                                                                                        .text_color(cx.theme().primary_foreground),
                                                                                                )
                                                                                            }),
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
                                                                                                cx.theme()
                                                                                                    .muted_foreground,
                                                                                            )
                                                                                            .child(feat.item_key),
                                                                                    )
                                                                                    .child(
                                                                                        div()
                                                                                            .flex_1()
                                                                                            .min_w_0()
                                                                                            .overflow_hidden()
                                                                                            .whitespace_nowrap()
                                                                                            .text_ellipsis()
                                                                                            .text_xs()
                                                                                            .child(feat.title),
                                                                                    )
                                                                                    .when_some(
                                                                                        depth_badge,
                                                                                        |this, badge| {
                                                                                            this.child(
                                                                                                div()
                                                                                                    .text_size(px(10.))
                                                                                                    .text_color(cx.theme().muted_foreground)
                                                                                                    .child(badge),
                                                                                            )
                                                                                        },
                                                                                    ),
                                                                            )
                                                                        },
                                                                    ),
                                                                )
                                                                .into_any_element()
                                                        })
                                                }),
                                        )
                                        .when(attached_len >= 1, |this| {
                                            this.child(
                                                Button::new("inspector-unallocate-btn")
                                                    .outline()
                                                    .xsmall()
                                                    .icon(IconName::Inbox)
                                                    .label("Unallocate")
                                                    .tooltip(
                                                        "Detach from all Features and move to Unallocated",
                                                    )
                                                    .on_click(move |_, window, cx| {
                                                        inspector_for_unallocate.update(
                                                            cx,
                                                            |this, cx| {
                                                                this.attach_popover_open = false;
                                                                this.dispatch_feature_membership_command(
                                                                    Box::new(
                                                                        SetWorkItemFeatures::new(
                                                                            wi_id,
                                                                            Vec::new(),
                                                                        ),
                                                                    ),
                                                                    window,
                                                                    cx,
                                                                );
                                                            },
                                                        );
                                                    }),
                                            )
                                        }),
                                )
                        }
                    };

                    let live_description = if self.dirty_description {
                        self.description_input.read(cx).value().to_string()
                    } else {
                        item.description.clone()
                    };
                    let mode_ix = match self.description_mode {
                        DescriptionMode::Edit => 0,
                        DescriptionMode::Preview => 1,
                    };

                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .p_4()
                        .gap_4()
                        .overflow_y_scrollbar()
                        .id("inspector-scroll")
                        // Identity + disabled Convert action + Editable Title
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
                                    v_flex()
                                        .gap_1()
                                        .child(Input::new(&self.title_input).small())
                                        .when_some(self.title_error.clone(), |this, err| {
                                            this.child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(cx.theme().danger)
                                                    .child(err),
                                            )
                                        }),
                                ),
                        )
                        // Metadata rows (Editable Status & Priority; Assignee/Tags in Phase 5)
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
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .child(Select::new(&self.status_select).small()),
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
                                                .child("Priority"),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .child(Select::new(&self.priority_select).small()),
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
                                                .child("Assignee"),
                                        )
                                        .child(match assignee {
                                            Some(a) => assignee_pill(a, true, cx).into_any_element(),
                                            None => div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("Unassigned (Phase 5)")
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
                                                .child("No tags (Phase 5)")
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
                        // Description (Edit | Preview)
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
                                            TabBar::new("inspector-desc-mode")
                                                .segmented()
                                                .small()
                                                .selected_index(mode_ix)
                                                .child(Tab::new().label("Edit"))
                                                .child(Tab::new().label("Preview"))
                                                .on_click(cx.listener(|this, ix: &usize, window, cx| {
                                                    let next = if *ix == 0 {
                                                        DescriptionMode::Edit
                                                    } else {
                                                        DescriptionMode::Preview
                                                    };
                                                    this.set_description_mode(next, window, cx);
                                                    if next == DescriptionMode::Edit {
                                                        this.description_input
                                                            .update(cx, |inp, cx| {
                                                                inp.focus(window, cx);
                                                            });
                                                    }
                                                })),
                                        ),
                                )
                                .child(match self.description_mode {
                                    DescriptionMode::Edit => Textarea::new(&self.description_input)
                                        .small()
                                        .h(px(210.))
                                        .into_any_element(),
                                    DescriptionMode::Preview => div()
                                        .p_3()
                                        .rounded(cx.theme().radius)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .bg(cx.theme().popover)
                                        .text_sm()
                                        .child(if live_description.trim().is_empty() {
                                            div()
                                                .id("inspector-empty-description")
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .cursor_pointer()
                                                .hover(|s| s.text_color(cx.theme().foreground))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.set_description_mode(
                                                        DescriptionMode::Edit,
                                                        window,
                                                        cx,
                                                    );
                                                    this.description_input
                                                        .update(cx, |inp, cx| {
                                                            inp.focus(window, cx);
                                                        });
                                                }))
                                                .child("No description provided. Click to add Markdown…")
                                                .into_any_element()
                                        } else {
                                            TextView::markdown(
                                                SharedString::from(format!(
                                                    "inspector-md-{}-{}",
                                                    item.id, self.preview_revision
                                                )),
                                                live_description,
                                            )
                                            .selectable(true)
                                            .into_any_element()
                                        })
                                        .into_any_element(),
                                }),
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
