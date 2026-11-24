use std::cell::Cell;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _, StyledExt as _,
    WindowExt as _, h_flex, notification::Notification, v_flex,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, TestSupportExt as _, Window, div, prelude::FluentBuilder as _, px,
};

use crate::commands::{
    ArchiveAssignee, CreateFeature, CreateProject, DeleteTag, UndoableCommand, UpdateProject,
    UpsertAssignee, UpsertTag,
};
use crate::db::assignee_repo::{validate_assignee_handle, validate_assignee_name};
use crate::db::project_repo::{validate_hex_color, validate_key_prefix, validate_project_name};
use crate::db::tag_repo::validate_tag_name;
use crate::domain::{
    AssigneeId, ItemId, ItemStatus, Priority, Project, ProjectId, ProjectStatus, TagId,
};
use crate::state::{AppState, ProjectStore};
use crate::ui::{
    PrioritySelectOption, StatusSelectOption, all_priority_options, all_status_options,
    assignee_pill, domain_tag_chip, parse_hex_or,
};

const PROJECT_COLOR_PRESETS: &[(&str, &str)] = &[
    ("#6366F1", "Indigo"),
    ("#8B5CF6", "Violet"),
    ("#EC4899", "Pink"),
    ("#EF4444", "Red"),
    ("#F59E0B", "Amber"),
    ("#10B981", "Emerald"),
    ("#0EA5E9", "Sky"),
    ("#64748B", "Slate"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectStatusOption(ProjectStatus);

impl SelectItem for ProjectStatusOption {
    type Value = ProjectStatus;

    fn title(&self) -> SharedString {
        SharedString::from(self.0.label())
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}

fn project_status_options() -> Vec<ProjectStatusOption> {
    vec![
        ProjectStatusOption(ProjectStatus::Active),
        ProjectStatusOption(ProjectStatus::Paused),
        ProjectStatusOption(ProjectStatus::Completed),
        ProjectStatusOption(ProjectStatus::Archived),
    ]
}

fn suggest_key_prefix(name: &str) -> String {
    let words: Vec<String> = name
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_uppercase())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect();

    let raw = if words.len() >= 2 {
        words
            .iter()
            .filter_map(|w| w.chars().next())
            .collect::<String>()
    } else {
        words.into_iter().next().unwrap_or_default()
    };

    let mut out = String::new();
    for ch in raw.chars() {
        if out.is_empty() && !ch.is_ascii_uppercase() {
            continue;
        }
        out.push(ch);
        if out.len() >= 4 {
            break;
        }
    }
    out
}

#[derive(Clone)]
enum ProjectModalMode {
    Create,
    Edit {
        original: Project,
        item_count: usize,
    },
}

pub struct ProjectModalView {
    app_state: Entity<AppState>,
    mode: ProjectModalMode,
    name_input: Entity<InputState>,
    prefix_input: Entity<InputState>,
    prefix_edited_manually: bool,
    suppress_prefix_change: bool,
    desc_input: Entity<TextareaState>,
    status_select: Entity<SelectState<Vec<ProjectStatusOption>>>,
    selected_status: ProjectStatus,
    selected_color: String,
    confirm_prefix_rewrite: bool,
    error: Option<String>,
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl ProjectModalView {
    fn new(
        app_state: Entity<AppState>,
        mode: ProjectModalMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (initial_name, initial_prefix, initial_desc, initial_status, initial_color) =
            match &mode {
                ProjectModalMode::Create => (
                    String::new(),
                    String::new(),
                    String::new(),
                    ProjectStatus::Active,
                    PROJECT_COLOR_PRESETS[0].0.to_string(),
                ),
                ProjectModalMode::Edit { original, .. } => (
                    original.name.clone(),
                    original.key_prefix.clone(),
                    original.description.clone(),
                    original.status,
                    original.color.clone(),
                ),
            };

        let is_edit = matches!(mode, ProjectModalMode::Edit { .. });

        let name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("e.g., ProTrack v1")
                .default_value(initial_name)
        });
        let prefix_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("e.g., PRO")
                .default_value(initial_prefix)
        });
        let desc_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .soft_wrap(true)
                .placeholder("Optional project summary or goals…")
                .default_value(initial_desc)
        });

        let status_options = project_status_options();
        let initial_status_ix = status_options
            .iter()
            .position(|opt| opt.0 == initial_status)
            .map(IndexPath::new);
        let status_select =
            cx.new(|cx| SelectState::new(status_options, initial_status_ix, window, cx));

        let name_sub = cx.subscribe_in(
            &name_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.error = None;
                    if matches!(this.mode, ProjectModalMode::Create) && !this.prefix_edited_manually
                    {
                        let current_name = this.name_input.read(cx).value();
                        let suggested = suggest_key_prefix(&current_name);
                        this.suppress_prefix_change = true;
                        this.prefix_input.update(cx, |inp, cx| {
                            inp.set_value(suggested, window, cx);
                        });
                        this.suppress_prefix_change = false;
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.submit(window, cx);
                }
                _ => {}
            },
        );

        let prefix_sub = cx.subscribe_in(
            &prefix_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    if !this.suppress_prefix_change {
                        this.prefix_edited_manually = true;
                        this.confirm_prefix_rewrite = false;
                        this.error = None;
                        cx.notify();
                    }
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
            |this, _, event: &SelectEvent<Vec<ProjectStatusOption>>, _, cx| {
                if let SelectEvent::Confirm(Some(status)) = event {
                    this.selected_status = *status;
                    cx.notify();
                }
            },
        );

        Self {
            app_state,
            mode,
            name_input,
            prefix_input,
            prefix_edited_manually: is_edit,
            suppress_prefix_change: false,
            desc_input,
            status_select,
            selected_status: initial_status,
            selected_color: initial_color,
            confirm_prefix_rewrite: false,
            error: None,
            busy: false,
            _subscriptions: vec![name_sub, prefix_sub, status_sub],
        }
    }

    fn normalized_prefix(&self, cx: &App) -> String {
        self.prefix_input
            .read(cx)
            .value()
            .trim()
            .to_ascii_uppercase()
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }

        let name = match validate_project_name(&self.name_input.read(cx).value()) {
            Ok(v) => v,
            Err(err) => {
                self.error = Some(err.to_string());
                cx.notify();
                return;
            }
        };

        let raw_prefix = self.normalized_prefix(cx);
        let key_prefix = match validate_key_prefix(&raw_prefix) {
            Ok(v) => v,
            Err(err) => {
                self.error = Some(err.to_string());
                cx.notify();
                return;
            }
        };

        let color = match validate_hex_color(&self.selected_color) {
            Ok(v) => v,
            Err(err) => {
                self.error = Some(err.to_string());
                cx.notify();
                return;
            }
        };

        if let ProjectModalMode::Edit { original, .. } = &self.mode
            && key_prefix != original.key_prefix
            && !self.confirm_prefix_rewrite
        {
            self.error = Some(
                "Please confirm rewriting existing item keys before saving the new prefix."
                    .to_string(),
            );
            cx.notify();
            return;
        }

        let description = self.desc_input.read(cx).value().to_string();
        let status = self.selected_status;

        let command: Box<dyn UndoableCommand> = match &self.mode {
            ProjectModalMode::Create => {
                Box::new(CreateProject::new(name, key_prefix, description, color, status))
            }
            ProjectModalMode::Edit { original, .. } => Box::new(UpdateProject::new(
                original.id,
                name,
                key_prefix,
                description,
                color,
                status,
            )),
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
                let outcome = task.await;
                match outcome {
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

impl Render for ProjectModalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let normalized_prefix = self.normalized_prefix(cx);
        let prefix_validation = if normalized_prefix.is_empty() {
            None
        } else {
            validate_key_prefix(&normalized_prefix).err()
        };

        let rewrite_info = match &self.mode {
            ProjectModalMode::Edit {
                original,
                item_count,
            } if !normalized_prefix.is_empty()
                && normalized_prefix != original.key_prefix
                && prefix_validation.is_none() =>
            {
                Some((original.key_prefix.clone(), *item_count))
            }
            _ => None,
        };

        let name_ok = validate_project_name(&self.name_input.read(cx).value()).is_ok();
        let prefix_ok = validate_key_prefix(&normalized_prefix).is_ok();
        let rewrite_ok = rewrite_info.is_none() || self.confirm_prefix_rewrite;
        let can_submit = !self.busy && name_ok && prefix_ok && rewrite_ok;

        let submit_label = match &self.mode {
            ProjectModalMode::Create => "Create Project",
            ProjectModalMode::Edit { .. } => "Save Changes",
        };

        let danger = cx.theme().danger;
        let danger_bg = danger.opacity(if cx.theme().is_dark() { 0.16 } else { 0.10 });
        let danger_border = danger.opacity(if cx.theme().is_dark() { 0.42 } else { 0.30 });

        let warning = cx.theme().warning;
        let warning_bg = warning.opacity(if cx.theme().is_dark() { 0.16 } else { 0.10 });
        let warning_border = warning.opacity(if cx.theme().is_dark() { 0.42 } else { 0.30 });

        v_flex()
            .w_full()
            .gap_3()
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
            // Name
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xs().font_medium().child("Project Name"))
                    .child(
                        Input::new(&self.name_input)
                            .id("project-modal-name")
                            .small()
                            .disabled(self.busy),
                    ),
            )
            // Key Prefix + Status
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(
                                h_flex()
                                    .items_center()
                                    .justify_between()
                                    .child(div().text_xs().font_medium().child("Key Prefix"))
                                    .when(prefix_ok, |this| {
                                        this.child(
                                            div()
                                                .text_size(px(10.))
                                                .font_family(cx.theme().mono_font_family.clone())
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!("{normalized_prefix}-1")),
                                        )
                                    }),
                            )
                            .child(
                                Input::new(&self.prefix_input)
                                    .id("project-modal-prefix")
                                    .small()
                                    .disabled(self.busy),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(if prefix_validation.is_some() {
                                        danger
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(match prefix_validation {
                                        Some(err) => err.to_string(),
                                        None => "2–10 chars: [A-Z][A-Z0-9]* (normalized uppercase)"
                                            .to_string(),
                                    }),
                            ),
                    )
                    .child(
                        v_flex()
                            .w(px(150.))
                            .gap_1()
                            .child(div().text_xs().font_medium().child("Status"))
                            .child(
                                Select::new(&self.status_select)
                                    .small()
                                    .disabled(self.busy),
                            ),
                    ),
            )
            // Atomic key rewrite confirmation banner
            .when_some(rewrite_info, |this, (old_prefix, item_count)| {
                this.child(
                    v_flex()
                        .w_full()
                        .p_3()
                        .gap_2()
                        .rounded(cx.theme().radius)
                        .bg(warning_bg)
                        .border_1()
                        .border_color(warning_border)
                        .child(
                            h_flex()
                                .gap_2()
                                .items_start()
                                .child(
                                    Icon::new(IconName::TriangleAlert)
                                        .small()
                                        .text_color(warning),
                                )
                                .child(
                                    div().flex_1().text_xs().text_color(cx.theme().foreground).child(
                                        format!(
                                            "Renaming key prefix from {old_prefix} to {normalized_prefix} will atomically rewrite all {item_count} item key(s) in this project (e.g., {old_prefix}-1 → {normalized_prefix}-1)."
                                        ),
                                    ),
                                ),
                        )
                        .child(
                            Switch::new("project-modal-confirm-rewrite")
                                .small()
                                .checked(self.confirm_prefix_rewrite)
                                .disabled(self.busy)
                                .label("Confirm rewriting existing item keys")
                                .on_change(cx.listener(|this, checked, _, cx| {
                                    this.confirm_prefix_rewrite = *checked;
                                    this.error = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            // Color swatch picker
            .child(
                v_flex()
                    .gap_1p5()
                    .child(div().text_xs().font_medium().child("Project Color"))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .children(PROJECT_COLOR_PRESETS.iter().enumerate().map(
                                |(ix, (hex, label))| {
                                    let is_selected =
                                        self.selected_color.eq_ignore_ascii_case(hex);
                                    let swatch = parse_hex_or(hex, cx.theme().primary);
                                    let hex_owned = (*hex).to_string();

                                    div()
                                        .id(("project-color-swatch", ix))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .size(px(24.))
                                        .rounded(px(999.))
                                        .cursor_pointer()
                                        .border_2()
                                        .border_color(if is_selected {
                                            cx.theme().foreground
                                        } else {
                                            cx.theme().transparent
                                        })
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(*label).build(window, cx)
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if !this.busy {
                                                this.selected_color = hex_owned.clone();
                                                cx.notify();
                                            }
                                        }))
                                        .child(
                                            div()
                                                .size(px(16.))
                                                .rounded(px(999.))
                                                .bg(swatch),
                                        )
                                },
                            )),
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
                            .h(px(84.))
                            .disabled(self.busy),
                    ),
            )
            // Footer
            .child(
                DialogFooter::new()
                    .pt_2()
                    .child(
                        Button::new("project-modal-cancel")
                            .outline()
                            .small()
                            .label("Cancel")
                            .disabled(self.busy)
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("project-modal-submit")
                            .primary()
                            .small()
                            .label(submit_label)
                            .disabled(!can_submit)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit(window, cx);
                            })),
                    ),
            )
    }
}

