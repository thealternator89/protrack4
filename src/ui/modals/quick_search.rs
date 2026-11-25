use std::cell::Cell;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, Styled as _, Subscription, TestSupportExt as _,
    Window, div, prelude::FluentBuilder as _, px,
};

use crate::db::SearchResultItem;
use crate::domain::{ItemKind, Priority, ProjectId};
use crate::state::{AppState, CenterViewTab, ProjectStore};
use crate::ui::modals::{open_new_project_modal, open_workspace_modal};
use crate::ui::{
    kind_badge, parse_hex_or, priority_badge, status_badge, status_color, status_icon,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickSearchScope {
    ActiveProject,
    AllProjects,
}

#[derive(Debug, Clone)]
enum QuickActionKind {
    GoToUnallocated,
    NewProject,
    OpenWorkspace,
    ToggleTheme,
    SwitchProject(ProjectId),
}

#[derive(Clone)]
struct QuickActionEntry {
    id_suffix: String,
    title: String,
    subtitle: String,
    keywords: String,
    icon: IconName,
    kind: QuickActionKind,
    disabled: bool,
}

#[derive(Clone)]
enum PaletteEntry {
    Item(SearchResultItem),
    Action(QuickActionEntry),
}

pub struct QuickSearchModalView {
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    query_input: Entity<InputState>,
    scope: QuickSearchScope,
    results: Vec<SearchResultItem>,
    selected_index: usize,
    search_generation: u64,
    is_searching: bool,
    _subscriptions: Vec<Subscription>,
}

impl QuickSearchModalView {
    fn new(
        app_state: Entity<AppState>,
        store: Entity<ProjectStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let has_active_project = store.read(cx).snapshot().is_some();
        let scope = if has_active_project {
            QuickSearchScope::ActiveProject
        } else {
            QuickSearchScope::AllProjects
        };

        let query_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search items by key (PRO-12), title, description, link, or command…")
        });

        let input_sub = cx.subscribe_in(
            &query_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.selected_index = 0;
                    this.trigger_search(cx);
                }
                InputEvent::PressEnter { .. } => {
                    this.activate_selected(window, cx);
                }
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );

        let store_sub = cx.observe(&store, |_, _, cx| {
            cx.notify();
        });

        Self {
            app_state,
            store,
            query_input,
            scope,
            results: Vec::new(),
            selected_index: 0,
            search_generation: 0,
            is_searching: false,
            _subscriptions: vec![input_sub, store_sub],
        }
    }

    fn set_scope(&mut self, scope: QuickSearchScope, cx: &mut Context<Self>) {
        if scope == QuickSearchScope::ActiveProject && self.store.read(cx).snapshot().is_none() {
            return;
        }
        if self.scope != scope {
            self.scope = scope;
            self.selected_index = 0;
            self.trigger_search(cx);
        }
    }

    fn trigger_search(&mut self, cx: &mut Context<Self>) {
        let query = self.query_input.read(cx).value().trim().to_string();
        self.search_generation += 1;
        let search_gen = self.search_generation;

        if query.is_empty() {
            self.results.clear();
            self.is_searching = false;
            cx.notify();
            return;
        }

        let scope_project_id = match self.scope {
            QuickSearchScope::ActiveProject => {
                self.store.read(cx).snapshot().map(|s| s.project.id)
            }
            QuickSearchScope::AllProjects => None,
        };

        self.is_searching = true;
        cx.notify();

        let task = self.app_state.update(cx, |app, cx| {
            app.search_items_fts(query, scope_project_id, 50, cx)
        });

        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |view, cx| {
                if view.search_generation != search_gen {
                    return;
                }
                view.is_searching = false;
                match res {
                    Ok(items) => {
                        view.results = items;
                        let total = view.build_entries(cx).len();
                        if total == 0 {
                            view.selected_index = 0;
                        } else if view.selected_index >= total {
                            view.selected_index = total - 1;
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "Quick search FTS5 query failed");
                        view.results.clear();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn quick_actions(&self, cx: &App) -> Vec<QuickActionEntry> {
        let state = self.store.read(cx);
        let has_workspace = state.has_workspace();
        let active_snapshot = state.snapshot();
        let active_project_id = active_snapshot.map(|s| s.project.id);
        let unallocated_count = active_snapshot.map_or(0, |s| s.unallocated_work_items().len());
        let next_theme = state.theme_preference().cycle();

        let mut actions = vec![
            QuickActionEntry {
                id_suffix: "unallocated".to_string(),
                title: "Go to Unallocated".to_string(),
                subtitle: format!("{unallocated_count} item(s) waiting for triage"),
                keywords: "unallocated inbox triage bucket".to_string(),
                icon: IconName::Inbox,
                kind: QuickActionKind::GoToUnallocated,
                disabled: active_snapshot.is_none(),
            },
            QuickActionEntry {
                id_suffix: "new-project".to_string(),
                title: "New Project…".to_string(),
                subtitle: "Create a new isolated Project in this workspace".to_string(),
                keywords: "create new project".to_string(),
                icon: IconName::Plus,
                kind: QuickActionKind::NewProject,
                disabled: !has_workspace,
            },
            QuickActionEntry {
                id_suffix: "open-workspace".to_string(),
                title: "Open Workspace…".to_string(),
                subtitle: "Switch, open, or create a .protrack workspace".to_string(),
                keywords: "open switch create workspace protrack".to_string(),
                icon: IconName::HardDrive,
                kind: QuickActionKind::OpenWorkspace,
                disabled: false,
            },
            QuickActionEntry {
                id_suffix: "toggle-theme".to_string(),
                title: "Toggle Theme".to_string(),
                subtitle: format!(
                    "Currently {} · switch to {}",
                    state.theme_preference().label(),
                    next_theme.label()
                ),
                keywords: "theme dark light system appearance".to_string(),
                icon: IconName::Palette,
                kind: QuickActionKind::ToggleTheme,
                disabled: false,
            },
        ];

        for (ix, project) in state.projects().iter().enumerate() {
            if Some(project.id) == active_project_id {
                continue;
            }
            actions.push(QuickActionEntry {
                id_suffix: format!("switch-project-{ix}"),
                title: format!("Switch Project: {} · {}", project.key_prefix, project.name),
                subtitle: format!("Status: {}", project.status.label()),
                keywords: format!(
                    "switch project {} {} {}",
                    project.key_prefix,
                    project.name,
                    project.status.label()
                ),
                icon: IconName::FolderOpen,
                kind: QuickActionKind::SwitchProject(project.id),
                disabled: false,
            });
        }

        actions
    }

    fn recent_items(&self, cx: &App) -> Vec<SearchResultItem> {
        let Some(snap) = self.store.read(cx).snapshot() else {
            return Vec::new();
        };
        let mut active = snap.active_items();
        active.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.item_number.cmp(&b.item_number))
        });
        active
            .into_iter()
            .take(8)
            .map(|item| SearchResultItem {
                id: item.id,
                project_id: snap.project.id,
                project_name: snap.project.name.clone(),
                project_key_prefix: snap.project.key_prefix.clone(),
                project_color: snap.project.color.clone(),
                item_key: item.item_key.clone(),
                kind: item.kind,
                title: item.title.clone(),
                status: item.status,
                priority: item.priority,
                link: item.link.clone(),
                rank: 0.0,
            })
            .collect()
    }

    fn build_entries(&self, cx: &App) -> Vec<PaletteEntry> {
        let query = self.query_input.read(cx).value().trim().to_lowercase();
        let actions = self.quick_actions(cx);

        if query.is_empty() {
            let mut out: Vec<PaletteEntry> = actions
                .into_iter()
                .filter(|a| !a.disabled)
                .map(PaletteEntry::Action)
                .collect();
            out.extend(self.recent_items(cx).into_iter().map(PaletteEntry::Item));
            return out;
        }

        let mut out: Vec<PaletteEntry> = self
            .results
            .iter()
            .cloned()
            .map(PaletteEntry::Item)
            .collect();

        for action in actions {
            if action.disabled {
                continue;
            }
            let hay = format!(
                "{} {} {}",
                action.title.to_lowercase(),
                action.subtitle.to_lowercase(),
                action.keywords.to_lowercase()
            );
            if query.split_whitespace().all(|tok| hay.contains(tok)) {
                out.push(PaletteEntry::Action(action));
            }
        }

        out
    }

    fn activate_entry(&mut self, entry: &PaletteEntry, window: &mut Window, cx: &mut Context<Self>) {
        match entry {
            PaletteEntry::Item(item) => {
                let project_id = item.project_id;
                let item_id = item.id;
                window.close_dialog(cx);
                self.app_state
                    .update(cx, |app, cx| app.jump_to_item(project_id, item_id, cx))
                    .detach();
            }
            PaletteEntry::Action(action) => {
                if action.disabled {
                    return;
                }
                match action.kind {
                    QuickActionKind::GoToUnallocated => {
                        self.store.update(cx, |s, cx| {
                            s.set_active_tab(CenterViewTab::Unallocated);
                            cx.notify();
                        });
                        window.close_dialog(cx);
                    }
                    QuickActionKind::NewProject => {
                        let app_state = self.app_state.clone();
                        window.close_dialog(cx);
                        window.defer(cx, move |window, cx| {
                            open_new_project_modal(app_state, window, cx);
                        });
                    }
                    QuickActionKind::OpenWorkspace => {
                        let app_state = self.app_state.clone();
                        window.close_dialog(cx);
                        window.defer(cx, move |window, cx| {
                            open_workspace_modal(app_state, window, cx);
                        });
                    }
                    QuickActionKind::ToggleTheme => {
                        let next = self.store.read(cx).theme_preference().cycle();
                        self.app_state.update(cx, |app, cx| {
                            app.set_theme_preference(next, cx);
                        });
                        next.apply(Some(window), cx);
                        window.close_dialog(cx);
                    }
                    QuickActionKind::SwitchProject(pid) => {
                        window.close_dialog(cx);
                        self.app_state
                            .update(cx, |app, cx| app.switch_project(pid, cx))
                            .detach();
                    }
                }
            }
        }
    }

    fn activate_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.build_entries(cx);
        if let Some(entry) = entries.get(self.selected_index).cloned() {
            self.activate_entry(&entry, window, cx);
        }
    }

    fn render_item_row(
        &self,
        item: &SearchResultItem,
        entry_idx: usize,
        is_selected: bool,
        show_project_pill: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entry = PaletteEntry::Item(item.clone());
        let project_color = parse_hex_or(&item.project_color, cx.theme().primary);
        let kind_icon = match item.kind {
            ItemKind::Feature => IconName::Folder,
            ItemKind::WorkItem => status_icon(item.status),
        };
        let icon_color = match item.kind {
            ItemKind::Feature => cx.theme().primary,
            ItemKind::WorkItem => status_color(item.status, cx),
        };

        ListItem::new(("quick-search-item-li", entry_idx))
            .w_full()
            .px_2p5()
            .py_1p5()
            .rounded(cx.theme().radius)
            .selected(is_selected)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.activate_entry(&entry, window, cx);
            }))
            .child(
                h_flex()
                    .id(("quick-search-item", entry_idx))
                    .test_support()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(kind_icon).small().text_color(icon_color))
                    .when(show_project_pill, |this| {
                        this.child(
                            h_flex()
                                .px_1p5()
                                .py_0p5()
                                .gap_1()
                                .items_center()
                                .rounded(px(4.))
                                .bg(project_color.opacity(0.14))
                                .child(div().size(px(6.)).rounded(px(2.)).bg(project_color))
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .font_semibold()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_color(project_color)
                                        .child(item.project_key_prefix.clone()),
                                ),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(cx.theme().muted_foreground)
                            .child(item.item_key.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_sm()
                            .font_medium()
                            .child(item.title.clone()),
                    )
                    .when_some(item.link.clone(), |this, link| {
                        this.child(
                            Tag::secondary()
                                .xsmall()
                                .gap_1()
                                .child(Icon::new(IconName::ExternalLink).xsmall())
                                .child(link.label),
                        )
                    })
                    .child(kind_badge(item.kind, cx))
                    .when(item.priority != Priority::None, |this| {
                        this.child(priority_badge(item.priority, cx))
                    })
                    .child(status_badge(item.status, cx)),
            )
            .into_any_element()
    }

    fn render_action_row(
        &self,
        action: &QuickActionEntry,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entry = PaletteEntry::Action(action.clone());
        let id_str = format!("quick-search-cmd-{}", action.id_suffix);

        let li_id = format!("quick-search-cmd-li-{}", action.id_suffix);

        ListItem::new(li_id)
            .w_full()
            .px_2p5()
            .py_1p5()
            .rounded(cx.theme().radius)
            .selected(is_selected)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.activate_entry(&entry, window, cx);
            }))
            .child(
                h_flex()
                    .id(id_str)
                    .test_support()
                    .w_full()
                    .gap_2p5()
                    .items_center()
                    .child(
                        Icon::new(action.icon.clone())
                            .small()
                            .text_color(cx.theme().primary),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_xs()
                                    .font_semibold()
                                    .child(action.title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(action.subtitle.clone()),
                            ),
                    )
                    .child(Tag::secondary().xsmall().child("Command")),
            )
            .into_any_element()
    }
}

