pub mod dnd;
pub mod inspector;
pub mod modals;
pub mod outline_view;
pub mod shell;
pub mod sidebar;
pub mod table_view;
pub mod trash_view;
pub mod unallocated_view;

pub use shell::WorkspaceShell;

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::select::SelectItem;
use gpui_kit::component::{
    ActiveTheme as _, Colorize as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex,
    tag::Tag,
};
use gpui_kit::{
    AnyElement, App, Context, Entity, Hsla, IntoElement, KeyBinding, ParentElement as _,
    SharedString, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use crate::domain::{
    Assignee, BlockerWarning, EffectiveBlockState, ItemKind, ItemStatus, Priority, Project,
    ProjectStatus, Tag as DomainTag,
};
use crate::state::AppState;
use crate::ui::modals::{open_new_project_modal, open_project_settings_modal};

gpui_kit::actions!(
    protrack,
    [
        ToggleSidebar,
        ToggleInspector,
        OpenWorkspace,
        OpenQuickSearch,
        OpenQuickAdd,
        Quit
    ]
);

pub fn register_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-b", ToggleSidebar, None),
        KeyBinding::new("secondary-i", ToggleInspector, None),
        KeyBinding::new("secondary-o", OpenWorkspace, None),
        KeyBinding::new("secondary-k", OpenQuickSearch, None),
        KeyBinding::new("secondary-n", OpenQuickAdd, None),
        KeyBinding::new("secondary-q", Quit, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
}

pub fn parse_hex_or(hex: &str, fallback: Hsla) -> Hsla {
    Hsla::parse_hex(hex).unwrap_or(fallback)
}

pub fn status_color(status: ItemStatus, cx: &App) -> Hsla {
    match status {
        ItemStatus::Backlog => cx.theme().muted_foreground,
        ItemStatus::Todo => cx.theme().foreground.opacity(0.72),
        ItemStatus::InProgress => cx.theme().info,
        ItemStatus::Done => cx.theme().success,
        ItemStatus::Cancelled => cx.theme().muted_foreground.opacity(0.65),
    }
}

pub const fn status_icon(status: ItemStatus) -> IconName {
    match status {
        ItemStatus::Backlog => IconName::Dash,
        ItemStatus::Todo => IconName::Square,
        ItemStatus::InProgress => IconName::LoaderCircle,
        ItemStatus::Done => IconName::CircleCheck,
        ItemStatus::Cancelled => IconName::CircleX,
    }
}

pub fn priority_color(priority: Priority, cx: &App) -> Hsla {
    match priority {
        Priority::None => cx.theme().muted_foreground.opacity(0.6),
        Priority::Low => cx.theme().muted_foreground,
        Priority::Medium => cx.theme().warning,
        Priority::High => cx.theme().warning.mix_oklab(cx.theme().danger, 0.55),
        Priority::Urgent => cx.theme().danger,
    }
}

pub fn status_badge(status: ItemStatus, cx: &App) -> impl IntoElement {
    let color = status_color(status, cx);
    let bg = color.opacity(if cx.theme().is_dark() { 0.18 } else { 0.11 });
    let border = color.opacity(if cx.theme().is_dark() { 0.35 } else { 0.25 });

    Tag::custom(bg, color, border)
        .xsmall()
        .gap_1()
        .child(Icon::new(status_icon(status)).xsmall().text_color(color))
        .child(status.label())
}

pub fn priority_badge(priority: Priority, cx: &App) -> impl IntoElement {
    let color = priority_color(priority, cx);
    let bg = color.opacity(if cx.theme().is_dark() { 0.16 } else { 0.10 });
    let border = color.opacity(if cx.theme().is_dark() { 0.32 } else { 0.22 });

    let tag = Tag::custom(bg, color, border).xsmall().gap_1();
    if priority == Priority::Urgent {
        tag.child(Icon::new(IconName::CircleAlert).xsmall().text_color(color))
            .child(priority.label())
    } else if priority == Priority::None {
        tag.child("—")
    } else {
        tag.child(priority.label())
    }
}

pub fn kind_badge(kind: ItemKind, cx: &App) -> impl IntoElement {
    let accent = match kind {
        ItemKind::Feature => cx.theme().primary,
        ItemKind::WorkItem => cx.theme().info,
    };
    let bg = accent.opacity(if cx.theme().is_dark() { 0.2 } else { 0.12 });
    let border = accent.opacity(if cx.theme().is_dark() { 0.38 } else { 0.26 });
    let icon = match kind {
        ItemKind::Feature => IconName::Folder,
        ItemKind::WorkItem => IconName::CircleCheck,
    };

    Tag::custom(bg, accent, border)
        .xsmall()
        .gap_1()
        .child(Icon::new(icon).xsmall().text_color(accent))
        .child(kind.label())
}

pub fn assignee_pill(assignee: &Assignee, show_full_name: bool, cx: &App) -> impl IntoElement {
    let base_color = parse_hex_or(&assignee.color, cx.theme().primary);
    let archived = assignee.is_archived();
    let fg_color = if archived {
        base_color.opacity(0.65)
    } else {
        base_color
    };
    let bg = base_color.opacity(if archived {
        0.09
    } else if cx.theme().is_dark() {
        0.22
    } else {
        0.14
    });
    let border = base_color.opacity(if archived {
        0.22
    } else if cx.theme().is_dark() {
        0.42
    } else {
        0.28
    });

    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1p5()
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .px_1p5()
                .h(px(18.))
                .rounded(px(999.))
                .bg(bg)
                .border_1()
                .border_color(border)
                .text_size(px(10.))
                .font_semibold()
                .text_color(fg_color)
                .child(assignee.handle.clone()),
        );

    if show_full_name {
        row = row.child(
            div()
                .text_xs()
                .text_color(if archived {
                    cx.theme().muted_foreground
                } else {
                    cx.theme().foreground
                })
                .child(assignee.name.clone()),
        );
        if let Some(role) = &assignee.role {
            row = row.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("· {role}")),
            );
        }
        if archived {
            row = row.child(
                div()
                    .text_size(px(10.))
                    .text_color(cx.theme().muted_foreground)
                    .child("(Archived)"),
            );
        }
    }

    row
}