pub struct CreateFeatureModalView {
    app_state: Entity<AppState>,
    project_id: ProjectId,
    parent: Option<(ItemId, String, usize)>,
    title_input: Entity<InputState>,
    status_select: Entity<SelectState<Vec<StatusSelectOption>>>,
    priority_select: Entity<SelectState<Vec<PrioritySelectOption>>>,
    selected_status: ItemStatus,
    selected_priority: Priority,
    error: Option<String>,
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl CreateFeatureModalView {
    fn new(
        app_state: Entity<AppState>,
        project_id: ProjectId,
        parent: Option<(ItemId, String, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("e.g., Authentication & Session Sync")
        });

        let status_options = all_status_options();
        let default_status_ix = status_options
            .iter()
            .position(|o| o.0 == ItemStatus::Todo)
            .map(IndexPath::new);
        let status_select =
            cx.new(|cx| SelectState::new(status_options, default_status_ix, window, cx));

        let priority_options = all_priority_options();
        let default_priority_ix = priority_options
            .iter()
            .position(|o| o.0 == Priority::None)
            .map(IndexPath::new);
        let priority_select =
            cx.new(|cx| SelectState::new(priority_options, default_priority_ix, window, cx));

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

        Self {
            app_state,
            project_id,
            parent,
            title_input,
            status_select,
            priority_select,
            selected_status: ItemStatus::Todo,
            selected_priority: Priority::None,
            error: None,
            busy: false,
            _subscriptions: vec![title_sub, status_sub, priority_sub],
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let raw_title = self.title_input.read(cx).value();
        let trimmed = raw_title.trim().to_string();
        if trimmed.is_empty() {
            self.error = Some("Feature title cannot be empty".to_string());
            cx.notify();
            return;
        }

        self.busy = true;
        self.error = None;
        cx.notify();

        let command = Box::new(CreateFeature::new(
            self.project_id,
            self.parent.as_ref().map(|(id, _, _)| *id),
            trimmed,
            String::new(),
            self.selected_status,
            self.selected_priority,
        ));

        let task = self
            .app_state
            .update(cx, |app, cx| app.execute_command(command, cx));
        let view = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| match task.await {
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
            })
            .detach();
    }
}

