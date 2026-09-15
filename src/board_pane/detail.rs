use super::*;

impl BoardPaneView {
    pub(super) fn command_button(
        id: &'static str,
        label: &'static str,
        command: CommandId,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .px_2()
            .py_1()
            .text_size(px(12.0))
            .text_color(theme::accent())
            .child(label)
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(BoardCommand(command))))
    }

    pub(super) fn task_link(&self, item: &Item, cx: &mut Context<Self>) -> impl IntoElement {
        let id = item.id;
        div()
            .id(("board-task-link", id))
            .text_color(theme::accent())
            .child(item.title.clone())
            .on_click(cx.listener(move |view, _, _, cx| {
                view.navigation_item = Some(id);
                cx.emit(BoardCommand(CommandId::OpenBoardRelatedItem));
            }))
    }

    pub(super) fn render_detail(
        &self,
        item: &Item,
        comment_input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let all = self.list.read(cx).delegate().all.clone();
        let mut children: Vec<_> = all
            .iter()
            .filter(|child| child.parent == Some(item.id))
            .collect();
        children.sort_by(|a, b| a.rank.cmp(&b.rank).then(a.id.cmp(&b.id)));
        let unread = self.list.read(cx).delegate().unread.clone();
        let query = self.dependency_input.read(cx).value().to_lowercase();
        let candidates: Vec<_> = all
            .iter()
            .filter(|candidate| {
                candidate.id != item.id
                    && !item.depends_on.contains(&candidate.id)
                    && !query.trim().is_empty()
                    && candidate.title.to_lowercase().contains(query.trim())
            })
            .collect();
        let parent = item
            .parent
            .and_then(|id| all.iter().find(|item| item.id == id));
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .gap_2()
                    .child(Self::command_button(
                        "board-back",
                        "← Back to list",
                        CommandId::BackBoardList,
                        cx,
                    ))
                    .when(item.session_id.is_some(), |row| {
                        row.child(Self::command_button(
                            "board-session",
                            "Working history",
                            CommandId::OpenBoardTaskSession,
                            cx,
                        ))
                    }),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(div().px_2().child(error))
            })
            .child(
                v_flex()
                    .id(("board-detail-scroll", item.id))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .gap_3()
                    .when_some(parent, |column, parent| {
                        column.child(self.task_link(parent, cx))
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().text_size(px(18.0)).child(item.title.clone()))
                            .child(task_state(item)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Input::new(&self.state_input).appearance(false))
                            .child(Self::command_button(
                                "board-state",
                                "Save state",
                                CommandId::SaveBoardState,
                                cx,
                            ))
                            .child(Self::command_button(
                                "board-completed",
                                if item.completed {
                                    "Reopen"
                                } else {
                                    "Mark complete"
                                },
                                CommandId::ToggleBoardCompleted,
                                cx,
                            )),
                    )
                    .child(
                        TextView::markdown(("board-body", item.id), item.body.clone())
                            .text_color(theme::text_primary()),
                    )
                    .child(div().child("Prerequisites"))
                    .children(item.depends_on.iter().map(|dependency| {
                        let id = *dependency;
                        let name = all
                            .iter()
                            .find(|task| task.id == id)
                            .map(|task| task.title.clone())
                            .unwrap_or_else(|| format!("Task #{id}"));
                        h_flex().gap_2().child(name).child(
                            div()
                                .id(("board-remove-dependency", id))
                                .text_color(theme::accent())
                                .child("Remove")
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.pending_dependency = Some(id);
                                    cx.emit(BoardCommand(CommandId::RemoveBoardDependency));
                                })),
                        )
                    }))
                    .child(Input::new(&self.dependency_input).appearance(false))
                    .children(candidates.into_iter().map(|candidate| {
                        let id = candidate.id;
                        div()
                            .id(("board-add-dependency", id))
                            .text_color(theme::accent())
                            .child(candidate.title.clone())
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.pending_dependency = Some(id);
                                cx.emit(BoardCommand(CommandId::AddBoardDependency));
                            }))
                    }))
                    .child(div().child("Child tasks"))
                    .children(children.into_iter().map(|child| {
                        let id = child.id;
                        let dependencies = child
                            .depends_on
                            .iter()
                            .filter_map(|id| all.iter().find(|task| task.id == *id))
                            .map(|task| task.title.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        h_flex()
                            .id(("board-child", id))
                            .gap_2()
                            .on_drag(
                                BoardDragValue {
                                    item_id: id,
                                    title: child.title.clone(),
                                },
                                |drag, _, _, cx| cx.new(|_| drag.clone()),
                            )
                            .on_drag_move(cx.listener(
                                move |view, event: &DragMoveEvent<BoardDragValue>, _, cx| {
                                    let Some(half) =
                                        drop_half_for_row(&event.event.position, &event.bounds)
                                    else {
                                        return;
                                    };
                                    let dragged_id = event.drag(cx).item_id;
                                    let all = &view.list.read(cx).delegate().all;
                                    let next = drop_position_from_half(dragged_id, all, id, half)
                                        .map(|position| (dragged_id, position));
                                    if view.pending_move != next {
                                        view.pending_move = next;
                                        cx.notify();
                                    }
                                },
                            ))
                            .on_drop(cx.listener(move |view, drag: &BoardDragValue, _, cx| {
                                if view
                                    .pending_move
                                    .as_ref()
                                    .is_some_and(|(dragged, _)| *dragged == drag.item_id)
                                {
                                    cx.emit(BoardCommand(CommandId::ReorderBoardTask));
                                }
                            }))
                            .when(
                                cx.has_active_drag()
                                    && self.pending_move.as_ref().is_some_and(|(_, position)| {
                                        *position == Position::Before(id)
                                    }),
                                |row| row.border_t_2().border_color(theme::accent()),
                            )
                            .when(
                                cx.has_active_drag()
                                    && self.pending_move.as_ref().is_some_and(|(_, position)| {
                                        *position == Position::After(id)
                                    }),
                                |row| row.border_b_2().border_color(theme::accent()),
                            )
                            .child(self.task_link(child, cx))
                            .child(task_state(child))
                            .child(dependencies)
                            .child(if unread.contains(&id) { "●" } else { "" })
                            .children([(true, "↑"), (false, "↓")].into_iter().map(|(up, label)| {
                                div()
                                    .id((
                                        if up {
                                            "board-child-up"
                                        } else {
                                            "board-child-down"
                                        },
                                        id,
                                    ))
                                    .child(label)
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.navigation_item = Some(id);
                                        cx.emit(BoardCommand(if up {
                                            CommandId::MoveBoardTaskUp
                                        } else {
                                            CommandId::MoveBoardTaskDown
                                        }));
                                    }))
                            }))
                    }))
                    .child(Input::new(&self.new_item_input).appearance(false))
                    .child(div().child("Consultation"))
                    .children(item.comments.iter().map(|comment| {
                        let task = item.id;
                        let message = comment.id.clone();
                        let epoch = self.navigation_epoch;
                        let view = cx.entity().downgrade();
                        v_flex()
                            .relative()
                            .gap_1()
                            .when(comment.author == "system", |message| {
                                message.text_color(theme::danger())
                            })
                            .border_t_1()
                            .border_color(theme::border())
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(comment.author.clone())
                                    .child(comment.at.map(format_timestamp).unwrap_or_default()),
                            )
                            .child(
                                TextView::markdown(
                                    SharedString::from(format!("board-message-{}", comment.id)),
                                    comment.text.clone(),
                                )
                                .text_color(theme::text_primary()),
                            )
                            .child(
                                canvas(
                                    |_, _, _| {},
                                    move |bounds, _, window, cx| {
                                        let mask = window.content_mask().bounds;
                                        if message_visible(&bounds, &mask) {
                                            let view = view.clone();
                                            let message = message.clone();
                                            window.defer(cx, move |_, cx| {
                                                if let Some(view) = view.upgrade() {
                                                    view.update(cx, |view, cx| {
                                                        view.mark_displayed(
                                                            task, message, epoch, cx,
                                                        )
                                                    });
                                                }
                                            });
                                        }
                                    },
                                )
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full(),
                            )
                    }))
                    .child(Input::new(comment_input).appearance(false)),
            )
    }
}