pub fn domain_tag_chip(tag: &DomainTag, cx: &App) -> impl IntoElement {
    let color = parse_hex_or(&tag.color, cx.theme().primary);
    let bg = color.opacity(if cx.theme().is_dark() { 0.2 } else { 0.11 });
    let border = color.opacity(if cx.theme().is_dark() { 0.38 } else { 0.25 });

    Tag::custom(bg, color, border)
        .xsmall()
        .child(SharedString::from(tag.name.clone()))
}

pub fn block_state_badge(
    state: EffectiveBlockState,
    primary_blocker_key: Option<&str>,
    cx: &App,
) -> Option<AnyElement> {
    let label = state.badge_label(primary_blocker_key)?;
    let (color, icon, is_dashed) = match state {
        EffectiveBlockState::Unblocked => return None,
        EffectiveBlockState::DirectlyBlocked { .. } => (cx.theme().danger, IconName::Ban, false),
        EffectiveBlockState::InheritedBlocked { .. } => (
            cx.theme().warning.mix_oklab(cx.theme().danger, 0.35),
            IconName::TriangleAlert,
            false,
        ),
        EffectiveBlockState::PartiallyBlocked { .. } => {
            (cx.theme().warning, IconName::CircleAlert, true)
        }
    };
    let bg = color.opacity(if cx.theme().is_dark() { 0.20 } else { 0.12 });
    let border = color.opacity(if cx.theme().is_dark() { 0.42 } else { 0.30 });

    let mut tag = Tag::custom(bg, color, border)
        .xsmall()
        .gap_1()
        .child(Icon::new(icon).xsmall().text_color(color))
        .child(label);
    if is_dashed {
        tag = tag.border_dashed();
    }
    Some(tag.into_any_element())
}

pub fn blocker_warning_pill(warning: &BlockerWarning, compact: bool, cx: &App) -> AnyElement {
    let (color, icon) = match warning {
        BlockerWarning::Cancelled { .. } => (cx.theme().warning, IconName::TriangleAlert),
        BlockerWarning::InTrash { .. } => (cx.theme().muted_foreground, IconName::Delete),
    };
    let bg = color.opacity(if cx.theme().is_dark() { 0.18 } else { 0.11 });
    let border = color.opacity(if cx.theme().is_dark() { 0.35 } else { 0.24 });
    let label = if compact {
        warning.short_label().to_string()
    } else {
        warning.message()
    };

    Tag::custom(bg, color, border)
        .xsmall()
        .gap_1()
        .child(Icon::new(icon).xsmall().text_color(color))
        .child(label)
        .into_any_element()
}