impl Render for CreateFeatureModalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let can_submit = !self.busy && !self.title_input.read(cx).value().trim().is_empty();
        let placement_label = match &self.parent {
            None => "Root Feature (Level 1)".to_string(),
            Some((_, label, depth)) => format!("Under {label} (Level {})", depth + 2),
        };

        let danger = cx.theme().danger;
        let danger_bg = danger.opacity(if cx.theme().is_dark() { 0.16 } else { 0.10 });
        let danger_border = danger.opacity(if cx.theme().is_dark() { 0.42 } else { 0.30 });

        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Tag::secondary()
                            .xsmall()
                            .gap_1()
                            .child(Icon::new(IconName::Folder).xsmall())
                            .child(placement_label),
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
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xs().font_medium().child("Feature Title"))
                    .child(
                        Input::new(&self.title_input)
                            .id("create-feature-title")
                            .small()
                            .disabled(self.busy),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
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
                    ),
            )
            .child(
                DialogFooter::new()
                    .pt_2()
                    .child(
                        Button::new("create-feature-cancel")
                            .outline()
                            .small()
                            .label("Cancel")
                            .disabled(self.busy)
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("create-feature-submit")
                            .primary()
                            .small()
                            .label("Create Feature")
                            .disabled(!can_submit)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit(window, cx);
                            })),
                    ),
            )
    }
}

