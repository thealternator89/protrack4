use std::cmp::Ordering;
use std::collections::HashSet;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{
    Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState,
};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    StyledExt as _, WindowExt as _, h_flex, notification::Notification, v_flex,
};
use gpui_kit::{
    App, AppContext as _, Context, Div, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Stateful, Styled as _, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::commands::{
    ConvertFeatureToWorkItem, ConvertWorkItemToFeature, TrashItemSubtree, UndoableCommand,
    UpdateItemFields,
};
use crate::domain::{
    Assignee, AssigneeId, BlockerWarning, ConversionEligibility, EffectiveBlockState, Item,
    ItemId, ItemKind, ItemStatus, Priority, ProjectId, ProjectSnapshot, Tag as DomainTag, TagId,
};
use crate::state::{AppState, ProjectStore};
use crate::ui::{
    assignee_pill, block_state_badge, blocker_warning_pill, domain_tag_chip, item_link_pill,
    kind_badge, parse_hex_or, priority_badge, priority_color, status_badge, status_icon,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TableScope {
    #[default]
    AllFeatures,
    SelectedSubtree,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KindFilter {
    #[default]
    All,
    FeaturesOnly,
    WorkItemsOnly,
}

impl KindFilter {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::All => "Kind: All",
            Self::FeaturesOnly => "Kind: Features",
            Self::WorkItemsOnly => "Kind: Work Items",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StatusFilter {
    #[default]
    All,
    ActiveOnly,
    Specific(ItemStatus),
}

impl StatusFilter {
    pub fn label(&self) -> String {
        match self {
            Self::All => "Status: All".to_string(),
            Self::ActiveOnly => "Status: Active".to_string(),
            Self::Specific(s) => format!("Status: {}", s.label()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PriorityFilter {
    #[default]
    All,
    Specific(Priority),
}

impl PriorityFilter {
    pub fn label(&self) -> String {
        match self {
            Self::All => "Priority: All".to_string(),
            Self::Specific(p) => format!("Priority: {}", p.label()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AssigneeFilter {
    #[default]
    All,
    Unassigned,
    Specific(AssigneeId),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BlockFilter {
    #[default]
    All,
    ReadyToWork,
    BlockedOnly,
    HasWarnings,
}

impl BlockFilter {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::All => "Block: All",
            Self::ReadyToWork => "Block: Ready to Work",
            Self::BlockedOnly => "Block: Blocked Only",
            Self::HasWarnings => "Block: Has Warnings",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableFilterState {
    pub query: String,
    pub scope: TableScope,
    pub kind: KindFilter,
    pub status: StatusFilter,
    pub priority: PriorityFilter,
    pub assignee: AssigneeFilter,
    pub tag_ids: HashSet<TagId>,
    pub block: BlockFilter,
}

impl TableFilterState {
    pub fn has_active_filters(&self) -> bool {
        !self.query.trim().is_empty()
            || self.scope != TableScope::AllFeatures
            || self.kind != KindFilter::All
            || self.status != StatusFilter::All
            || self.priority != PriorityFilter::All
            || self.assignee != AssigneeFilter::All
            || !self.tag_ids.is_empty()
            || self.block != BlockFilter::All
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSortColumn {
    Key,
    Title,
    Status,
    Priority,
    BlockState,
}

impl TableSortColumn {
    pub const fn col_ix(self) -> usize {
        match self {
            Self::Key => 0,
            Self::Title => 2,
            Self::Status => 4,
            Self::Priority => 5,
            Self::BlockState => 7,
        }
    }

    pub const fn from_col_ix(col_ix: usize) -> Option<Self> {
        match col_ix {
            0 => Some(Self::Key),
            2 => Some(Self::Title),
            4 => Some(Self::Status),
            5 => Some(Self::Priority),
            7 => Some(Self::BlockState),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRowData {
    pub item: Item,
    pub feature_labels: Vec<(ItemId, String)>,
    pub is_unallocated: bool,
    pub feature_breadcrumb: Option<String>,
    pub assignee: Option<Assignee>,
    pub tags: Vec<DomainTag>,
    pub block_state: EffectiveBlockState,
    pub primary_blocker_key: Option<String>,
    pub warnings: Vec<BlockerWarning>,
}

const fn status_sort_rank(status: ItemStatus) -> u8 {
    match status {
        ItemStatus::Backlog => 0,
        ItemStatus::Todo => 1,
        ItemStatus::InProgress => 2,
        ItemStatus::Done => 3,
        ItemStatus::Cancelled => 4,
    }
}

const fn block_sort_rank(state: EffectiveBlockState) -> u8 {
    match state {
        EffectiveBlockState::DirectlyBlocked { .. } => 0,
        EffectiveBlockState::InheritedBlocked { .. } => 1,
        EffectiveBlockState::PartiallyBlocked { .. } => 2,
        EffectiveBlockState::Unblocked => 3,
    }
}

fn collect_subtree_item_ids(
    snapshot: &ProjectSnapshot,
    root_feature_id: ItemId,
) -> Option<HashSet<ItemId>> {
    let root = snapshot.item(root_feature_id)?;
    if !root.is_active()
        || root.project_id != snapshot.project.id
        || root.kind != ItemKind::Feature
    {
        return None;
    }

    let mut features = snapshot.descendant_feature_ids(root_feature_id);
    features.insert(root_feature_id);

    let mut allowed = features.clone();
    for link in &snapshot.feature_work_items {
        if link.project_id == snapshot.project.id && features.contains(&link.feature_id) {
            allowed.insert(link.work_item_id);
        }
    }
    Some(allowed)
}

/// Projects, deduplicates, filters, and sorts active items for the Table View.
pub fn project_table_rows(
    snapshot: &ProjectSnapshot,
    filter: &TableFilterState,
    focused_feature_id: Option<ItemId>,
    sort: Option<(TableSortColumn, ColumnSort)>,
) -> Vec<TableRowData> {
    let scope_ids = match filter.scope {
        TableScope::AllFeatures => None,
        TableScope::SelectedSubtree => {
            focused_feature_id.and_then(|fid| collect_subtree_item_ids(snapshot, fid))
        }
    };

    let query = filter.query.trim().to_lowercase();

    let mut rows: Vec<TableRowData> = snapshot
        .active_items()
        .into_iter()
        .filter(|item| {
            if let Some(allowed) = &scope_ids
                && !allowed.contains(&item.id)
            {
                return false;
            }
            match filter.kind {
                KindFilter::All => true,
                KindFilter::FeaturesOnly => item.kind == ItemKind::Feature,
                KindFilter::WorkItemsOnly => item.kind == ItemKind::WorkItem,
            }
        })
        .filter(|item| match filter.status {
            StatusFilter::All => true,
            StatusFilter::ActiveOnly => matches!(
                item.status,
                ItemStatus::Backlog | ItemStatus::Todo | ItemStatus::InProgress
            ),
            StatusFilter::Specific(status) => item.status == status,
        })
        .filter(|item| match filter.priority {
            PriorityFilter::All => true,
            PriorityFilter::Specific(prio) => item.priority == prio,
        })
        .filter(|item| match filter.assignee {
            AssigneeFilter::All => true,
            AssigneeFilter::Unassigned => item.assignee_id.is_none(),
            AssigneeFilter::Specific(aid) => item.assignee_id == Some(aid),
        })
        .filter_map(|item| {
            let tags: Vec<DomainTag> = snapshot
                .tags_for_item(item.id)
                .into_iter()
                .cloned()
                .collect();

            if !filter.tag_ids.is_empty() {
                let item_tag_ids: HashSet<TagId> = tags.iter().map(|t| t.id).collect();
                if !filter.tag_ids.iter().all(|tid| item_tag_ids.contains(tid)) {
                    return None;
                }
            }

            let block_state = snapshot.effective_block_state(item.id);
            let warnings = snapshot.blocker_warnings(item.id).to_vec();

            let matches_block = match filter.block {
                BlockFilter::All => true,
                BlockFilter::ReadyToWork => block_state.is_ready_to_work(),
                BlockFilter::BlockedOnly => block_state.is_blocked(),
                BlockFilter::HasWarnings => !warnings.is_empty(),
            };
            if !matches_block {
                return None;
            }

            let (feature_labels, is_unallocated, feature_breadcrumb) = match item.kind {
                ItemKind::WorkItem => {
                    let feats = snapshot.features_for_work_item(item.id);
                    let unalloc = feats.is_empty();
                    let labels: Vec<(ItemId, String)> =
                        feats.into_iter().map(|f| (f.id, f.title.clone())).collect();
                    (labels, unalloc, None)
                }
                ItemKind::Feature => {
                    let path = snapshot.feature_path(item.id);
                    let ancestors: Vec<String> = path
                        .into_iter()
                        .filter(|f| f.id != item.id)
                        .map(|f| f.title.clone())
                        .collect();
                    let crumb = if ancestors.is_empty() {
                        None
                    } else {
                        Some(ancestors.join(" › "))
                    };
                    (Vec::new(), false, crumb)
                }
            };

            let assignee = item
                .assignee_id
                .and_then(|aid| snapshot.assignee(aid))
                .cloned();

            if !query.is_empty() {
                let mut matched = item.item_key.to_lowercase().contains(&query)
                    || item.title.to_lowercase().contains(&query)
                    || item.link.as_ref().is_some_and(|l| {
                        l.label.to_lowercase().contains(&query)
                            || l.url.to_lowercase().contains(&query)
                    })
                    || tags.iter().any(|t| t.name.to_lowercase().contains(&query))
                    || assignee.as_ref().is_some_and(|a| {
                        a.name.to_lowercase().contains(&query)
                            || a.handle.to_lowercase().contains(&query)
                    });

                if !matched {
                    matched = match item.kind {
                        ItemKind::WorkItem => {
                            if is_unallocated {
                                "unallocated".contains(&query)
                            } else {
                                feature_labels
                                    .iter()
                                    .any(|(_, name)| name.to_lowercase().contains(&query))
                            }
                        }
                        ItemKind::Feature => feature_breadcrumb
                            .as_ref()
                            .is_some_and(|c| c.to_lowercase().contains(&query)),
                    };
                }

                if !matched {
                    return None;
                }
            }

            let primary_blocker_key = snapshot
                .primary_active_direct_blocker_key(item.id)
                .map(str::to_string);

            Some(TableRowData {
                item: item.clone(),
                feature_labels,
                is_unallocated,
                feature_breadcrumb,
                assignee,
                tags,
                block_state,
                primary_blocker_key,
                warnings,
            })
        })
        .collect();

    sort_table_rows(&mut rows, sort);
    rows
}

fn sort_table_rows(rows: &mut [TableRowData], sort: Option<(TableSortColumn, ColumnSort)>) {
    let Some((col, dir)) = sort else {
        rows.sort_by_key(|r| r.item.item_number);
        return;
    };
    if dir == ColumnSort::Default {
        rows.sort_by_key(|r| r.item.item_number);
        return;
    }

    rows.sort_by(|a, b| {
        let ord = match col {
            TableSortColumn::Key => a.item.item_number.cmp(&b.item.item_number),
            TableSortColumn::Title => a
                .item
                .title
                .to_lowercase()
                .cmp(&b.item.title.to_lowercase())
                .then_with(|| a.item.title.cmp(&b.item.title)),
            TableSortColumn::Status => {
                status_sort_rank(a.item.status).cmp(&status_sort_rank(b.item.status))
            }
            TableSortColumn::Priority => a.item.priority.cmp(&b.item.priority),
            TableSortColumn::BlockState => block_sort_rank(a.block_state)
                .cmp(&block_sort_rank(b.block_state))
                .then_with(|| b.warnings.len().cmp(&a.warnings.len())),
        };
        let ordered = match dir {
            ColumnSort::Ascending => ord,
            ColumnSort::Descending => ord.reverse(),
            ColumnSort::Default => Ordering::Equal,
        };
        ordered.then_with(|| a.item.item_number.cmp(&b.item.item_number))
    });
}

pub struct ItemTableDelegate {
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    table_view: Entity<TableView>,
    snapshot: Option<ProjectSnapshot>,
    rows: Vec<TableRowData>,
    has_active_filters: bool,
    sort: Option<(TableSortColumn, ColumnSort)>,
}

impl ItemTableDelegate {
    fn new(
        app_state: Entity<AppState>,
        store: Entity<ProjectStore>,
        table_view: Entity<TableView>,
    ) -> Self {
        Self {
            app_state,
            store,
            table_view,
            snapshot: None,
            rows: Vec::new(),
            has_active_filters: false,
            sort: None,
        }
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

    fn col_sort(&self, col: TableSortColumn) -> ColumnSort {
        match self.sort {
            Some((active_col, dir)) if active_col == col => dir,
            _ => ColumnSort::Default,
        }
    }
}

impl TableDelegate for ItemTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        9
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        match col_ix {
            0 => Column::new("key", "Key")
                .width(px(84.))
                .min_width(px(70.))
                .fixed_left()
                .sort(self.col_sort(TableSortColumn::Key)),
            1 => Column::new("kind", "Kind")
                .width(px(104.))
                .min_width(px(90.)),
            2 => Column::new("title", "Title")
                .width(px(280.))
                .min_width(px(160.))
                .sort(self.col_sort(TableSortColumn::Title)),
            3 => Column::new("features", "Features")
                .width(px(210.))
                .min_width(px(120.)),
            4 => Column::new("status", "Status")
                .width(px(118.))
                .min_width(px(100.))
                .sort(self.col_sort(TableSortColumn::Status)),
            5 => Column::new("priority", "Priority")
                .width(px(96.))
                .min_width(px(80.))
                .sort(self.col_sort(TableSortColumn::Priority)),
            6 => Column::new("assignee", "Assignee")
                .width(px(84.))
                .min_width(px(64.)),
            7 => Column::new("block", "Block State")
                .width(px(148.))
                .min_width(px(110.))
                .sort(self.col_sort(TableSortColumn::BlockState)),
            _ => Column::new("link", "Link")
                .width(px(124.))
                .min_width(px(90.)),
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(col) = TableSortColumn::from_col_ix(col_ix) else {
            return;
        };
        let next_sort = if sort == ColumnSort::Default {
            None
        } else {
            Some((col, sort))
        };
        self.sort = next_sort;
        sort_table_rows(&mut self.rows, next_sort);

        let selected_id = self.store.read(cx).selected_item_id();
        let next_row = selected_id.and_then(|id| self.rows.iter().position(|r| r.item.id == id));
        let table_view = self.table_view.clone();
        cx.defer(move |cx| {
            table_view.update(cx, |view, cx| {
                view.set_active_sort(next_sort, next_row, cx);
            });
        });
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        if let Some(row) = self.rows.get(row_ix) {
            div().id(("table-row", row.item.item_number as usize))
        } else {
            div().id(("table-filler-row", row_ix))
        }
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let Some(row) = self.rows.get(row_ix) else {
            return menu;
        };
        let Some(snapshot) = &self.snapshot else {
            return menu;
        };

        let item = row.item.clone();
        let item_id = item.id;
        let is_feature = item.kind == ItemKind::Feature;
        let header = format!("{} · {}", item.item_key, item.kind.label());

        menu = menu.min_w(px(210.)).label(header);

        if let Some(link) = item.link.clone() {
            menu = menu
                .item(
                    PopupMenuItem::new(format!("Open Link ({})", link.label))
                        .icon(IconName::ExternalLink)
                        .on_click(move |_, _, cx| {
                            cx.open_url(&link.url);
                        }),
                )
                .separator();
        }

        // Status submenu
        let current_status = item.status;
        let app_status = self.app_state.clone();
        let status_submenu = PopupMenu::build(window, cx, move |mut sub, _, _| {
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
                            Self::dispatch_command(
                                app_click.clone(),
                                Box::new(UpdateItemFields::new(item_id).with_status(status)),
                                window,
                                cx,
                            );
                        }),
                );
            }
            sub
        });
        menu = menu.item(PopupMenuItem::submenu("Status", status_submenu));

        // Priority submenu
        let current_priority = item.priority;
        let app_priority = self.app_state.clone();
        let priority_submenu = PopupMenu::build(window, cx, move |mut sub, _, _| {
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
                        Self::dispatch_command(
                            app_click.clone(),
                            Box::new(UpdateItemFields::new(item_id).with_priority(priority)),
                            window,
                            cx,
                        );
                    }),
                );
            }
            sub
        });
        menu = menu.item(PopupMenuItem::submenu("Priority", priority_submenu));

        // Assignee submenu
        let current_assignee_id = item.assignee_id;
        let selectable_assignees: Vec<_> = snapshot
            .selectable_assignees(current_assignee_id)
            .into_iter()
            .cloned()
            .collect();
        let app_assign = self.app_state.clone();
        let assign_submenu = PopupMenu::build(window, cx, move |mut sub, _, _| {
            let app_unassign = app_assign.clone();
            sub = sub.item(
                PopupMenuItem::new("Unassigned")
                    .checked(current_assignee_id.is_none())
                    .on_click(move |_, window, cx| {
                        Self::dispatch_command(
                            app_unassign.clone(),
                            Box::new(UpdateItemFields::new(item_id).with_assignee_id(None)),
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
                        Self::dispatch_command(
                            app_pick.clone(),
                            Box::new(UpdateItemFields::new(item_id).with_assignee_id(Some(aid))),
                            window,
                            cx,
                        );
                    }),
                );
            }
            sub
        });
        menu = menu.item(PopupMenuItem::submenu("Assign", assign_submenu));

        let can_convert = snapshot
            .conversion_eligibility(item_id)
            .as_ref()
            .is_some_and(ConversionEligibility::is_allowed);
        let convert_label = if is_feature {
            "Convert to Work Item"
        } else {
            "Convert to Feature"
        };
        let app_convert = self.app_state.clone();

        menu = menu.separator().item(
            PopupMenuItem::new(convert_label)
                .icon(IconName::Replace)
                .disabled(!can_convert)
                .on_click(move |_, window, cx| {
                    let cmd: Box<dyn UndoableCommand> = if is_feature {
                        Box::new(ConvertFeatureToWorkItem::new(item_id))
                    } else {
                        Box::new(ConvertWorkItemToFeature::new(item_id))
                    };
                    Self::dispatch_command(app_convert.clone(), cmd, window, cx);
                }),
        );

        if is_feature {
            let store_focus = self.store.clone();
            let view_focus = self.table_view.clone();
            menu = menu.item(
                PopupMenuItem::new("Filter to Subtree")
                    .icon(IconName::FolderOpen)
                    .on_click(move |_, _, cx| {
                        store_focus.update(cx, |s, cx| {
                            s.select_item(Some(item_id));
                            s.set_focused_feature(Some(item_id));
                            cx.notify();
                        });
                        view_focus.update(cx, |this, cx| {
                            this.set_scope(TableScope::SelectedSubtree, cx);
                        });
                    }),
            );
        }

        let app_trash = self.app_state.clone();
        menu = menu.separator().item(
            PopupMenuItem::new("Move to Trash")
                .icon(IconName::Delete)
                .on_click(move |_, window, cx| {
                    Self::dispatch_command(
                        app_trash.clone(),
                        Box::new(TrashItemSubtree::new(item_id)),
                        window,
                        cx,
                    );
                }),
        );

        menu
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let msg = if self.has_active_filters {
            "No items match the current filters."
        } else {
            "No active items in this Project yet."
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_8()
            .child(
                Icon::new(IconName::Inbox)
                    .large()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(msg),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(row) = self.rows.get(row_ix) else {
            return div().into_any_element();
        };

        match col_ix {
            0 => h_flex()
                .h_full()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(cx.theme().muted_foreground)
                        .child(row.item.item_key.clone()),
                )
                .into_any_element(),
            1 => h_flex()
                .h_full()
                .items_center()
                .child(kind_badge(row.item.kind, cx))
                .into_any_element(),
            2 => h_flex()
                .h_full()
                .w_full()
                .min_w_0()
                .gap_1p5()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_xs()
                        .font_medium()
                        .when(row.item.status.is_closed(), |this| {
                            this.text_color(cx.theme().muted_foreground)
                        })
                        .when(row.item.status.is_cancelled(), |this| this.line_through())
                        .child(row.item.title.clone()),
                )
                .when(!row.tags.is_empty(), |this| {
                    this.child(
                        h_flex()
                            .gap_1()
                            .flex_shrink_0()
                            .children(row.tags.iter().take(2).map(|t| domain_tag_chip(t, cx)))
                            .when(row.tags.len() > 2, |row_el| {
                                row_el.child(
                                    Tag::secondary()
                                        .xsmall()
                                        .child(format!("+{}", row.tags.len() - 2)),
                                )
                            }),
                    )
                })
                .into_any_element(),
            3 => match row.item.kind {
                ItemKind::WorkItem => {
                    if row.is_unallocated {
                        h_flex()
                            .h_full()
                            .items_center()
                            .child(Tag::warning().xsmall().outline().child("Unallocated"))
                            .into_any_element()
                    } else {
                        h_flex()
                            .h_full()
                            .min_w_0()
                            .gap_1()
                            .items_center()
                            .overflow_hidden()
                            .children(row.feature_labels.iter().take(2).map(|(_, title)| {
                                Tag::secondary().xsmall().child(title.clone())
                            }))
                            .when(row.feature_labels.len() > 2, |this| {
                                this.child(
                                    Tag::secondary()
                                        .xsmall()
                                        .child(format!("+{}", row.feature_labels.len() - 2)),
                                )
                            })
                            .into_any_element()
                    }
                }
                ItemKind::Feature => h_flex()
                    .h_full()
                    .min_w_0()
                    .items_center()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                row.feature_breadcrumb
                                    .clone()
                                    .unwrap_or_else(|| "Root Feature".to_string()),
                            ),
                    )
                    .into_any_element(),
            },
            4 => h_flex()
                .h_full()
                .items_center()
                .child(status_badge(row.item.status, cx))
                .into_any_element(),
            5 => h_flex()
                .h_full()
                .items_center()
                .child(priority_badge(row.item.priority, cx))
                .into_any_element(),
            6 => h_flex()
                .h_full()
                .items_center()
                .child(match &row.assignee {
                    Some(a) => assignee_pill(a, false, cx).into_any_element(),
                    None => div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("—")
                        .into_any_element(),
                })
                .into_any_element(),
            7 => h_flex()
                .h_full()
                .items_center()
                .gap_1()
                .child(
                    if let Some(badge) = block_state_badge(
                        row.block_state,
                        row.primary_blocker_key.as_deref(),
                        cx,
                    ) {
                        badge
                    } else if let Some(warn) = row.warnings.first() {
                        blocker_warning_pill(warn, true, cx)
                    } else {
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Ready")
                            .into_any_element()
                    },
                )
                .into_any_element(),
            _ => h_flex()
                .h_full()
                .items_center()
                .child(match &row.item.link {
                    Some(link) => item_link_pill(
                        ("table-link", row.item.item_number as usize),
                        link,
                        cx,
                    )
                    .into_any_element(),
                    None => div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("—")
                        .into_any_element(),
                })
                .into_any_element(),
        }
    }
}

pub struct TableView {
    store: Entity<ProjectStore>,
    filter: TableFilterState,
    sort: Option<(TableSortColumn, ColumnSort)>,
    search_input: Entity<InputState>,
    table_state: Entity<TableState<ItemTableDelegate>>,
    last_project_id: Option<ProjectId>,
    syncing_selection: bool,
    _subscriptions: Vec<Subscription>,
}

impl TableView {
    pub fn new(
        app_state: Entity<AppState>,
        store: Entity<ProjectStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter key, title, feature, tag…"));

        let view_entity = cx.entity();
        let delegate = ItemTableDelegate::new(app_state, store.clone(), view_entity);
        let table_state = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .col_movable(false)
                .col_selectable(false)
                .row_selectable(true)
                .sortable(true)
        });

        let store_sub = cx.observe(&store, |this, _, cx| {
            this.sync_from_store(cx);
        });

        let search_sub = cx.subscribe_in(
            &search_input,
            window,
            |this, inp, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filter.query = inp.read(cx).value().to_string();
                    this.sync_from_store(cx);
                }
            },
        );

        let table_sub = cx.subscribe(&table_state, |this, table, event: &TableEvent, cx| {
            if this.syncing_selection {
                return;
            }
            if let TableEvent::SelectRow(row_ix) = event {
                let item_id = table
                    .read(cx)
                    .delegate()
                    .rows
                    .get(*row_ix)
                    .map(|r| r.item.id);
                if let Some(id) = item_id
                    && this.store.read(cx).selected_item_id() != Some(id)
                {
                    this.store.update(cx, |s, cx| {
                        s.select_item(Some(id));
                        cx.notify();
                    });
                }
            }
        });

        let mut this = Self {
            store,
            filter: TableFilterState::default(),
            sort: None,
            search_input,
            table_state,
            last_project_id: None,
            syncing_selection: false,
            _subscriptions: vec![store_sub, search_sub, table_sub],
        };
        this.sync_from_store(cx);
        this
    }

    pub fn search_input(&self) -> &Entity<InputState> {
        &self.search_input
    }

    pub fn table_state(&self) -> &Entity<TableState<ItemTableDelegate>> {
        &self.table_state
    }

    pub fn filter_state(&self) -> &TableFilterState {
        &self.filter
    }

    pub fn filtered_rows<'a>(&self, cx: &'a App) -> &'a [TableRowData] {
        &self.table_state.read(cx).delegate().rows
    }

    pub fn set_scope(&mut self, scope: TableScope, cx: &mut Context<Self>) {
        if scope == TableScope::SelectedSubtree
            && self.store.read(cx).focused_feature_id().is_none()
        {
            return;
        }
        self.filter.scope = scope;
        self.sync_from_store(cx);
    }

    pub fn set_kind_filter(&mut self, kind: KindFilter, cx: &mut Context<Self>) {
        self.filter.kind = kind;
        self.sync_from_store(cx);
    }

    pub fn set_status_filter(&mut self, status: StatusFilter, cx: &mut Context<Self>) {
        self.filter.status = status;
        self.sync_from_store(cx);
    }

    pub fn set_priority_filter(&mut self, priority: PriorityFilter, cx: &mut Context<Self>) {
        self.filter.priority = priority;
        self.sync_from_store(cx);
    }

    pub fn set_assignee_filter(&mut self, assignee: AssigneeFilter, cx: &mut Context<Self>) {
        self.filter.assignee = assignee;
        self.sync_from_store(cx);
    }

    pub fn set_block_filter(&mut self, block: BlockFilter, cx: &mut Context<Self>) {
        self.filter.block = block;
        self.sync_from_store(cx);
    }

    pub fn toggle_tag_filter(&mut self, tag_id: TagId, cx: &mut Context<Self>) {
        if !self.filter.tag_ids.remove(&tag_id) {
            self.filter.tag_ids.insert(tag_id);
        }
        self.sync_from_store(cx);
    }

    pub fn clear_tag_filters(&mut self, cx: &mut Context<Self>) {
        if !self.filter.tag_ids.is_empty() {
            self.filter.tag_ids.clear();
            self.sync_from_store(cx);
        }
    }

    pub fn clear_filters(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.filter.reset();
        if let Some(win) = window {
            self.search_input.update(cx, |inp, cx| {
                inp.set_value("", win, cx);
            });
        }
        self.sync_from_store(cx);
    }

    pub fn sort_by(&mut self, col: TableSortColumn, dir: ColumnSort, cx: &mut Context<Self>) {
        self.sort = if dir == ColumnSort::Default {
            None
        } else {
            Some((col, dir))
        };
        self.sync_from_store(cx);
    }

    fn set_active_sort(
        &mut self,
        sort: Option<(TableSortColumn, ColumnSort)>,
        selected_row: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.sort = sort;
        self.syncing_selection = true;
        self.table_state.update(cx, |ts, cx| {
            if let Some(ix) = selected_row {
                if ts.selected_row() != Some(ix) {
                    ts.set_selected_row(ix, cx);
                }
            } else if ts.selected_row().is_some() {
                ts.clear_selection(cx);
            }
        });
        self.syncing_selection = false;
        cx.notify();
    }

    fn sync_from_store(&mut self, cx: &mut Context<Self>) {
        let (snapshot, focused_feature_id, selected_item_id) = {
            let s = self.store.read(cx);
            (
                s.snapshot().cloned(),
                s.focused_feature_id(),
                s.selected_item_id(),
            )
        };

        let new_project_id = snapshot.as_ref().map(|s| s.project.id);
        if new_project_id != self.last_project_id {
            self.last_project_id = new_project_id;
            self.filter.tag_ids.clear();
        }

        if self.filter.scope == TableScope::SelectedSubtree && focused_feature_id.is_none() {
            self.filter.scope = TableScope::AllFeatures;
        }

        let rows = snapshot
            .as_ref()
            .map(|snap| project_table_rows(snap, &self.filter, focused_feature_id, self.sort))
            .unwrap_or_default();

        let target_row =
            selected_item_id.and_then(|id| rows.iter().position(|r| r.item.id == id));
        let has_active_filters = self.filter.has_active_filters();
        let sort = self.sort;

        self.syncing_selection = true;
        self.table_state.update(cx, |ts, cx| {
            let del = ts.delegate_mut();
            del.snapshot = snapshot;
            del.rows = rows;
            del.has_active_filters = has_active_filters;
            del.sort = sort;
            ts.refresh(cx);
            match target_row {
                Some(ix) => {
                    if ts.selected_row() != Some(ix) {
                        ts.set_selected_row(ix, cx);
                    }
                }
                None => {
                    if ts.selected_row().is_some() {
                        ts.clear_selection(cx);
                    }
                }
            }
        });
        self.syncing_selection = false;
        cx.notify();
    }
}

impl Render for TableView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (snapshot, focused_feature_id) = {
            let s = self.store.read(cx);
            (s.snapshot().cloned(), s.focused_feature_id())
        };

        let Some(snapshot) = snapshot else {
            return v_flex().flex_1().into_any_element();
        };

        let total_active = snapshot.active_items().len();
        let showing_count = self.table_state.read(cx).delegate().rows.len();
        let is_subtree =
            self.filter.scope == TableScope::SelectedSubtree && focused_feature_id.is_some();

        let assignee_label = match self.filter.assignee {
            AssigneeFilter::All => "Assignee: All".to_string(),
            AssigneeFilter::Unassigned => "Assignee: Unassigned".to_string(),
            AssigneeFilter::Specific(aid) => snapshot
                .assignee(aid)
                .map(|a| format!("Assignee: {}", a.handle))
                .unwrap_or_else(|| "Assignee: All".to_string()),
        };

        let tags_label = if self.filter.tag_ids.is_empty() {
            "Tags: All".to_string()
        } else {
            format!("Tags ({})", self.filter.tag_ids.len())
        };

        let view_kind = cx.entity();
        let kind_active = self.filter.kind;
        let view_status = cx.entity();
        let status_active = self.filter.status;
        let view_prio = cx.entity();
        let prio_active = self.filter.priority;
        let view_assignee = cx.entity();
        let assignee_active = self.filter.assignee;
        let assignees = snapshot.assignees.clone();
        let view_block = cx.entity();
        let block_active = self.filter.block;
        let view_tags = cx.entity();
        let selected_tags = self.filter.tag_ids.clone();
        let project_tags = snapshot.tags.clone();
        let has_filters = self.filter.has_active_filters();

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            // Multi-facet filter bar
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_1p5()
                    .flex_wrap()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(
                        div().w(px(210.)).child(
                            Input::new(&self.search_input)
                                .id("table-search-input")
                                .xsmall()
                                .cleanable(true),
                        ),
                    )
                    .child(
                        Button::new("table-scope-all")
                            .outline()
                            .xsmall()
                            .selected(!is_subtree)
                            .label("All")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_scope(TableScope::AllFeatures, cx);
                            })),
                    )
                    .child(
                        Button::new("table-scope-subtree")
                            .outline()
                            .xsmall()
                            .selected(is_subtree)
                            .disabled(focused_feature_id.is_none())
                            .label("Subtree")
                            .tooltip(if focused_feature_id.is_some() {
                                "Filter Table to selected Feature subtree"
                            } else {
                                "Select a Feature in Sidebar or Outline first"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_scope(TableScope::SelectedSubtree, cx);
                            })),
                    )
                    .child(
                        Button::new("table-filter-kind")
                            .outline()
                            .xsmall()
                            .selected(kind_active != KindFilter::All)
                            .dropdown_caret(true)
                            .label(kind_active.label())
                            .dropdown_menu(move |menu, _, _| {
                                let mut m = menu;
                                for k in [
                                    KindFilter::All,
                                    KindFilter::FeaturesOnly,
                                    KindFilter::WorkItemsOnly,
                                ] {
                                    let v = view_kind.clone();
                                    m = m.item(
                                        PopupMenuItem::new(k.label())
                                            .checked(kind_active == k)
                                            .on_click(move |_, _, cx| {
                                                v.update(cx, |this, cx| {
                                                    this.set_kind_filter(k, cx);
                                                });
                                            }),
                                    );
                                }
                                m
                            }),
                    )
                    .child(
                        Button::new("table-filter-status")
                            .outline()
                            .xsmall()
                            .selected(status_active != StatusFilter::All)
                            .dropdown_caret(true)
                            .label(status_active.label())
                            .dropdown_menu(move |mut menu, _, _| {
                                for s in [StatusFilter::All, StatusFilter::ActiveOnly] {
                                    let v = view_status.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(s.label())
                                            .checked(status_active == s)
                                            .on_click(move |_, _, cx| {
                                                v.update(cx, |this, cx| {
                                                    this.set_status_filter(s, cx);
                                                });
                                            }),
                                    );
                                }
                                menu = menu.separator();
                                for st in [
                                    ItemStatus::Backlog,
                                    ItemStatus::Todo,
                                    ItemStatus::InProgress,
                                    ItemStatus::Done,
                                    ItemStatus::Cancelled,
                                ] {
                                    let s = StatusFilter::Specific(st);
                                    let v = view_status.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(st.label())
                                            .icon(status_icon(st))
                                            .checked(status_active == s)
                                            .on_click(move |_, _, cx| {
                                                v.update(cx, |this, cx| {
                                                    this.set_status_filter(s, cx);
                                                });
                                            }),
                                    );
                                }
                                menu
                            }),
                    )
                    .child(
                        Button::new("table-filter-priority")
                            .outline()
                            .xsmall()
                            .selected(prio_active != PriorityFilter::All)
                            .dropdown_caret(true)
                            .label(prio_active.label())
                            .dropdown_menu(move |mut menu, _, _| {
                                let v_all = view_prio.clone();
                                menu = menu
                                    .item(
                                        PopupMenuItem::new("All Priorities")
                                            .checked(prio_active == PriorityFilter::All)
                                            .on_click(move |_, _, cx| {
                                                v_all.update(cx, |this, cx| {
                                                    this.set_priority_filter(
                                                        PriorityFilter::All,
                                                        cx,
                                                    );
                                                });
                                            }),
                                    )
                                    .separator();
                                for p in [
                                    Priority::Urgent,
                                    Priority::High,
                                    Priority::Medium,
                                    Priority::Low,
                                    Priority::None,
                                ] {
                                    let pf = PriorityFilter::Specific(p);
                                    let v = view_prio.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(p.label())
                                            .checked(prio_active == pf)
                                            .on_click(move |_, _, cx| {
                                                v.update(cx, |this, cx| {
                                                    this.set_priority_filter(pf, cx);
                                                });
                                            }),
                                    );
                                }
                                menu
                            }),
                    )
                    .child(
                        Button::new("table-filter-assignee")
                            .outline()
                            .xsmall()
                            .selected(assignee_active != AssigneeFilter::All)
                            .dropdown_caret(true)
                            .label(assignee_label)
                            .dropdown_menu(move |mut menu, _, _| {
                                let v_all = view_assignee.clone();
                                let v_un = view_assignee.clone();
                                menu = menu
                                    .item(
                                        PopupMenuItem::new("All Assignees")
                                            .checked(assignee_active == AssigneeFilter::All)
                                            .on_click(move |_, _, cx| {
                                                v_all.update(cx, |this, cx| {
                                                    this.set_assignee_filter(
                                                        AssigneeFilter::All,
                                                        cx,
                                                    );
                                                });
                                            }),
                                    )
                                    .item(
                                        PopupMenuItem::new("Unassigned")
                                            .checked(assignee_active == AssigneeFilter::Unassigned)
                                            .on_click(move |_, _, cx| {
                                                v_un.update(cx, |this, cx| {
                                                    this.set_assignee_filter(
                                                        AssigneeFilter::Unassigned,
                                                        cx,
                                                    );
                                                });
                                            }),
                                    );
                                if !assignees.is_empty() {
                                    menu = menu.separator();
                                }
                                for a in &assignees {
                                    let af = AssigneeFilter::Specific(a.id);
                                    let v = view_assignee.clone();
                                    let label = if a.is_archived() {
                                        format!("{} · {} (Archived)", a.handle, a.name)
                                    } else {
                                        format!("{} · {}", a.handle, a.name)
                                    };
                                    menu = menu.item(
                                        PopupMenuItem::new(label)
                                            .checked(assignee_active == af)
                                            .on_click(move |_, _, cx| {
                                                v.update(cx, |this, cx| {
                                                    this.set_assignee_filter(af, cx);
                                                });
                                            }),
                                    );
                                }
                                menu
                            }),
                    )
                    .child(
                        Button::new("table-filter-block")
                            .outline()
                            .xsmall()
                            .selected(block_active != BlockFilter::All)
                            .dropdown_caret(true)
                            .label(block_active.label())
                            .dropdown_menu(move |mut menu, _, _| {
                                for b in [
                                    BlockFilter::All,
                                    BlockFilter::ReadyToWork,
                                    BlockFilter::BlockedOnly,
                                    BlockFilter::HasWarnings,
                                ] {
                                    let v = view_block.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(b.label())
                                            .checked(block_active == b)
                                            .on_click(move |_, _, cx| {
                                                v.update(cx, |this, cx| {
                                                    this.set_block_filter(b, cx);
                                                });
                                            }),
                                    );
                                }
                                menu
                            }),
                    )
                    .child(
                        Button::new("table-filter-tags")
                            .outline()
                            .xsmall()
                            .selected(!self.filter.tag_ids.is_empty())
                            .dropdown_caret(true)
                            .label(tags_label)
                            .dropdown_menu(move |mut menu, _, _| {
                                if project_tags.is_empty() {
                                    return menu.item(PopupMenuItem::label("No tags in project"));
                                }
                                let v_clear = view_tags.clone();
                                menu = menu
                                    .item(
                                        PopupMenuItem::new("All Tags")
                                            .checked(selected_tags.is_empty())
                                            .on_click(move |_, _, cx| {
                                                v_clear.update(cx, |this, cx| {
                                                    this.clear_tag_filters(cx);
                                                });
                                            }),
                                    )
                                    .separator();
                                for tag in &project_tags {
                                    let tid = tag.id;
                                    let checked = selected_tags.contains(&tid);
                                    let v = view_tags.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(tag.name.clone())
                                            .checked(checked)
                                            .on_click(move |_, _, cx| {
                                                v.update(cx, |this, cx| {
                                                    this.toggle_tag_filter(tid, cx);
                                                });
                                            }),
                                    );
                                }
                                menu
                            }),
                    )
                    .when(has_filters, |this| {
                        this.child(
                            Button::new("table-filter-clear")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .label("Clear")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.clear_filters(Some(window), cx);
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("table-showing-count")
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if showing_count == total_active {
                                format!("{total_active} items")
                            } else {
                                format!("{total_active} items (showing {showing_count})")
                            }),
                    ),
            )
            // Virtualized DataTable
            .child(
                div().flex_1().min_h_0().w_full().child(
                    DataTable::new(&self.table_state)
                        .small()
                        .stripe(true)
                        .bordered(false),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{DateTime, TimeZone, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::domain::{FeatureWorkItem, Project, ProjectStatus};

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 24, 12, 0, 0).unwrap()
    }

    fn build_table_test_snapshot() -> (
        ProjectSnapshot,
        ItemId,
        ItemId,
        ItemId,
        ItemId,
        ItemId,
        ItemId,
        ItemId,
        AssigneeId,
        TagId,
    ) {
        let pid = ProjectId::from_uuid(Uuid::from_u128(10));
        let a1 = AssigneeId::from_uuid(Uuid::from_u128(20));
        let t_bug = TagId::from_uuid(Uuid::from_u128(30));

        let f1 = ItemId::from_uuid(Uuid::from_u128(101));
        let f2 = ItemId::from_uuid(Uuid::from_u128(102));
        let f3 = ItemId::from_uuid(Uuid::from_u128(103));
        let f_blk = ItemId::from_uuid(Uuid::from_u128(104));

        let wi_shared_3 = ItemId::from_uuid(Uuid::from_u128(201));
        let wi_partial = ItemId::from_uuid(Uuid::from_u128(202));
        let wi_inherited = ItemId::from_uuid(Uuid::from_u128(203));
        let wi_warn = ItemId::from_uuid(Uuid::from_u128(204));

        let project = Project {
            id: pid,
            name: "TableProj".to_string(),
            key_prefix: "TBL".to_string(),
            next_item_seq: 9,
            description: String::new(),
            color: "#6366F1".to_string(),
            status: ProjectStatus::Active,
            sort_order: "a0".to_string(),
            created_at: ts(),
            updated_at: ts(),
        };

        let make_item = |id, num, kind, parent, title: &str, status, prio, assignee| Item {
            id,
            project_id: pid,
            item_number: num,
            item_key: format!("TBL-{num}"),
            kind,
            parent_feature_id: parent,
            title: title.to_string(),
            description: String::new(),
            status,
            priority: prio,
            assignee_id: assignee,
            link: None,
            sort_order: format!("a{num}"),
            deleted_at: None,
            trash_root_id: None,
            created_at: ts(),
            updated_at: ts(),
        };

        let mut effective_block_state = HashMap::new();
        effective_block_state.insert(f1, EffectiveBlockState::DirectlyBlocked { active_count: 1 });
        effective_block_state.insert(
            f2,
            EffectiveBlockState::InheritedBlocked {
                ancestor_blocker_count: 1,
            },
        );
        effective_block_state.insert(f3, EffectiveBlockState::Unblocked);
        effective_block_state.insert(f_blk, EffectiveBlockState::Unblocked);
        effective_block_state.insert(
            wi_shared_3,
            EffectiveBlockState::PartiallyBlocked {
                blocked_features: 2,
                total_features: 3,
            },
        );
        effective_block_state.insert(
            wi_partial,
            EffectiveBlockState::PartiallyBlocked {
                blocked_features: 1,
                total_features: 2,
            },
        );
        effective_block_state.insert(
            wi_inherited,
            EffectiveBlockState::InheritedBlocked {
                ancestor_blocker_count: 1,
            },
        );
        effective_block_state.insert(wi_warn, EffectiveBlockState::Unblocked);

        let mut blocker_warnings = HashMap::new();
        blocker_warnings.insert(
            wi_warn,
            vec![BlockerWarning::Cancelled {
                blocker_id: f_blk,
                blocker_key: "TBL-4".to_string(),
                title: "Blocker".to_string(),
            }],
        );

        let mut item_tags = HashMap::new();
        item_tags.insert(wi_shared_3, vec![t_bug]);
        item_tags.insert(wi_inherited, vec![t_bug]);

        let snap = ProjectSnapshot {
            project: project.clone(),
            projects: vec![project],
            items: vec![
                make_item(f1, 1, ItemKind::Feature, None, "Core", ItemStatus::Todo, Priority::High, Some(a1)),
                make_item(f2, 2, ItemKind::Feature, Some(f1), "Auth", ItemStatus::InProgress, Priority::Medium, None),
                make_item(f3, 3, ItemKind::Feature, None, "CLI", ItemStatus::Done, Priority::Low, None),
                make_item(f_blk, 4, ItemKind::Feature, None, "Blocker", ItemStatus::Cancelled, Priority::Urgent, Some(a1)),
                make_item(wi_shared_3, 5, ItemKind::WorkItem, None, "Shared 3", ItemStatus::Todo, Priority::High, Some(a1)),
                make_item(wi_partial, 6, ItemKind::WorkItem, None, "Partial WI", ItemStatus::InProgress, Priority::Medium, None),
                make_item(wi_inherited, 7, ItemKind::WorkItem, None, "Inherited WI", ItemStatus::Backlog, Priority::High, Some(a1)),
                make_item(wi_warn, 8, ItemKind::WorkItem, None, "Warn WI", ItemStatus::Todo, Priority::None, None),
            ],
            assignees: vec![Assignee {
                id: a1,
                name: "Mark".to_string(),
                handle: "MB".to_string(),
                color: "#6366F1".to_string(),
                role: None,
                notes: String::new(),
                archived_at: None,
                created_at: ts(),
                updated_at: ts(),
            }],
            tags: vec![DomainTag {
                id: t_bug,
                project_id: pid,
                name: "bug".to_string(),
                color: "#EF4444".to_string(),
                created_at: ts(),
            }],
            feature_work_items: vec![
                FeatureWorkItem {
                    project_id: pid,
                    feature_id: f1,
                    work_item_id: wi_shared_3,
                    sort_order: "a0".to_string(),
                    created_at: ts(),
                },
                FeatureWorkItem {
                    project_id: pid,
                    feature_id: f2,
                    work_item_id: wi_shared_3,
                    sort_order: "a0".to_string(),
                    created_at: ts(),
                },
                FeatureWorkItem {
                    project_id: pid,
                    feature_id: f3,
                    work_item_id: wi_shared_3,
                    sort_order: "a0".to_string(),
                    created_at: ts(),
                },
                FeatureWorkItem {
                    project_id: pid,
                    feature_id: f2,
                    work_item_id: wi_partial,
                    sort_order: "a1".to_string(),
                    created_at: ts(),
                },
                FeatureWorkItem {
                    project_id: pid,
                    feature_id: f3,
                    work_item_id: wi_partial,
                    sort_order: "a1".to_string(),
                    created_at: ts(),
                },
                FeatureWorkItem {
                    project_id: pid,
                    feature_id: f2,
                    work_item_id: wi_inherited,
                    sort_order: "a2".to_string(),
                    created_at: ts(),
                },
            ],
            item_tags,
            progress_by_feature: HashMap::new(),
            item_blockers: Vec::new(),
            direct_blockers_by_item: HashMap::new(),
            inherited_blockers_by_item: HashMap::new(),
            effective_block_state,
            blocker_warnings,
            trash_count: 0,
            trash_groups: Vec::new(),
        };

        (snap, f1, f2, f3, wi_shared_3, wi_partial, wi_inherited, wi_warn, a1, t_bug)
    }

    #[test]
    fn deduplicates_multi_feature_work_items_and_ready_filter_follows_inv06() {
        let (snap, f1, _f2, _f3, wi_shared_3, wi_partial, wi_inherited, wi_warn, a1, t_bug) =
            build_table_test_snapshot();

        // 1. Project-wide deduplication: wi_shared_3 appears exactly ONCE with 3 Feature badges
        let all = project_table_rows(&snap, &TableFilterState::default(), None, None);
        assert_eq!(all.len(), 8);
        let shared_rows: Vec<_> = all.iter().filter(|r| r.item.id == wi_shared_3).collect();
        assert_eq!(shared_rows.len(), 1);
        assert_eq!(shared_rows[0].feature_labels.len(), 3);

        // Subtree scope on F1 also deduplicates wi_shared_3 (attached to F1 and F2)
        let subtree_filter = TableFilterState {
            scope: TableScope::SelectedSubtree,
            ..Default::default()
        };
        let subtree_rows = project_table_rows(&snap, &subtree_filter, Some(f1), None);
        assert_eq!(
            subtree_rows.iter().filter(|r| r.item.id == wi_shared_3).count(),
            1
        );
        assert_eq!(subtree_rows.len(), 5); // f1, f2, wi_shared_3, wi_partial, wi_inherited

        // 2. INV-06 ReadyToWork includes Unblocked + PartiallyBlocked; excludes DirectlyBlocked + InheritedBlocked
        let ready_filter = TableFilterState {
            block: BlockFilter::ReadyToWork,
            ..Default::default()
        };
        let ready_ids: HashSet<ItemId> = project_table_rows(&snap, &ready_filter, None, None)
            .into_iter()
            .map(|r| r.item.id)
            .collect();
        assert!(ready_ids.contains(&wi_shared_3));
        assert!(ready_ids.contains(&wi_partial));
        assert!(ready_ids.contains(&wi_warn));
        assert!(!ready_ids.contains(&f1));
        assert!(!ready_ids.contains(&wi_inherited));

        // 3. BlockedOnly includes only DirectlyBlocked and InheritedBlocked
        let blocked_filter = TableFilterState {
            block: BlockFilter::BlockedOnly,
            ..Default::default()
        };
        let blocked_ids: HashSet<ItemId> = project_table_rows(&snap, &blocked_filter, None, None)
            .into_iter()
            .map(|r| r.item.id)
            .collect();
        assert_eq!(blocked_ids.len(), 3);
        assert!(blocked_ids.contains(&f1));
        assert!(blocked_ids.contains(&wi_inherited));

        // 4. HasWarnings surfaces wi_warn
        let warn_filter = TableFilterState {
            block: BlockFilter::HasWarnings,
            ..Default::default()
        };
        let warn_rows = project_table_rows(&snap, &warn_filter, None, None);
        assert_eq!(warn_rows.len(), 1);
        assert_eq!(warn_rows[0].item.id, wi_warn);

        // 5. Combined multi-facet filter + sorting
        let multi_filter = TableFilterState {
            kind: KindFilter::WorkItemsOnly,
            priority: PriorityFilter::Specific(Priority::High),
            assignee: AssigneeFilter::Specific(a1),
            tag_ids: HashSet::from([t_bug]),
            block: BlockFilter::ReadyToWork,
            ..Default::default()
        };
        let multi_rows = project_table_rows(&snap, &multi_filter, None, None);
        assert_eq!(multi_rows.len(), 1);
        assert_eq!(multi_rows[0].item.id, wi_shared_3);

        // Sort by Priority Descending -> Urgent (TBL-4) first
        let sorted_prio = project_table_rows(
            &snap,
            &TableFilterState::default(),
            None,
            Some((TableSortColumn::Priority, ColumnSort::Descending)),
        );
        assert_eq!(sorted_prio[0].item.priority, Priority::Urgent);

        // Sort by Title Ascending -> Auth (TBL-2) first
        let sorted_title = project_table_rows(
            &snap,
            &TableFilterState::default(),
            None,
            Some((TableSortColumn::Title, ColumnSort::Ascending)),
        );
        assert_eq!(sorted_title[0].item.title, "Auth");
    }
}