impl Render for QuickSearchModalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_active_project = self.store.read(cx).snapshot().is_some();
        let active_project_label = self
            .store
            .read(cx)
            .snapshot()
            .map(|s| format!("Active Project ({})", s.project.key_prefix))
            .unwrap_or_else(|| "Active Project".to_string());
        let query = self.query_input.read(cx).value().trim().to_string();
        let entries = self.build_entries(cx);
        let entry_count = entries.len();
        let show_project_pill = self.scope == QuickSearchScope::AllProjects;

        let mut item_rows: Vec<(usize, SearchResultItem)> = Vec::new();
        let mut action_rows: Vec<(usize, QuickActionEntry)> = Vec::new();
        for (idx, entry) in entries.iter().enumerate() {
            match entry {
                PaletteEntry::Item(it) => item_rows.push((idx, it.clone())),
                PaletteEntry::Action(act) => action_rows.push((idx, act.clone())),
            }
        }

        v_flex()
            .w_full()
            .gap_3()
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "down" => {
                        if entry_count > 0 {
                            this.selected_index = (this.selected_index + 1) % entry_count;
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }
                    "up" => {
                        if entry_count > 0 {
                            this.selected_index = if this.selected_index == 0 {
                                entry_count - 1
                            } else {
                                this.selected_index - 1
                            };
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }
                    "escape" => {
                        window.close_dialog(cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.query_input)
                                .id("quick-search-input")
                                .small()
                                .cleanable(true)
                                .prefix(
                                    Icon::new(IconName::Search)
                                        .small()
                                        .text_color(cx.theme().muted_foreground),
                                ),
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .flex_shrink_0()
                            .child(
                                Button::new("quick-search-scope-active")
                                    .outline()
                                    .xsmall()
                                    .selected(self.scope == QuickSearchScope::ActiveProject)
                                    .disabled(!has_active_project)
                                    .label(active_project_label)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_scope(QuickSearchScope::ActiveProject, cx);
                                    })),
                            )
                            .child(
                                Button::new("quick-search-scope-all")
                                    .outline()
                                    .xsmall()
                                    .selected(self.scope == QuickSearchScope::AllProjects)
                                    .label("All Projects")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_scope(QuickSearchScope::AllProjects, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .w_full()
                    .h(px(340.))
                    .overflow_hidden()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(if query.is_empty() {
                        v_flex()
                            .size_full()
                            .p_2()
                            .gap_1()
                            .overflow_y_scrollbar()
                            .id("quick-search-results-scroll")
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .text_size(px(10.))
                                    .font_semibold()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("QUICK ACTIONS"),
                            )
                            .children(action_rows.into_iter().map(|(idx, act)| {
                                self.render_action_row(&act, self.selected_index == idx, cx)
                            }))
                            .when(!item_rows.is_empty(), |this| {
                                this.child(
                                    div()
                                        .px_2()
                                        .pt_2()
                                        .pb_1()
                                        .text_size(px(10.))
                                        .font_semibold()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("RECENT ITEMS"),
                                )
                                .children(item_rows.into_iter().map(|(idx, item)| {
                                    self.render_item_row(
                                        &item,
                                        idx,
                                        self.selected_index == idx,
                                        false,
                                        cx,
                                    )
                                }))
                            })
                            .into_any_element()
                    } else if self.is_searching && entries.is_empty() {
                        v_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .child(
                                Icon::new(IconName::LoaderCircle)
                                    .small()
                                    .text_color(cx.theme().primary),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Searching…"),
                            )
                            .into_any_element()
                    } else if entries.is_empty() {
                        v_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .p_6()
                            .gap_1p5()
                            .child(
                                Icon::new(IconName::Search)
                                    .large()
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .child(format!("No matches for \"{query}\"")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Try searching by item key (e.g. PRO-1), title, or switch to All Projects."),
                            )
                            .into_any_element()
                    } else {
                        v_flex()
                            .size_full()
                            .p_2()
                            .gap_0p5()
                            .overflow_y_scrollbar()
                            .id("quick-search-results-scroll")
                            .when(!item_rows.is_empty(), |this| {
                                this.child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .text_size(px(10.))
                                        .font_semibold()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("MATCHING ITEMS ({})", item_rows.len())),
                                )
                                .children(item_rows.into_iter().map(|(idx, item)| {
                                    self.render_item_row(
                                        &item,
                                        idx,
                                        self.selected_index == idx,
                                        show_project_pill,
                                        cx,
                                    )
                                }))
                            })
                            .when(!action_rows.is_empty(), |this| {
                                this.child(
                                    div()
                                        .px_2()
                                        .pt_2()
                                        .pb_1()
                                        .text_size(px(10.))
                                        .font_semibold()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("COMMANDS"),
                                )
                                .children(action_rows.into_iter().map(|(idx, act)| {
                                    self.render_action_row(&act, self.selected_index == idx, cx)
                                }))
                            })
                            .into_any_element()
                    }),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .child("↑↓ Navigate · Enter Open · Esc Close")
                    .child(
                        Button::new("quick-search-close")
                            .ghost()
                            .xsmall()
                            .label("Close")
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                            }),
                    ),
            )
    }
}

pub fn open_quick_search_modal(
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }

    let view = cx.new(|cx| QuickSearchModalView::new(app_state, store, window, cx));
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            let input = view.read(cx).query_input.clone();
            input.update(cx, |inp, cx| inp.focus(window, cx));
        }
        let view = view.clone();
        dialog
            .width(px(620.))
            .title("Quick Search & Commands")
            .content(move |content, _, _| content.child(view.clone()))
    });
}
