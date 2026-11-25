use std::cell::Cell;
use std::collections::HashSet;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Selectable as _, Sizable as _,
    StyledExt as _, WindowExt as _, h_flex, notification::Notification, v_flex,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, TestSupportExt as _, Window, div, prelude::FluentBuilder as _, px,
};

use crate::commands::{CreateFeature, CreateWorkItem, UndoableCommand};
use crate::domain::{
    AssigneeId, ItemId, ItemKind, ItemStatus, MAX_FEATURE_DEPTH, Priority, ProjectId, TagId,
    clamped_depth_badge, suggest_link_label, validate_item_link, visual_indent_px,
};
use crate::state::{AppState, CenterViewTab, ProjectStore};
use crate::ui::{
    PrioritySelectOption, StatusSelectOption, all_priority_options, all_status_options,
    domain_tag_chip, parse_hex_or,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickAddDefaults {
    pub default_kind: ItemKind,
    pub work_item_feature_ids: Vec<ItemId>,
    pub feature_parent_id: Option<ItemId>,
}

/// Computes smart placement defaults for `Cmd+N` based on current view tab & selection (`UI_SPEC.md` §6).
pub fn compute_quick_add_defaults(store: &ProjectStore) -> Option<QuickAddDefaults> {
    let snapshot = store.snapshot()?;
    if store.active_tab() == CenterViewTab::Unallocated {
        return Some(QuickAddDefaults {
            default_kind: ItemKind::WorkItem,
            work_item_feature_ids: Vec::new(),
            feature_parent_id: None,
        });
    }

    let selected_active = store
        .selected_item_id()
        .and_then(|id| snapshot.item(id))
        .filter(|item| item.project_id == snapshot.project.id && item.is_active());

    let context_feature_id = match selected_active {
        Some(item) if item.kind == ItemKind::Feature => Some(item.id),
        Some(item) if item.kind == ItemKind::WorkItem => {
            let attached = snapshot.features_for_work_item(item.id);
            if let Some(focused) = store.focused_feature_id()
                && attached.iter().any(|f| f.id == focused)
            {
                Some(focused)
            } else {
                attached.first().map(|f| f.id)
            }
        }
        _ => store.focused_feature_id().filter(|fid| {
            snapshot.item(*fid).is_some_and(|f| {
                f.project_id == snapshot.project.id
                    && f.is_active()
                    && f.kind == ItemKind::Feature
            })
        }),
    };

    let feature_parent_id =
        context_feature_id.filter(|&fid| snapshot.feature_depth(fid) + 1 < MAX_FEATURE_DEPTH);

    Some(QuickAddDefaults {
        default_kind: ItemKind::WorkItem,
        work_item_feature_ids: context_feature_id.into_iter().collect(),
        feature_parent_id,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AssigneeSelectOption {
    id: Option<AssigneeId>,
    label: String,
    handle: Option<String>,
    color: Option<String>,
}

impl SelectItem for AssigneeSelectOption {
    type Value = Option<AssigneeId>;

    fn title(&self) -> SharedString {
        SharedString::from(self.label.clone())
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        if let (Some(handle), Some(color_hex)) = (&self.handle, &self.color) {
            let color = parse_hex_or(color_hex, cx.theme().primary);
            h_flex()
                .gap_1p5()
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
                .child(div().text_xs().child(self.label.clone()))
                .into_any_element()
        } else {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(self.label.clone())
                .into_any_element()
        }
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParentFeatureOption {
    id: Option<ItemId>,
    label: String,
}

impl SelectItem for ParentFeatureOption {
    type Value = Option<ItemId>;

    fn title(&self) -> SharedString {
        SharedString::from(self.label.clone())
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
}

pub struct QuickAddModalView {
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    project_id: ProjectId,
    kind: ItemKind,
    title_input: Entity<InputState>,
    desc_input: Entity<TextareaState>,
    link_url_input: Entity<InputState>,
    link_label_input: Entity<InputState>,
    link_label_edited_manually: bool,
    suppress_link_label_change: bool,
    last_auto_link_label: Option<String>,
    status_select: Entity<SelectState<Vec<StatusSelectOption>>>,
    priority_select: Entity<SelectState<Vec<PrioritySelectOption>>>,
    assignee_select: Entity<SelectState<Vec<AssigneeSelectOption>>>,
    parent_select: Entity<SelectState<Vec<ParentFeatureOption>>>,
    selected_status: ItemStatus,
    selected_priority: Priority,
    selected_assignee_id: Option<AssigneeId>,
    selected_feature_ids: HashSet<ItemId>,
    selected_parent_id: Option<ItemId>,
    selected_tag_ids: HashSet<TagId>,
    feature_picker_open: bool,
    error: Option<String>,
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl QuickAddModalView {
    fn new(
        app_state: Entity<AppState>,
        store: Entity<ProjectStore>,
        defaults: QuickAddDefaults,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let snapshot = store
            .read(cx)
            .snapshot()
            .expect("QuickAddModalView requires an active ProjectSnapshot")
            .clone();
        let project_id = snapshot.project.id;

        let title_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Item title (press Enter to create)…")
        });
        let desc_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .soft_wrap(true)
                .placeholder("Optional Markdown description…")
        });
        let link_url_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Optional URL (Jira, GitHub, Azure DevOps)…")
        });
        let link_label_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Auto-filled label (e.g. ABC-123)")
        });

        let status_options = all_status_options();
        let status_ix = status_options
            .iter()
            .position(|opt| opt.0 == ItemStatus::Todo)
            .map(IndexPath::new);
        let status_select =
            cx.new(|cx| SelectState::new(status_options, status_ix, window, cx));

        let priority_options = all_priority_options();
        let priority_ix = priority_options
            .iter()
            .position(|opt| opt.0 == Priority::None)
            .map(IndexPath::new);
        let priority_select =
            cx.new(|cx| SelectState::new(priority_options, priority_ix, window, cx));

        let mut assignee_options = vec![AssigneeSelectOption {
            id: None,
            label: "Unassigned".to_string(),
            handle: None,
            color: None,
        }];
        for a in snapshot.active_assignees() {
            assignee_options.push(AssigneeSelectOption {
                id: Some(a.id),
                label: a.name.clone(),
                handle: Some(a.handle.clone()),
                color: Some(a.color.clone()),
            });
        }
        let assignee_select =
            cx.new(|cx| SelectState::new(assignee_options, Some(IndexPath::new(0)), window, cx));

        let mut parent_options = vec![ParentFeatureOption {
            id: None,
            label: "Root Feature (Level 1)".to_string(),
        }];
        for (feat, depth) in snapshot.active_features_dfs() {
            if depth + 1 >= MAX_FEATURE_DEPTH {
                continue;
            }
            let indent = "  ".repeat(depth.min(6));
            parent_options.push(ParentFeatureOption {
                id: Some(feat.id),
                label: format!("{indent}{} · {} (L{})", feat.item_key, feat.title, depth + 1),
            });
        }
        let parent_ix = parent_options
            .iter()
            .position(|opt| opt.id == defaults.feature_parent_id)
            .or(Some(0))
            .map(IndexPath::new);
        let parent_select = cx.new(|cx| SelectState::new(parent_options, parent_ix, window, cx));

        let title_sub = cx.subscribe_in(
            &title_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.error = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.submit(window, cx);
                }
                _ => {}
            },
        );

        let status_sub = cx.subscribe_in(
            &status_select,
            window,
            |this, _, event: &SelectEvent<Vec<StatusSelectOption>>, _, cx| {
                if let SelectEvent::Confirm(Some(status)) = event {
                    this.selected_status = *status;
                    cx.notify();
                }
            },
        );

        let priority_sub = cx.subscribe_in(
            &priority_select,
            window,
            |this, _, event: &SelectEvent<Vec<PrioritySelectOption>>, _, cx| {
                if let SelectEvent::Confirm(Some(priority)) = event {
                    this.selected_priority = *priority;
                    cx.notify();
                }
            },
        );

        let assignee_sub = cx.subscribe_in(
            &assignee_select,
            window,
            |this, _, event: &SelectEvent<Vec<AssigneeSelectOption>>, _, cx| {
                if let SelectEvent::Confirm(Some(aid)) = event {
                    this.selected_assignee_id = *aid;
                    cx.notify();
                }
            },
        );

        let parent_sub = cx.subscribe_in(
            &parent_select,
            window,
            |this, _, event: &SelectEvent<Vec<ParentFeatureOption>>, _, cx| {
                if let SelectEvent::Confirm(Some(pid)) = event {
                    this.selected_parent_id = *pid;
                    cx.notify();
                }
            },
        );

        let link_url_sub = cx.subscribe_in(
            &link_url_input,
            window,
            |this, inp, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.error = None;
                    let url = inp.read(cx).value().to_string();
                    let current_label = this.link_label_input.read(cx).value().to_string();
                    if url.trim().is_empty() {
                        this.link_label_edited_manually = false;
                        this.last_auto_link_label = None;
                        this.suppress_link_label_change = true;
                        this.link_label_input
                            .update(cx, |lbl, cx| lbl.set_value("", window, cx));
                        this.suppress_link_label_change = false;
                    } else if !this.link_label_edited_manually || current_label.trim().is_empty() {
                        let suggested = suggest_link_label(&url).unwrap_or_default();
                        this.last_auto_link_label = if suggested.is_empty() {
                            None
                        } else {
                            Some(suggested.clone())
                        };
                        this.suppress_link_label_change = true;
                        this.link_label_input
                            .update(cx, |lbl, cx| lbl.set_value(suggested, window, cx));
                        this.suppress_link_label_change = false;
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.submit(window, cx);
                }
                _ => {}
            },
        );

        let link_label_sub = cx.subscribe_in(
            &link_label_input,
            window,
            |this, inp, event: &InputEvent, window, cx| {
                if this.suppress_link_label_change {
                    return;
                }
                match event {
                    InputEvent::Change => {
                        this.error = None;
                        let val = inp.read(cx).value().to_string();
                        if val.trim().is_empty() {
                            this.link_label_edited_manually = false;
                        } else if this.last_auto_link_label.as_deref() != Some(val.as_str()) {
                            this.link_label_edited_manually = true;
                        }
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => {
                        this.submit(window, cx);
                    }
                    _ => {}
                }
            },
        );

        Self {
            app_state,
            store,
            project_id,
            kind: defaults.default_kind,
            title_input,
            desc_input,
            link_url_input,
            link_label_input,
            link_label_edited_manually: false,
            suppress_link_label_change: false,
            last_auto_link_label: None,
            status_select,
            priority_select,
            assignee_select,
            parent_select,
            selected_status: ItemStatus::Todo,
            selected_priority: Priority::None,
            selected_assignee_id: None,
            selected_feature_ids: defaults.work_item_feature_ids.into_iter().collect(),
            selected_parent_id: defaults.feature_parent_id,
            selected_tag_ids: HashSet::new(),
            feature_picker_open: false,
            error: None,
            busy: false,
            _subscriptions: vec![
                title_sub,
                status_sub,
                priority_sub,
                assignee_sub,
                parent_sub,
                link_url_sub,
                link_label_sub,
            ],
        }
    }

    pub fn link_url_input(&self) -> &Entity<InputState> {
        &self.link_url_input
    }

    pub fn link_label_input(&self) -> &Entity<InputState> {
        &self.link_label_input
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let title = self.title_input.read(cx).value().trim().to_string();
        if title.is_empty() {
            self.error = Some("Title cannot be empty".to_string());
            cx.notify();
            return;
        }

        let link_url = self.link_url_input.read(cx).value().to_string();
        let link_label = self.link_label_input.read(cx).value().to_string();
        let link = if link_url.trim().is_empty() {
            None
        } else {
            match validate_item_link(&link_url, &link_label) {
                Ok(validated) => Some(validated),
                Err(err) => {
                    self.error = Some(err.to_string());
                    cx.notify();
                    return;
                }
            }
        };

        let description = self.desc_input.read(cx).value().to_string();
        let Some(snapshot) = self.store.read(cx).snapshot() else {
            return;
        };

        let ordered_tag_ids: Vec<TagId> = snapshot
            .tags
            .iter()
            .filter(|t| self.selected_tag_ids.contains(&t.id))
            .map(|t| t.id)
            .collect();

        let command: Box<dyn UndoableCommand> = match self.kind {
            ItemKind::WorkItem => {
                let ordered_feature_ids: Vec<ItemId> = snapshot
                    .active_features_dfs()
                    .into_iter()
                    .filter(|(f, _)| self.selected_feature_ids.contains(&f.id))
                    .map(|(f, _)| f.id)
                    .collect();

                Box::new(
                    CreateWorkItem::new(
                        self.project_id,
                        title,
                        description,
                        self.selected_status,
                        self.selected_priority,
                        ordered_feature_ids,
                    )
                    .with_assignee(self.selected_assignee_id)
                    .with_link(link)
                    .with_tags(ordered_tag_ids),
                )
            }
            ItemKind::Feature => Box::new(
                CreateFeature::new(
                    self.project_id,
                    self.selected_parent_id,
                    title,
                    description,
                    self.selected_status,
                    self.selected_priority,
                )
                .with_assignee(self.selected_assignee_id)
                .with_link(link)
                .with_tags(ordered_tag_ids),
            ),
        };

        self.busy = true;
        self.error = None;
        cx.notify();

        let task = self
            .app_state
            .update(cx, |app, cx| app.execute_command(command, cx));
        let view = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let res = task.await;
                match res {
                    Ok(effect) => {
                        let _ = cx.update(|window, cx| {
                            window.close_dialog(cx);
                            if let Some(msg) = effect.toast {
                                window.push_notification(Notification::success(msg), cx);
                            }
                        });
                    }
                    Err(err) => {
                        let _ = view.update(cx, |this, cx| {
                            this.busy = false;
                            this.error = Some(err.to_string());
                            cx.notify();
                        });
                    }
                }
            })
            .detach();
    }
}