pub fn open_new_project_modal(
    app_state: Entity<AppState>,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }

    let view = cx.new(|cx| ProjectModalView::new(app_state, ProjectModalMode::Create, window, cx));
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            let input = view.read(cx).name_input.clone();
            input.update(cx, |inp, cx| inp.focus(window, cx));
        }
        let view = view.clone();
        dialog
            .width(px(480.))
            .title("New Project")
            .content(move |content, _, _| content.child(view.clone()))
    });
}

pub fn open_project_settings_modal(
    app_state: Entity<AppState>,
    project: Project,
    item_count: usize,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }

    let title = format!("Project Settings · {}", project.key_prefix);
    let view = cx.new(|cx| {
        ProjectModalView::new(
            app_state,
            ProjectModalMode::Edit {
                original: project,
                item_count,
            },
            window,
            cx,
        )
    });
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            let input = view.read(cx).name_input.clone();
            input.update(cx, |inp, cx| inp.focus(window, cx));
        }
        let view = view.clone();
        dialog
            .width(px(480.))
            .title(title.clone())
            .content(move |content, _, _| content.child(view.clone()))
    });
}

pub fn open_create_feature_modal(
    app_state: Entity<AppState>,
    project_id: ProjectId,
    parent: Option<(ItemId, String, usize)>,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }

    let title = if parent.is_some() {
        "New Sub-Feature"
    } else {
        "New Root Feature"
    };
    let view = cx.new(|cx| CreateFeatureModalView::new(app_state, project_id, parent, window, cx));
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            let input = view.read(cx).title_input.clone();
            input.update(cx, |inp, cx| inp.focus(window, cx));
        }
        let view = view.clone();
        dialog
            .width(px(440.))
            .title(title)
            .content(move |content, _, _| content.child(view.clone()))
    });
}

fn suggest_assignee_handle(name: &str) -> String {
    let words: Vec<String> = name
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_uppercase())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect();

    if words.len() >= 2 {
        words
            .iter()
            .filter_map(|w| w.chars().next())
            .take(4)
            .collect()
    } else if let Some(first) = words.into_iter().next() {
        first.chars().take(4).collect()
    } else {
        String::new()
    }
}

