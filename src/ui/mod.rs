pub mod inspector;
pub mod shell;
pub mod sidebar;

pub use shell::WorkspaceShell;

use gpui_kit::component::{
    ActiveTheme as _, Colorize as _, Icon, IconName, Sizable as _, StyledExt as _, tag::Tag,
};
use gpui_kit::{
    App, Hsla, IntoElement, KeyBinding, ParentElement as _, SharedString, Styled as _, div, px,
};

use crate::domain::{Assignee, ItemKind, ItemStatus, Priority, Tag as DomainTag};

gpui_kit::actions!(protrack, [ToggleSidebar, ToggleInspector, Quit]);

pub fn register_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-b", ToggleSidebar, None),
        KeyBinding::new("secondary-i", ToggleInspector, None),
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
    let bg = base_color.opacity(if cx.theme().is_dark() { 0.22 } else { 0.14 });
    let border = base_color.opacity(if cx.theme().is_dark() { 0.42 } else { 0.28 });

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
                .text_color(base_color)
                .child(assignee.handle.clone()),
        );

    if show_full_name {
        row = row.child(
            div()
                .text_xs()
                .text_color(cx.theme().foreground)
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
