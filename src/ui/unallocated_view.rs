use std::collections::HashSet;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    WindowExt as _, h_flex, notification::Notification, v_flex,
};
use gpui_kit::{
    Anchor, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, prelude::FluentBuilder as _, px,
};

use crate::commands::{ConvertWorkItemToFeature, CreateWorkItem, SetWorkItemFeatures};
use crate::domain::{
    ItemId, ItemStatus, Priority, clamped_depth_badge, visual_indent_px,
};
use crate::state::{AppState, ProjectStore};
use crate::ui::{
    assignee_pill, domain_tag_chip, priority_badge, status_color, status_icon,
};

pub struct UnallocatedView {
    app_state: Entity<AppState>,
    store: Entity<ProjectStore>,
    quick_capture_input: Entity<InputState>,
    picker_search_input: Entity<InputState>,
    picker_item_id: Option<ItemId>,
    picker_draft_features: HashSet<ItemId>,
    _subscriptions: Vec<Subscription>,
}

impl UnallocatedView {
    pub fn new(
        app_state: Entity<AppState>,
        store: Entity<ProjectStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let quick_capture_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                "Capture unallocated work item… (Enter to add, ⌘Enter to add & assign features)",
            )
        });
        let picker_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter Features…"));

        let store_sub = cx.observe(&store, |this, store, cx| {
            if let Some(pid) = this.picker_item_id {
                let still_unallocated = store.read(cx).snapshot().is_some_and(|s| {
                    s.unallocated_work_items()
                        .iter()
                        .any(|wi| wi.id == pid)
                });
                if !still_unallocated {
                    this.picker_item_id = None;
                    this.picker_draft_features.clear();
                }
            }
            cx.notify();
        });

        let capture_sub = cx.subscribe_in(
            &quick_capture_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    cx.notify();
                }
                InputEvent::PressEnter { secondary, .. } => {
                    this.submit_quick_capture(*secondary, window, cx);
                }
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );

        let search_sub = cx.subscribe_in(
            &picker_search_input,
            window,
            |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        );

        Self {
            app_state,
            store,
            quick_capture_input,
            picker_search_input,
            picker_item_id: None,
            picker_draft_features: HashSet::new(),
            _subscriptions: vec![store_sub, capture_sub, search_sub],
        }
    }

    pub fn quick_capture_input(&self) -> &Entity<InputState> {
        &self.quick_capture_input
    }

    pub fn picker_search_input(&self) -> &Entity<InputState> {
        &self.picker_search_input
    }

    fn submit_quick_capture(
        &mut self,
        open_picker_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let raw = self.quick_capture_input.read(cx).value();
        let title = raw.trim().to_string();
        if title.is_empty() {
            return;
        }
        let Some(project_id) = self.store.read(cx).snapshot().map(|s| s.project.id) else {
            return;
        };

        self.quick_capture_input.update(cx, |inp, cx| {
            inp.set_value("", window, cx);
            if !open_picker_after {
                inp.focus(window, cx);
            }
        });
        cx.notify();

        let command = Box::new(CreateWorkItem::new(
            project_id,
            title,
            String::new(),
            ItemStatus::Todo,
            Priority::None,
            Vec::new(),
        ));
        let task = self
            .app_state
            .update(cx, |app, cx| app.execute_command(command, cx));
        let view = cx.entity().downgrade();

        window
            .spawn(cx, async move |cx| {
                let res = task.await;
                let _ = cx.update(|window, cx| match res {
                    Ok(effect) => {
                        if open_picker_after && let Some(created_id) = effect.created_item_id {
                            let _ = view.update(cx, |this, cx| {
                                this.picker_item_id = Some(created_id);
                                this.picker_draft_features.clear();
                                this.picker_search_input.update(cx, |inp, cx| {
                                    inp.set_value("", window, cx);
                                    inp.focus(window, cx);
                                });
                                cx.notify();
                            });
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

    fn apply_feature_assignment(
        &mut self,
        work_item_id: ItemId,
        feature_ids: Vec<ItemId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker_item_id = None;
        self.picker_draft_features.clear();
        cx.notify();

        let command = Box::new(SetWorkItemFeatures::new(work_item_id, feature_ids));
        let task = self
            .app_state
            .update(cx, |app, cx| app.execute_command(command, cx));

        window
            .spawn(cx, async move |cx| {
                let res = task.await;
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

impl Render for UnallocatedView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(snapshot) = self.store.read(cx).snapshot().cloned() else {
            return v_flex().flex_1().into_any_element();
        };

        let selected_item_id = self.store.read(cx).selected_item_id();
        let unallocated = snapshot.unallocated_work_items();
        let unallocated_count = unallocated.len();
        let capture_empty = self.quick_capture_input.read(cx).value().trim().is_empty();
        let search_focus = self.picker_search_input.read(cx).focus_handle(cx);
        let view_entity = cx.entity();

        v_flex()
            .flex_1()
            .min_h_0()
            .p_4()
            .gap_3()
            // Header with subtitle and live count
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Things we need to do but I haven't figured out when yet"),
                    )
                    .child(if unallocated_count > 0 {
                        Tag::warning()
                            .xsmall()
                            .outline()
                            .child(format!("{unallocated_count} unallocated"))
                    } else {
                        Tag::secondary()
                            .xsmall()
                            .child("0 unallocated")
                    }),
            )
            // Persistent quick-capture bar
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.quick_capture_input)
                                .id("unallocated-quick-capture")
                                .small()
                                .prefix(
                                    Icon::new(IconName::Plus)
                                        .small()
                                        .text_color(cx.theme().muted_foreground),
                                ),
                        ),
                    )
                    .child(
                        Button::new("unallocated-quick-add-btn")
                            .primary()
                            .small()
                            .label("Add")
                            .disabled(capture_empty)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit_quick_capture(false, window, cx);
                            })),
                    ),
            )
            // Row list
            .child(if unallocated.is_empty() {
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .p_8()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Inbox)
                            .large()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .child("No unallocated work items"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Type above and press Enter to brain-dump tasks for later triage."),
                    )
                    .into_any_element()
            } else {
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_1p5()
                    .overflow_y_scrollbar()
                    .id("center-unallocated-scroll")
                    .children(unallocated.into_iter().map(|wi| {
                        let wi_id = wi.id;
                        let wi_num = wi.item_number as usize;
                        let is_selected = selected_item_id == Some(wi_id);
                        let assignee = wi.assignee_id.and_then(|aid| snapshot.assignee(aid));
                        let tags = snapshot.tags_for_item(wi_id);
                        let picker_open = self.picker_item_id == Some(wi_id);

                        let view_for_change = view_entity.clone();
                        let view_for_content = view_entity.clone();
                        let search_focus = search_focus.clone();

                        h_flex()
                            .id(("unallocated-row", wi_num))
                            .w_full()
                            .px_3()
                            .py_2()
                            .gap_2p5()
                            .items_center()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(if is_selected {
                                cx.theme().primary.opacity(0.45)
                            } else {
                                cx.theme().border
                            })
                            .bg(if is_selected {
                                cx.theme().accent
                            } else {
                                cx.theme().popover
                            })
                            .cursor_pointer()
                            .when(!is_selected, |this| {
                                this.hover(|s| s.bg(cx.theme().list_hover))
                            })
                            .on_click({
                                let store = self.store.clone();
                                move |_, _, cx| {
                                    store.update(cx, |s, cx| {
                                        s.select_item(Some(wi_id));
                                        cx.notify();
                                    });
                                }
                            })
                            .child(
                                Icon::new(status_icon(wi.status))
                                    .small()
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
                                    .font_medium()
                                    .child(wi.title.clone()),
                            )
                            .when(!tags.is_empty(), |this| {
                                this.child(
                                    h_flex()
                                        .gap_1()
                                        .children(tags.into_iter().map(|tag| domain_tag_chip(tag, cx))),
                                )
                            })
                            .child(priority_badge(wi.priority, cx))
                            .when_some(assignee, |this, a| {
                                this.child(assignee_pill(a, false, cx))
                            })
                            .child(
                                Popover::new(("unallocated-assign-popover", wi_num))
                                    .anchor(Anchor::TopRight)
                                    .open(picker_open)
                                    .track_focus(&search_focus)
                                    .on_open_change(move |is_open, window, cx| {
                                        view_for_change.update(cx, |this, cx| {
                                            if *is_open {
                                                this.picker_item_id = Some(wi_id);
                                                this.picker_draft_features.clear();
                                                this.picker_search_input.update(cx, |inp, cx| {
                                                    inp.set_value("", window, cx);
                                                    inp.focus(window, cx);
                                                });
                                                this.store.update(cx, |s, cx| {
                                                    s.select_item(Some(wi_id));
                                                    cx.notify();
                                                });
                                            } else if this.picker_item_id == Some(wi_id) {
                                                this.picker_item_id = None;
                                                this.picker_draft_features.clear();
                                            }
                                            cx.notify();
                                        });
                                    })
                                    .trigger(
                                        Button::new(("unallocated-assign-btn", wi_num))
                                            .outline()
                                            .xsmall()
                                            .label("+ Assign Features")
                                            .dropdown_caret(true),
                                    )
                                    .content(move |_popover_state, _, cx| {
                                        let view_ref = view_for_content.read(cx);
                                        let search_input = view_ref.picker_search_input.clone();
                                        let query_raw = search_input.read(cx).value();
                                        let query = query_raw.trim().to_lowercase();
                                        let draft = view_ref.picker_draft_features.clone();
                                        let snap_opt = view_ref.store.read(cx).snapshot().cloned();

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
                                                    .map(|(feat, depth)| (feat.clone(), depth))
                                                    .collect()
                                            })
                                            .unwrap_or_default();

                                        let ordered_selected: Vec<ItemId> = snap_opt
                                            .as_ref()
                                            .map(|snap| {
                                                snap.active_features_dfs()
                                                    .into_iter()
                                                    .filter(|(f, _)| draft.contains(&f.id))
                                                    .map(|(f, _)| f.id)
                                                    .collect()
                                            })
                                            .unwrap_or_default();
                                        let selected_count = ordered_selected.len();

                                        let view_cancel = view_for_content.clone();
                                        let view_apply = view_for_content.clone();
                                        let popover_cancel = cx.entity();
                                        let popover_apply = cx.entity();

                                        v_flex()
                                            .w(px(320.))
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_semibold()
                                                    .child("Assign to Features"),
                                            )
                                            .child(
                                                Input::new(&search_input)
                                                    .id("unallocated-feature-search")
                                                    .small()
                                                    .cleanable(true),
                                            )
                                            .child(if features.is_empty() {
                                                div()
                                                    .px_2()
                                                    .py_4()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(if query.is_empty() {
                                                        "No Features in this Project yet."
                                                    } else {
                                                        "No matching Features."
                                                    })
                                                    .into_any_element()
                                            } else {
                                                v_flex()
                                                    .w_full()
                                                    .max_h(px(220.))
                                                    .overflow_hidden()
                                                    .child(
                                                        div().flex_1().overflow_hidden().child(
                                                            v_flex()
                                                                .size_full()
                                                                .gap_0p5()
                                                                .overflow_y_scrollbar()
                                                                .id("unallocated-picker-scroll")
                                                                .children(features.into_iter().map(
                                                                    |(feat, depth)| {
                                                            let fid = feat.id;
                                                            let checked = draft.contains(&fid);
                                                            let indent = if query.is_empty() {
                                                                visual_indent_px(depth).min(72.0)
                                                            } else {
                                                                0.0
                                                            };
                                                            let depth_badge =
                                                                clamped_depth_badge(depth);
                                                            let view_toggle =
                                                                view_for_content.clone();

                                                            ListItem::new((
                                                                "unallocated-picker-feature",
                                                                feat.item_number as usize,
                                                            ))
                                                            .flex_shrink_0()
                                                            .w_full()
                                                            .pl(px(8.0 + indent))
                                                            .pr_2()
                                                            .py_1p5()
                                                            .rounded(cx.theme().radius * 0.75)
                                                            .selected(checked)
                                                            .on_click(move |_, _, cx| {
                                                                view_toggle.update(
                                                                    cx,
                                                                    |this, cx| {
                                                                        if !this
                                                                            .picker_draft_features
                                                                            .remove(&fid)
                                                                        {
                                                                            this.picker_draft_features
                                                                                .insert(fid);
                                                                        }
                                                                        cx.notify();
                                                                    },
                                                                );
                                                            })
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
                                                                                    Icon::new(
                                                                                        IconName::Check,
                                                                                    )
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
                                                                                    .text_color(
                                                                                        cx.theme()
                                                                                            .muted_foreground,
                                                                                    )
                                                                                    .child(badge),
                                                                            )
                                                                        },
                                                                    ),
                                                            )
                                                        },
                                                    )),
                                                    ),
                                                    )
                                                    .into_any_element()
                                            })
                                            .child(
                                                h_flex()
                                                    .pt_1()
                                                    .border_t_1()
                                                    .border_color(cx.theme().border)
                                                    .items_center()
                                                    .justify_between()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(cx.theme().muted_foreground)
                                                            .child(format!(
                                                                "{selected_count} selected"
                                                            )),
                                                    )
                                                    .child(
                                                        h_flex()
                                                            .gap_1p5()
                                                            .child(
                                                                Button::new("unallocated-picker-cancel")
                                                                    .ghost()
                                                                    .xsmall()
                                                                    .label("Cancel")
                                                                    .on_click(move |_, window, cx| {
                                                                        view_cancel.update(cx, |this, cx| {
                                                                            this.picker_item_id = None;
                                                                            this.picker_draft_features.clear();
                                                                            cx.notify();
                                                                        });
                                                                        popover_cancel.update(cx, |st, cx| {
                                                                            st.dismiss(window, cx);
                                                                        });
                                                                    }),
                                                            )
                                                            .child(
                                                                Button::new("unallocated-picker-apply")
                                                                    .primary()
                                                                    .xsmall()
                                                                    .label(if selected_count > 0 {
                                                                        format!("Assign ({selected_count})")
                                                                    } else {
                                                                        "Assign".to_string()
                                                                    })
                                                                    .disabled(ordered_selected.is_empty())
                                                                    .on_click(move |_, window, cx| {
                                                                        let ids = ordered_selected.clone();
                                                                        view_apply.update(cx, |this, cx| {
                                                                            this.apply_feature_assignment(
                                                                                wi_id, ids, window, cx,
                                                                            );
                                                                        });
                                                                        popover_apply.update(cx, |st, cx| {
                                                                            st.dismiss(window, cx);
                                                                        });
                                                                    }),
                                                            ),
                                                    ),
                                            )
                                    })
                            )
                            .child({
                                let app_convert = self.app_state.clone();
                                Button::new(("unallocated-convert-btn", wi_num))
                                    .ghost()
                                    .xsmall()
                                    .label("→ Feature")
                                    .tooltip("Convert to root Feature (preserves key, tags, assignee, and blockers)")
                                    .on_click(move |_, window, cx| {
                                        cx.stop_propagation();
                                        let task = app_convert.update(cx, |app, cx| {
                                            app.execute_command(
                                                Box::new(ConvertWorkItemToFeature::new(wi_id)),
                                                cx,
                                            )
                                        });
                                        window
                                            .spawn(cx, async move |cx| {
                                                let res = task.await;
                                                let _ = cx.update(|window, cx| match res {
                                                    Ok(effect) => {
                                                        if let Some(msg) = effect.toast {
                                                            window.push_notification(
                                                                Notification::success(msg),
                                                                cx,
                                                            );
                                                        }
                                                    }
                                                    Err(err) => {
                                                        window.push_notification(
                                                            Notification::error(err.to_string()),
                                                            cx,
                                                        );
                                                    }
                                                });
                                            })
                                            .detach();
                                    })
                            })
                    }))
                    .into_any_element()
            })
            .into_any_element()
    }
}