fn render_error_banner(error: Option<String>, cx: &App) -> Option<impl IntoElement> {
    let err = error?;
    let danger = cx.theme().danger;
    let danger_bg = danger.opacity(if cx.theme().is_dark() { 0.16 } else { 0.10 });
    let danger_border = danger.opacity(if cx.theme().is_dark() { 0.42 } else { 0.30 });

    Some(
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
}

pub struct AssigneesManagerModalView {
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    editing_id: Option<AssigneeId>,
    name_input: Entity<InputState>,
    handle_input: Entity<InputState>,
    role_input: Entity<InputState>,
    notes_input: Entity<TextareaState>,
    selected_color: String,
    handle_edited_manually: bool,
    suppress_handle_change: bool,
    error: Option<String>,
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl AssigneesManagerModalView {
    fn new(
        app_state: Entity<AppState>,
        store: Entity<ProjectStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g., Mark Benson"));
        let handle_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g., MB"));
        let role_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Optional role (e.g., Founder)"));
        let notes_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(2)
                .soft_wrap(true)
                .placeholder("Optional local notes…")
        });

        let store_sub = cx.observe(&store, |_, _, cx| cx.notify());

        let name_sub = cx.subscribe_in(
            &name_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.error = None;
                    if this.editing_id.is_none() && !this.handle_edited_manually {
                        let name_val = this.name_input.read(cx).value();
                        let suggested = suggest_assignee_handle(&name_val);
                        this.suppress_handle_change = true;
                        this.handle_input.update(cx, |inp, cx| {
                            inp.set_value(suggested, window, cx);
                        });
                        this.suppress_handle_change = false;
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.submit(window, cx);
                }
                _ => {}
            },
        );

        let handle_sub = cx.subscribe_in(
            &handle_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    if !this.suppress_handle_change {
                        this.handle_edited_manually = true;
                        this.error = None;
                        cx.notify();
                    }
                }
                InputEvent::PressEnter { .. } => {
                    this.submit(window, cx);
                }
                _ => {}
            },
        );

        let role_sub = cx.subscribe_in(
            &role_input,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit(window, cx);
                }
            },
        );

        Self {
            app_state,
            store,
            editing_id: None,
            name_input,
            handle_input,
            role_input,
            notes_input,
            selected_color: "#64748B".to_string(),
            handle_edited_manually: false,
            suppress_handle_change: false,
            error: None,
            busy: false,
            _subscriptions: vec![store_sub, name_sub, handle_sub, role_sub],
        }
    }

    fn reset_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_id = None;
        self.handle_edited_manually = false;
        self.selected_color = "#64748B".to_string();
        self.error = None;
        self.suppress_handle_change = true;
        self.name_input.update(cx, |inp, cx| {
            inp.set_value("", window, cx);
            inp.focus(window, cx);
        });
        self.handle_input
            .update(cx, |inp, cx| inp.set_value("", window, cx));
        self.role_input
            .update(cx, |inp, cx| inp.set_value("", window, cx));
        self.notes_input
            .update(cx, |inp, cx| inp.set_value("", window, cx));
        self.suppress_handle_change = false;
        cx.notify();
    }

    fn start_editing(&mut self, id: AssigneeId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(assignee) = self
            .store
            .read(cx)
            .assignees()
            .iter()
            .find(|a| a.id == id)
            .cloned()
        else {
            return;
        };

        self.editing_id = Some(id);
        self.handle_edited_manually = true;
        self.selected_color = assignee.color;
        self.error = None;
        self.suppress_handle_change = true;
        self.name_input.update(cx, |inp, cx| {
            inp.set_value(assignee.name, window, cx);
            inp.focus(window, cx);
        });
        self.handle_input
            .update(cx, |inp, cx| inp.set_value(assignee.handle, window, cx));
        self.role_input.update(cx, |inp, cx| {
            inp.set_value(assignee.role.unwrap_or_default(), window, cx)
        });
        self.notes_input
            .update(cx, |inp, cx| inp.set_value(assignee.notes, window, cx));
        self.suppress_handle_change = false;
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }

        let name = match validate_assignee_name(&self.name_input.read(cx).value()) {
            Ok(v) => v,
            Err(err) => {
                self.error = Some(err.to_string());
                cx.notify();
                return;
            }
        };
        let handle = match validate_assignee_handle(&self.handle_input.read(cx).value()) {
            Ok(v) => v,
            Err(err) => {
                self.error = Some(err.to_string());
                cx.notify();
                return;
            }
        };
        let color = match validate_hex_color(&self.selected_color) {
            Ok(v) => v,
            Err(err) => {
                self.error = Some(err.to_string());
                cx.notify();
                return;
            }
        };

        let role_raw = self.role_input.read(cx).value();
        let role = (!role_raw.trim().is_empty()).then(|| role_raw.trim().to_string());
        let notes = self.notes_input.read(cx).value().to_string();

        let command: Box<dyn UndoableCommand> = match self.editing_id {
            Some(id) => Box::new(UpsertAssignee::update(id, name, handle, color, role, notes)),
            None => Box::new(UpsertAssignee::create(name, handle, color, role, notes)),
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
                let outcome = task.await;
                let _ = cx.update(|window, cx| match outcome {
                    Ok(effect) => {
                        let _ = view.update(cx, |this, cx| {
                            this.busy = false;
                            this.reset_form(window, cx);
                        });
                        if let Some(msg) = effect.toast {
                            window.push_notification(Notification::success(msg), cx);
                        }
                    }
                    Err(err) => {
                        let _ = view.update(cx, |this, cx| {
                            this.busy = false;
                            this.error = Some(err.to_string());
                            cx.notify();
                        });
                    }
                });
            })
            .detach();
    }

    fn toggle_archive(
        &mut self,
        assignee_id: AssigneeId,
        archived: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();

        let command = Box::new(ArchiveAssignee::new(assignee_id, archived));
        let task = self
            .app_state
            .update(cx, |app, cx| app.execute_command(command, cx));
        let view = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let outcome = task.await;
                let _ = cx.update(|window, cx| match outcome {
                    Ok(effect) => {
                        let _ = view.update(cx, |this, cx| {
                            this.busy = false;
                            cx.notify();
                        });
                        if let Some(msg) = effect.toast {
                            window.push_notification(Notification::success(msg), cx);
                        }
                    }
                    Err(err) => {
                        let _ = view.update(cx, |this, cx| {
                            this.busy = false;
                            this.error = Some(err.to_string());
                            cx.notify();
                        });
                    }
                });
            })
            .detach();
    }
}