impl Render for QuickAddModalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(snapshot) = self.store.read(cx).snapshot().cloned() else {
            return v_flex().into_any_element();
        };

        let can_submit = !self.busy && !self.title_input.read(cx).value().trim().is_empty();
        let danger = cx.theme().danger;
        let danger_bg = danger.opacity(if cx.theme().is_dark() { 0.16 } else { 0.10 });
        let danger_border = danger.opacity(if cx.theme().is_dark() { 0.42 } else { 0.30 });

        let all_features = snapshot.active_features_dfs();
        let selected_features: Vec<_> = all_features
            .iter()
            .filter(|(f, _)| self.selected_feature_ids.contains(&f.id))
            .map(|(f, _)| (*f).clone())
            .collect();

        v_flex()
            .w_full()
            .gap_3()
            // Kind segmented toggle + project pill
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("quick-add-kind-wi")
                                    .outline()
                                    .xsmall()
                                    .selected(self.kind == ItemKind::WorkItem)
                                    .icon(IconName::CircleCheck)
                                    .label("Work Item")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.kind = ItemKind::WorkItem;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("quick-add-kind-feature")
                                    .outline()
                                    .xsmall()
                                    .selected(self.kind == ItemKind::Feature)
                                    .icon(IconName::Folder)
                                    .label("Feature")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.kind = ItemKind::Feature;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        Tag::secondary()
                            .xsmall()
                            .child(format!("{} · {}", snapshot.project.key_prefix, snapshot.project.name)),
                    ),
            )
            .when_some(self.error.clone(), |this, err| {
                this.child(
                    h_flex()
                        .w_full()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .items_start()
                        .rounded(cx.theme().radius)
                        .bg(danger_bg)
                        .border_1()
                        .border_color(danger_border)
                        .child(Icon::new(IconName::TriangleAlert).small().text_color(danger))
                        .child(div().flex_1().text_xs().text_color(danger).child(err)),
                )
            })
            // Title
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xs().font_medium().child("Title"))
                    .child(
                        Input::new(&self.title_input)
                            .id("quick-add-title")
                            .small()
                            .disabled(self.busy),
                    ),
            )
            // Contextual Placement
            .child(match self.kind {
                ItemKind::WorkItem => v_flex()
                    .gap_1p5()
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .font_medium()
                                    .child(format!("Features ({})", selected_features.len())),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .when(!selected_features.is_empty(), |this| {
                                        this.child(
                                            Button::new("quick-add-wi-clear-features")
                                                .ghost()
                                                .xsmall()
                                                .label("Unallocate")
                                                .disabled(self.busy)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.selected_feature_ids.clear();
                                                    cx.notify();
                                                })),
                                        )
                                    })
                                    .child(
                                        Button::new("quick-add-wi-features-toggle")
                                            .outline()
                                            .xsmall()
                                            .label(if self.feature_picker_open {
                                                "Hide Features"
                                            } else {
                                                "Choose Features"
                                            })
                                            .dropdown_caret(true)
                                            .disabled(self.busy || all_features.is_empty())
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.feature_picker_open = !this.feature_picker_open;
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    )
                    .child(if selected_features.is_empty() {
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(Tag::warning().xsmall().outline().child("Unallocated"))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Will land in the Project's Unallocated inbox."),
                            )
                            .into_any_element()
                    } else {
                        h_flex()
                            .flex_wrap()
                            .gap_1()
                            .children(selected_features.into_iter().map(|feat| {
                                let fid = feat.id;
                                let fnum = feat.item_number as usize;
                                h_flex()
                                    .px_2()
                                    .py_0p5()
                                    .gap_1()
                                    .items_center()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().secondary)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .child(format!("{} · {}", feat.item_key, feat.title)),
                                    )
                                    .child(
                                        Button::new(("quick-add-wi-remove-feature", fnum))
                                            .ghost()
                                            .xsmall()
                                            .compact()
                                            .accessibility_label("Remove feature attachment")
                                            .icon(IconName::Close)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.selected_feature_ids.remove(&fid);
                                                cx.notify();
                                            })),
                                    )
                            }))
                            .into_any_element()
                    })
                    .when(self.feature_picker_open && !all_features.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .w_full()
                                .max_h(px(150.))
                                .p_1p5()
                                .gap_0p5()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().background)
                                .overflow_y_scrollbar()
                                .id("quick-add-feature-picker-scroll")
                                .children(all_features.iter().map(|(feat, depth)| {
                                    let fid = feat.id;
                                    let fnum = feat.item_number as usize;
                                    let checked = self.selected_feature_ids.contains(&fid);
                                    let indent = visual_indent_px(*depth).min(72.0);
                                    let depth_badge = clamped_depth_badge(*depth);

                                    ListItem::new(("quick-add-wi-feature-li", fnum))
                                        .w_full()
                                        .pl(px(6.0 + indent))
                                        .pr_2()
                                        .py_1()
                                        .rounded(cx.theme().radius * 0.75)
                                        .selected(checked)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if !this.selected_feature_ids.remove(&fid) {
                                                this.selected_feature_ids.insert(fid);
                                            }
                                            cx.notify();
                                        }))
                                        .child(
                                            h_flex()
                                                .id(("quick-add-wi-feature", fnum))
                                                .test_support()
                                                .w_full()
                                                .gap_2()
                                                .items_center()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .size(px(13.))
                                                        .rounded(px(3.))
                                                        .border_1()
                                                        .border_color(if checked {
                                                            cx.theme().primary
                                                        } else {
                                                            cx.theme().border
                                                        })
                                                        .bg(if checked {
                                                            cx.theme().primary
                                                        } else {
                                                            cx.theme().background
                                                        })
                                                        .when(checked, |this| {
                                                            this.child(
                                                                Icon::new(IconName::Check)
                                                                    .xsmall()
                                                                    .text_color(
                                                                        cx.theme()
                                                                            .primary_foreground,
                                                                    ),
                                                            )
                                                        }),
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
                                                        .child(feat.title.clone()),
                                                )
                                                .when_some(depth_badge, |this, badge| {
                                                    this.child(
                                                        div()
                                                            .text_size(px(10.))
                                                            .text_color(
                                                                cx.theme().muted_foreground,
                                                            )
                                                            .child(badge),
                                                    )
                                                }),
                                        )
                                })),
                        )
                    })
                    .into_any_element(),
                ItemKind::Feature => v_flex()
                    .gap_1()
                    .child(div().text_xs().font_medium().child("Parent Feature"))
                    .child(
                        Select::new(&self.parent_select)
                            .id("quick-add-parent-select")
                            .small()
                            .disabled(self.busy),
                    )
                    .into_any_element(),
            })
            // Status / Priority / Assignee
            .child(
                h_flex()
                    .w_full()
                    .gap_2p5()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(div().text_xs().font_medium().child("Status"))
                            .child(
                                Select::new(&self.status_select)
                                    .small()
                                    .disabled(self.busy),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(div().text_xs().font_medium().child("Priority"))
                            .child(
                                Select::new(&self.priority_select)
                                    .small()
                                    .disabled(self.busy),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(div().text_xs().font_medium().child("Assignee"))
                            .child(
                                Select::new(&self.assignee_select)
                                    .small()
                                    .disabled(self.busy),
                            ),
                    ),
            )
            // Tags
            .when(!snapshot.tags.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_xs().font_medium().child("Tags"))
                        .child(
                            h_flex().flex_wrap().gap_1().children(
                                snapshot.tags.iter().enumerate().map(|(ix, tag)| {
                                    let tid = tag.id;
                                    let selected = self.selected_tag_ids.contains(&tid);
                                    div()
                                        .id(("quick-add-tag", ix))
                                        .test_support()
                                        .px_1()
                                        .py_0p5()
                                        .rounded(cx.theme().radius)
                                        .border_1()
                                        .border_color(if selected {
                                            cx.theme().primary
                                        } else {
                                            cx.theme().transparent
                                        })
                                        .bg(if selected {
                                            cx.theme().accent
                                        } else {
                                            cx.theme().transparent
                                        })
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if !this.busy {
                                                if !this.selected_tag_ids.remove(&tid) {
                                                    this.selected_tag_ids.insert(tid);
                                                }
                                                cx.notify();
                                            }
                                        }))
                                        .child(domain_tag_chip(tag, cx))
                                }),
                            ),
                        ),
                )
            })
            // Link
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xs().font_medium().child("Link"))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.link_url_input)
                                        .id("quick-add-link-url")
                                        .small()
                                        .cleanable(true)
                                        .disabled(self.busy),
                                ),
                            )
                            .child(
                                div().w(px(150.)).child(
                                    Input::new(&self.link_label_input)
                                        .id("quick-add-link-label")
                                        .small()
                                        .cleanable(true)
                                        .disabled(self.busy),
                                ),
                            ),
                    ),
            )
            // Description
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xs().font_medium().child("Description"))
                    .child(
                        Textarea::new(&self.desc_input)
                            .small()
                            .disabled(self.busy),
                    ),
            )
            // Footer
            .child(
                DialogFooter::new()
                    .pt_1()
                    .child(
                        Button::new("quick-add-cancel")
                            .outline()
                            .small()
                            .label("Cancel")
                            .disabled(self.busy)
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("quick-add-submit")
                            .primary()
                            .small()
                            .label(match self.kind {
                                ItemKind::WorkItem => "Create Work Item",
                                ItemKind::Feature => "Create Feature",
                            })
                            .disabled(!can_submit)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}

pub fn open_quick_add_modal(
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }

    let Some(defaults) = compute_quick_add_defaults(store.read(cx)) else {
        window.push_notification(
            Notification::warning("Create or open a Project before adding items."),
            cx,
        );
        return;
    };

    let view = cx.new(|cx| QuickAddModalView::new(app_state, store, defaults, window, cx));
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            let input = view.read(cx).title_input.clone();
            input.update(cx, |inp, cx| inp.focus(window, cx));
        }
        let view = view.clone();
        dialog
            .width(px(540.))
            .title("Quick Add")
            .content(move |content, _, _| content.child(view.clone()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ProjectStatus;
    use crate::state::ProjectStore;

    #[test]
    fn quick_add_defaults_follow_active_tab_and_selection_context() {
        let mut store = ProjectStore::new_with_seed();
        let snap = store.snapshot().unwrap().clone();
        let f1 = snap.child_features(None)[0].id;
        let f2 = snap.child_features(Some(f1))[0].id;
        let w5 = snap
            .active_items()
            .into_iter()
            .find(|i| i.item_key == "PRO-5")
            .unwrap()
            .id;

        // Outline with Feature f2 selected -> defaults WI to [f2] and Feature parent to Some(f2)
        store.set_active_tab(CenterViewTab::Outline);
        store.select_item(Some(f2));
        let defaults_f2 = compute_quick_add_defaults(&store).unwrap();
        assert_eq!(defaults_f2.default_kind, ItemKind::WorkItem);
        assert_eq!(defaults_f2.work_item_feature_ids, vec![f2]);
        assert_eq!(defaults_f2.feature_parent_id, Some(f2));

        // Shared Work Item w5 selected while focused on f2 -> prefers f2
        store.select_item(Some(w5));
        let defaults_w5 = compute_quick_add_defaults(&store).unwrap();
        assert_eq!(defaults_w5.work_item_feature_ids, vec![f2]);
        assert_eq!(defaults_w5.feature_parent_id, Some(f2));

        // Unallocated tab -> defaults WI to Unallocated ([]) and Feature parent to None (Root)
        store.set_active_tab(CenterViewTab::Unallocated);
        let defaults_unalloc = compute_quick_add_defaults(&store).unwrap();
        assert_eq!(defaults_unalloc.default_kind, ItemKind::WorkItem);
        assert!(defaults_unalloc.work_item_feature_ids.is_empty());
        assert_eq!(defaults_unalloc.feature_parent_id, None);
    }

    #[test]
    fn quick_add_defaults_returns_none_without_project() {
        let store = ProjectStore::new_from_preferences(&Default::default());
        assert!(compute_quick_add_defaults(&store).is_none());
        let _ = ProjectStatus::Active;
    }
}