pub fn progress_pill(done: usize, total: usize, cx: &App) -> impl IntoElement {
    let text = if total == 0 {
        "—".to_string()
    } else {
        format!("{done}/{total}")
    };
    let complete = total > 0 && done == total;
    let color = if complete {
        cx.theme().success
    } else {
        cx.theme().muted_foreground
    };

    div()
        .px_1p5()
        .py_0p5()
        .rounded(px(4.))
        .bg(cx.theme().muted)
        .text_size(px(10.))
        .font_family(cx.theme().mono_font_family.clone())
        .text_color(color)
        .child(text)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusSelectOption(pub ItemStatus);

impl SelectItem for StatusSelectOption {
    type Value = ItemStatus;

    fn title(&self) -> SharedString {
        SharedString::from(self.0.label())
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = status_color(self.0, cx);
        h_flex()
            .gap_1p5()
            .items_center()
            .child(Icon::new(status_icon(self.0)).xsmall().text_color(color))
            .child(div().text_xs().child(self.0.label()))
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}

pub fn all_status_options() -> Vec<StatusSelectOption> {
    vec![
        StatusSelectOption(ItemStatus::Backlog),
        StatusSelectOption(ItemStatus::Todo),
        StatusSelectOption(ItemStatus::InProgress),
        StatusSelectOption(ItemStatus::Done),
        StatusSelectOption(ItemStatus::Cancelled),
    ]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrioritySelectOption(pub Priority);

impl SelectItem for PrioritySelectOption {
    type Value = Priority;

    fn title(&self) -> SharedString {
        SharedString::from(self.0.label())
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = priority_color(self.0, cx);
        h_flex()
            .gap_1p5()
            .items_center()
            .child(div().size(px(7.)).rounded(px(999.)).bg(color))
            .child(div().text_xs().child(self.0.label()))
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}

pub fn all_priority_options() -> Vec<PrioritySelectOption> {
    vec![
        PrioritySelectOption(Priority::None),
        PrioritySelectOption(Priority::Low),
        PrioritySelectOption(Priority::Medium),
        PrioritySelectOption(Priority::High),
        PrioritySelectOption(Priority::Urgent),
    ]
}

pub fn build_project_switcher_menu(
    mut menu: PopupMenu,
    projects: Vec<Project>,
    active_project: Option<Project>,
    active_item_count: usize,
    app_state: Entity<AppState>,
    _cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let active_id = active_project.as_ref().map(|p| p.id);
    menu = menu.min_w(px(240.)).label("PROJECTS");

    if projects.is_empty() {
        menu = menu.item(PopupMenuItem::label("No projects in workspace"));
    } else {
        for proj in projects {
            let pid = proj.id;
            let is_active = active_id == Some(pid);
            let prefix = proj.key_prefix.clone();
            let name = proj.name.clone();
            let color_hex = proj.color.clone();
            let status = proj.status;
            let app_switch = app_state.clone();

            menu = menu.item(
                PopupMenuItem::element(move |_, cx| {
                    let dot_color = parse_hex_or(&color_hex, cx.theme().primary);
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(div().size(px(8.)).flex_shrink_0().rounded(px(2.)).bg(dot_color))
                        .child(
                            div()
                                .px_1()
                                .rounded(px(3.))
                                .bg(cx.theme().muted)
                                .text_size(px(10.))
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_color(cx.theme().muted_foreground)
                                .child(prefix.clone()),
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
                                .child(name.clone()),
                        )
                        .when(status != ProjectStatus::Active, |this| {
                            this.child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(status.label()),
                            )
                        })
                })
                .checked(is_active)
                .on_click(move |_, _, cx| {
                    app_switch
                        .update(cx, |app, cx| app.switch_project(pid, cx))
                        .detach();
                }),
            );
        }
    }

    let app_new = app_state.clone();
    let app_settings = app_state;
    let active_for_settings = active_project;

    menu.separator()
        .item(
            PopupMenuItem::new("New Project…")
                .icon(IconName::Plus)
                .on_click(move |_, window, cx| {
                    open_new_project_modal(app_new.clone(), window, cx);
                }),
        )
        .item(
            PopupMenuItem::new("Project Settings…")
                .icon(IconName::Settings)
                .disabled(active_for_settings.is_none())
                .on_click(move |_, window, cx| {
                    if let Some(proj) = active_for_settings.clone() {
                        open_project_settings_modal(
                            app_settings.clone(),
                            proj,
                            active_item_count,
                            window,
                            cx,
                        );
                    }
                }),
        )
}