impl Render for AssigneesManagerModalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let assignees = self.store.read(cx).assignees().to_vec();
        let name_ok = validate_assignee_name(&self.name_input.read(cx).value()).is_ok();
        let handle_raw = self.handle_input.read(cx).value();
        let handle_err = if handle_raw.trim().is_empty() {
            None
        } else {
            validate_assignee_handle(&handle_raw).err()
        };
        let handle_ok = validate_assignee_handle(&handle_raw).is_ok();
        let can_submit = !self.busy && name_ok && handle_ok;
        let is_editing = self.editing_id.is_some();

        v_flex()
            .w_full()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Local assignees are shared globally across all Projects in this workspace. Archived assignees remain visible on historical items."),
            )
            .when_some(render_error_banner(self.error.clone(), cx), |this, banner| {
                this.child(banner)
            })
            // Existing Assignees list
            .child(if assignees.is_empty() {
                div()
                    .px_3()
                    .py_4()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("No assignees in this workspace yet. Add one below.")
                    .into_any_element()
            } else {
                v_flex()
                    .id("assignee-list-viewport")
                    .test_support()
                    .w_full()
                    .max_h(px(210.))
                    .overflow_hidden()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        div().flex_1().overflow_hidden().child(
                            v_flex()
                                .size_full()
                                .gap_1p5()
                                .p_2()
                                .overflow_y_scrollbar()
                                .id("assignee-list-scroll")
                                .children(assignees.into_iter().enumerate().map(|(ix, a)| {
                                    let aid = a.id;
                                    let is_archived = a.is_archived();
                                    let is_being_edited = self.editing_id == Some(aid);
                                    let notes_preview = a.notes.trim().to_string();

                                    h_flex()
                                        .id(("assignee-row", ix))
                                        .test_support()
                                        .flex_shrink_0()
                                        .w_full()
                                        .px_2p5()
                                        .py_1p5()
                                        .gap_2()
                                        .items_center()
                                        .justify_between()
                                        .rounded(cx.theme().radius * 0.75)
                                        .bg(if is_being_edited {
                                            cx.theme().accent
                                        } else {
                                            cx.theme().muted.opacity(0.35)
                                        })
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w_0()
                                                .gap_0p5()
                                                .child(assignee_pill(&a, true, cx))
                                                .when(!notes_preview.is_empty(), |this| {
                                                    this.child(
                                                        div()
                                                            .text_size(px(11.))
                                                            .text_color(cx.theme().muted_foreground)
                                                            .overflow_hidden()
                                                            .whitespace_nowrap()
                                                            .text_ellipsis()
                                                            .child(notes_preview),
                                                    )
                                                }),
                                        )
                                        .child(
                                            h_flex()
                                                .gap_1()
                                                .items_center()
                                                .child(
                                                    Button::new(("assignee-row-edit", ix))
                                                        .ghost()
                                                        .xsmall()
                                                        .label("Edit")
                                                        .disabled(self.busy)
                                                        .on_click(cx.listener(
                                                            move |this, _, window, cx| {
                                                                this.start_editing(aid, window, cx);
                                                            },
                                                        )),
                                                )
                                                .child(
                                                    Button::new(("assignee-row-archive", ix))
                                                        .outline()
                                                        .xsmall()
                                                        .label(if is_archived {
                                                            "Restore"
                                                        } else {
                                                            "Archive"
                                                        })
                                                        .disabled(self.busy)
                                                        .on_click(cx.listener(
                                                            move |this, _, window, cx| {
                                                                this.toggle_archive(
                                                                    aid,
                                                                    !is_archived,
                                                                    window,
                                                                    cx,
                                                                );
                                                            },
                                                        )),
                                                ),
                                        )
                                })),
                        ),
                    )
                    .into_any_element()
            })
            // Create / Edit form
            .child(
                v_flex()
                    .p_3()
                    .gap_2p5()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted.opacity(0.35))
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .child(if is_editing {
                                        "Edit Assignee"
                                    } else {
                                        "New Assignee"
                                    }),
                            )
                            .when(is_editing, |this| {
                                this.child(
                                    Button::new("assignee-modal-cancel-edit")
                                        .ghost()
                                        .xsmall()
                                        .label("Cancel Edit")
                                        .disabled(self.busy)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.reset_form(window, cx);
                                        })),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_start()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap_1()
                                    .child(div().text_xs().font_medium().child("Name"))
                                    .child(
                                        Input::new(&self.name_input)
                                            .id("assignee-modal-name")
                                            .small()
                                            .disabled(self.busy),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .w(px(120.))
                                    .gap_1()
                                    .child(div().text_xs().font_medium().child("Handle"))
                                    .child(
                                        Input::new(&self.handle_input)
                                            .id("assignee-modal-handle")
                                            .small()
                                            .disabled(self.busy),
                                    )
                                    .when_some(handle_err, |this, err| {
                                        this.child(
                                            div()
                                                .text_size(px(10.))
                                                .text_color(cx.theme().danger)
                                                .child(err.to_string()),
                                        )
                                    }),
                            )
                            .child(
                                v_flex()
                                    .w(px(140.))
                                    .gap_1()
                                    .child(div().text_xs().font_medium().child("Role"))
                                    .child(
                                        Input::new(&self.role_input)
                                            .id("assignee-modal-role")
                                            .small()
                                            .disabled(self.busy),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_3()
                            .items_center()
                            .justify_between()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(div().text_xs().font_medium().child("Pill Color"))
                                    .child(
                                        h_flex().gap_1p5().children(
                                            PROJECT_COLOR_PRESETS.iter().enumerate().map(
                                                |(ix, (hex, label))| {
                                                    let is_selected = self
                                                        .selected_color
                                                        .eq_ignore_ascii_case(hex);
                                                    let swatch =
                                                        parse_hex_or(hex, cx.theme().primary);
                                                    let hex_owned = (*hex).to_string();

                                                    div()
                                                        .id(("assignee-color-swatch", ix))
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .size(px(22.))
                                                        .rounded(px(999.))
                                                        .cursor_pointer()
                                                        .border_2()
                                                        .border_color(if is_selected {
                                                            cx.theme().foreground
                                                        } else {
                                                            cx.theme().transparent
                                                        })
                                                        .tooltip(move |window, cx| {
                                                            Tooltip::new(*label).build(window, cx)
                                                        })
                                                        .on_click(cx.listener(move |this, _, _, cx| {
                                                            if !this.busy {
                                                                this.selected_color =
                                                                    hex_owned.clone();
                                                                cx.notify();
                                                            }
                                                        }))
                                                        .child(
                                                            div()
                                                                .size(px(14.))
                                                                .rounded(px(999.))
                                                                .bg(swatch),
                                                        )
                                                },
                                            ),
                                        ),
                                    ),
                            )
                            .child(
                                Button::new("assignee-modal-submit")
                                    .primary()
                                    .small()
                                    .label(if is_editing {
                                        "Save Assignee"
                                    } else {
                                        "Add Assignee"
                                    })
                                    .disabled(!can_submit)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit(window, cx);
                                    })),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().font_medium().child("Notes"))
                            .child(
                                Textarea::new(&self.notes_input)
                                    .small()
                                    .h(px(56.))
                                    .disabled(self.busy),
                            ),
                    ),
            )
            .child(
                DialogFooter::new().pt_1().child(
                    Button::new("assignee-modal-close")
                        .outline()
                        .small()
                        .label("Done")
                        .disabled(self.busy)
                        .on_click(|_, window, cx| {
                            window.close_dialog(cx);
                        }),
                ),
            )
    }
}

