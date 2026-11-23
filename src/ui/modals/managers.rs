use std::cell::Cell;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
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
    Subscription, Window, div, prelude::FluentBuilder as _, px,
};

use crate::commands::{CreateFeature, CreateProject, UndoableCommand, UpdateProject};
use crate::db::project_repo::{validate_hex_color, validate_key_prefix, validate_project_name};
use crate::domain::{ItemId, ItemStatus, Priority, Project, ProjectId, ProjectStatus};
use crate::state::AppState;
use crate::ui::{
    PrioritySelectOption, StatusSelectOption, all_priority_options, all_status_options,
    parse_hex_or,
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
            ProjectModalMode::Edit { original, .. } => Box::new(UpdateProject {
                project_id: original.id,
                name,
                key_prefix,
                description,
                color,
                status,
            }),
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