pub struct TagsManagerModalView {
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    project_id: ProjectId,
    project_name: String,
    key_prefix: String,
    editing_id: Option<TagId>,
    name_input: Entity<InputState>,
    selected_color: String,
    error: Option<String>,
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl TagsManagerModalView {
    fn new(
        app_state: Entity<AppState>,
        store: Entity<ProjectStore>,
        project_id: ProjectId,
        project_name: String,
        key_prefix: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g., bug, core, tech-debt"));

        let store_sub = cx.observe(&store, |_, _, cx| cx.notify());
        let name_sub = cx.subscribe_in(
            &name_input,
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

        Self {
            app_state,
            store,
            project_id,
            project_name,
            key_prefix,
            editing_id: None,
            name_input,
            selected_color: "#8B5CF6".to_string(),
            error: None,
            busy: false,
            _subscriptions: vec![store_sub, name_sub],
        }
    }

    fn reset_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_id = None;
        self.selected_color = "#8B5CF6".to_string();
        self.error = None;
        self.name_input.update(cx, |inp, cx| {
            inp.set_value("", window, cx);
            inp.focus(window, cx);
        });
        cx.notify();
    }

    fn start_editing(&mut self, tag_id: TagId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tag) = self
            .store
            .read(cx)
            .snapshot()
            .and_then(|s| s.tags.iter().find(|t| t.id == tag_id).cloned())
        else {
            return;
        };
        self.editing_id = Some(tag_id);
        self.selected_color = tag.color;
        self.error = None;
        self.name_input.update(cx, |inp, cx| {
            inp.set_value(tag.name, window, cx);
            inp.focus(window, cx);
        });
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let name = match validate_tag_name(&self.name_input.read(cx).value()) {
            Ok(v) => v,
            Err(err) => {
                self.error = Some(err.to_string());
                cx.notify();
                return;
            }
        };
        let color = match validate_hex_color(&self.selected_color) {
            Ok(v) => v,
            Err(err) => {
                self.error = Some(err.to_string());
                cx.notify();
                return;
            }
        };

        let command: Box<dyn UndoableCommand> = match self.editing_id {
            Some(id) => Box::new(UpsertTag::update(id, self.project_id, name, color)),
            None => Box::new(UpsertTag::create(self.project_id, name, color)),
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
                let outcome = task.await;
                let _ = cx.update(|window, cx| match outcome {
                    Ok(effect) => {
                        let _ = view.update(cx, |this, cx| {
                            this.busy = false;
                            this.reset_form(window, cx);
                        });
                        if let Some(msg) = effect.toast {
                            window.push_notification(Notification::success(msg), cx);
                        }
                    }
                    Err(err) => {
                        let _ = view.update(cx, |this, cx| {
                            this.busy = false;
                            this.error = Some(err.to_string());
                            cx.notify();
                        });
                    }
                });
            })
            .detach();
    }

    fn delete_tag(&mut self, tag_id: TagId, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();

        let command = Box::new(DeleteTag::new(tag_id));
        let task = self
            .app_state
            .update(cx, |app, cx| app.execute_command(command, cx));
        let view = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let outcome = task.await;
                let _ = cx.update(|window, cx| match outcome {
                    Ok(effect) => {
                        let _ = view.update(cx, |this, cx| {
                            this.busy = false;
                            if this.editing_id == Some(tag_id) {
                                this.reset_form(window, cx);
                            } else {
                                cx.notify();
                            }
                        });
                        if let Some(msg) = effect.toast {
                            window.push_notification(Notification::success(msg), cx);
                        }
                    }
                    Err(err) => {
                        let _ = view.update(cx, |this, cx| {
                            this.busy = false;
                            this.error = Some(err.to_string());
                            cx.notify();
                        });
                    }
                });
            })
            .detach();
    }
}

impl Render for TagsManagerModalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snap_opt = self.store.read(cx).snapshot().cloned();
        let tags: Vec<_> = snap_opt
            .as_ref()
            .filter(|s| s.project.id == self.project_id)
            .map(|s| s.tags.clone())
            .unwrap_or_default();

        let name_ok = validate_tag_name(&self.name_input.read(cx).value()).is_ok();
        let can_submit = !self.busy && name_ok;
        let is_editing = self.editing_id.is_some();

        v_flex()
            .w_full()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "Tags here are strictly scoped to {} ({}) and never leak into other Projects.",
                        self.project_name, self.key_prefix
                    )),
            )
            .when_some(render_error_banner(self.error.clone(), cx), |this, banner| {
                this.child(banner)
            })
            .child(if tags.is_empty() {
                div()
                    .px_3()
                    .py_4()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("No tags in this project yet. Add one below.")
                    .into_any_element()
            } else {
                v_flex()
                    .id("tag-list-viewport")
                    .test_support()
                    .w_full()
                    .max_h(px(210.))
                    .overflow_hidden()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        div().flex_1().overflow_hidden().child(
                            v_flex()
                                .size_full()
                                .gap_1p5()
                                .p_2()
                                .overflow_y_scrollbar()
                                .id("tag-list-scroll")
                                .children(tags.into_iter().enumerate().map(|(ix, tag)| {
                                    let tid = tag.id;
                                    let is_being_edited = self.editing_id == Some(tid);
                                    let usage_count = snap_opt.as_ref().map_or(0, |s| {
                                        s.item_tags
                                            .values()
                                            .filter(|ids| ids.contains(&tid))
                                            .count()
                                    });

                                    h_flex()
                                        .id(("tag-row", ix))
                                        .test_support()
                                        .flex_shrink_0()
                                        .w_full()
                                        .px_2p5()
                                        .py_1p5()
                                        .gap_2()
                                        .items_center()
                                        .justify_between()
                                        .rounded(cx.theme().radius * 0.75)
                                        .bg(if is_being_edited {
                                            cx.theme().accent
                                        } else {
                                            cx.theme().muted.opacity(0.35)
                                        })
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .items_center()
                                                .child(domain_tag_chip(&tag, cx))
                                                .child(
                                                    div()
                                                        .text_size(px(11.))
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(format!("{usage_count} item(s)")),
                                                ),
                                        )
                                        .child(
                                            h_flex()
                                                .gap_1()
                                                .items_center()
                                                .child(
                                                    Button::new(("tag-row-edit", ix))
                                                        .ghost()
                                                        .xsmall()
                                                        .label("Edit")
                                                        .disabled(self.busy)
                                                        .on_click(cx.listener(
                                                            move |this, _, window, cx| {
                                                                this.start_editing(tid, window, cx);
                                                            },
                                                        )),
                                                )
                                                .child(
                                                    Button::new(("tag-row-delete", ix))
                                                        .outline()
                                                        .xsmall()
                                                        .label("Delete")
                                                        .disabled(self.busy)
                                                        .on_click(cx.listener(
                                                            move |this, _, window, cx| {
                                                                this.delete_tag(tid, window, cx);
                                                            },
                                                        )),
                                                ),
                                        )
                                })),
                        ),
                    )
                    .into_any_element()
            })
            .child(
                v_flex()
                    .p_3()
                    .gap_2p5()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted.opacity(0.35))
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .child(if is_editing { "Edit Tag" } else { "New Tag" }),
                            )
                            .when(is_editing, |this| {
                                this.child(
                                    Button::new("tag-modal-cancel-edit")
                                        .ghost()
                                        .xsmall()
                                        .label("Cancel Edit")
                                        .disabled(self.busy)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.reset_form(window, cx);
                                        })),
                                )
                            }),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().font_medium().child("Tag Name"))
                            .child(
                                Input::new(&self.name_input)
                                    .id("tag-modal-name")
                                    .small()
                                    .disabled(self.busy),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_3()
                            .items_center()
                            .justify_between()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(div().text_xs().font_medium().child("Color"))
                                    .child(
                                        h_flex().gap_1p5().children(
                                            PROJECT_COLOR_PRESETS.iter().enumerate().map(
                                                |(ix, (hex, label))| {
                                                    let is_selected = self
                                                        .selected_color
                                                        .eq_ignore_ascii_case(hex);
                                                    let swatch =
                                                        parse_hex_or(hex, cx.theme().primary);
                                                    let hex_owned = (*hex).to_string();

                                                    div()
                                                        .id(("tag-color-swatch", ix))
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .size(px(22.))
                                                        .rounded(px(999.))
                                                        .cursor_pointer()
                                                        .border_2()
                                                        .border_color(if is_selected {
                                                            cx.theme().foreground
                                                        } else {
                                                            cx.theme().transparent
                                                        })
                                                        .tooltip(move |window, cx| {
                                                            Tooltip::new(*label).build(window, cx)
                                                        })
                                                        .on_click(cx.listener(move |this, _, _, cx| {
                                                            if !this.busy {
                                                                this.selected_color =
                                                                    hex_owned.clone();
                                                                cx.notify();
                                                            }
                                                        }))
                                                        .child(
                                                            div()
                                                                .size(px(14.))
                                                                .rounded(px(999.))
                                                                .bg(swatch),
                                                        )
                                                },
                                            ),
                                        ),
                                    ),
                            )
                            .child(
                                Button::new("tag-modal-submit")
                                    .primary()
                                    .small()
                                    .label(if is_editing { "Save Tag" } else { "Add Tag" })
                                    .disabled(!can_submit)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit(window, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                DialogFooter::new().pt_1().child(
                    Button::new("tag-modal-close")
                        .outline()
                        .small()
                        .label("Done")
                        .disabled(self.busy)
                        .on_click(|_, window, cx| {
                            window.close_dialog(cx);
                        }),
                ),
            )
    }
}

pub fn open_assignees_manager_modal(
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }

    let view = cx.new(|cx| AssigneesManagerModalView::new(app_state, store, window, cx));
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            let input = view.read(cx).name_input.clone();
            input.update(cx, |inp, cx| inp.focus(window, cx));
        }
        let view = view.clone();
        dialog
            .width(px(560.))
            .title("Global Assignees")
            .content(move |content, _, _| content.child(view.clone()))
    });
}

pub fn open_tags_manager_modal(
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    project_id: ProjectId,
    project_name: String,
    key_prefix: String,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }

    let title = format!("Project Tags · {key_prefix}");
    let view = cx.new(|cx| {
        TagsManagerModalView::new(
            app_state,
            store,
            project_id,
            project_name,
            key_prefix,
            window,
            cx,
        )
    });
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            let input = view.read(cx).name_input.clone();
            input.update(cx, |inp, cx| inp.focus(window, cx));
        }
        let view = view.clone();
        dialog
            .width(px(480.))
            .title(title.clone())
            .content(move |content, _, _| content.child(view.clone()))
    });
}
