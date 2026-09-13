use super::*;
use horizon_board::workflow::{Mutation, Work};

impl BoardPaneView {
    pub(crate) fn set_workflow_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.workflow_error = Some(error);
        cx.notify();
    }
    pub(crate) fn milestone_session(&self) -> Option<horizon_workspace::SessionId> {
        let BoardPaneMode::Detail { item, .. } = &self.mode else {
            return None;
        };
        let flow = item.workflow.as_ref()?;
        let value = flow
            .active
            .as_ref()
            .map(|a| a.session.as_str())
            .or_else(|| {
                flow.problem
                    .as_ref()
                    .and(flow.last_attempt.as_ref())
                    .map(|last| last.attempt.session.as_str())
            })
            .or_else(|| flow.worker.as_ref().map(|w| w.session.as_str()))?;
        uuid::Uuid::parse_str(value)
            .ok()
            .map(horizon_workspace::SessionId::from_uuid)
    }

    pub(crate) fn workflow_command(
        &mut self,
        command: CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if command == CommandId::ToggleBoardMilestoneFilter {
            self.list.update(cx, |list, cx| {
                let delegate = list.delegate_mut();
                delegate.milestones_only = !delegate.milestones_only;
                delegate.rederive();
                cx.notify();
            });
            cx.notify();
            return;
        }
        if command == CommandId::ToggleBoardHistory {
            self.show_history = !self.show_history;
            cx.notify();
            return;
        }
        if self.workflow_busy {
            return;
        }
        let BoardPaneMode::Detail { item, .. } = &self.mode else {
            return;
        };
        let mutation = match command {
            CommandId::EnableBoardMilestone => Mutation::Enable,
            CommandId::PauseBoardMilestone => Mutation::Pause,
            CommandId::ResumeBoardMilestone => Mutation::Resume,
            CommandId::ReplanBoardMilestone => Mutation::Replan,
            CommandId::SubmitBoardDecision => {
                let Some(decision) = item
                    .workflow
                    .as_ref()
                    .and_then(|f| f.unanswered().first().copied())
                else {
                    return;
                };
                Mutation::Answer {
                    key: decision.key.clone(),
                    text: self.decision_input.read(cx).value().to_string(),
                }
            }
            _ => return,
        };
        let Some(root) = self.root.clone() else {
            return;
        };
        let id = item.id;
        let revision = item.workflow.as_ref().map_or(0, |f| f.revision);
        let answer = if let Mutation::Answer { text, .. } = &mutation {
            Some(text.clone())
        } else {
            None
        };
        self.workflow_busy = true;
        self.workflow_error = None;
        cx.notify();
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| e.to_string())?;
                    let store = Store::from_dir(&root).map_err(|e| e.to_string())?;
                    runtime
                        .block_on(store.workflow(id, revision, mutation))
                        .map_err(|e| e.to_string())
                })
                .await;
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |view, cx| {
                    view.workflow_busy = false;
                    match result {
                        Ok(updated) => {
                            if let BoardPaneMode::Detail { item, .. } = &mut view.mode {
                                if item.id == id {
                                    **item = updated;
                                    if answer.as_ref().is_some_and(|text| {
                                        text == view.decision_input.read(cx).value().as_str()
                                    }) {
                                        view.decision_input.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                    }
                                }
                            }
                            view.spawn_load(cx);
                        }
                        Err(error) => {
                            view.workflow_error = Some(error);
                            view.spawn_show(id, cx);
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn workflow_button(
        &self,
        id: &'static str,
        label: &'static str,
        command: CommandId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(theme::surface_selected())
            .text_color(theme::readable_on(
                theme::text_primary(),
                theme::surface_selected(),
            ))
            .child(label)
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(BoardCommand(command))))
            .into_any_element()
    }

    pub(super) fn render_workflow(&self, item: &Item, cx: &mut Context<Self>) -> AnyElement {
        let mut content = v_flex().gap_3().w_full().text_size(px(12.0));
        if let Some(error) = &self.workflow_error {
            content = content.child(div().child(error.clone()));
        }
        if self.workflow_busy {
            content = content.child("Saving…");
        }
        let Some(flow) = &item.workflow else {
            return content
                .child(self.workflow_button(
                    "milestone-enable",
                    "Plan and run as milestone",
                    CommandId::EnableBoardMilestone,
                    cx,
                ))
                .into_any_element();
        };
        content = content.child(
            div()
                .text_size(px(14.0))
                .child(format!("Milestone · {}", flow.label())),
        );
        let mut actions = h_flex().flex_wrap().gap_2();
        if !flow.paused {
            actions = actions.child(self.workflow_button(
                "milestone-pause",
                "Pause",
                CommandId::PauseBoardMilestone,
                cx,
            ));
        }
        if flow.active.is_none() {
            if flow.paused || flow.problem.is_some() {
                actions = actions.child(self.workflow_button(
                    "milestone-resume",
                    "Retry / resume",
                    CommandId::ResumeBoardMilestone,
                    cx,
                ));
            }
            actions = actions.child(self.workflow_button(
                "milestone-replan",
                "Revise plan",
                CommandId::ReplanBoardMilestone,
                cx,
            ));
        }
        if self.milestone_session().is_some() {
            actions = actions.child(self.workflow_button(
                "milestone-session",
                "Open session",
                CommandId::OpenBoardMilestoneSession,
                cx,
            ));
        }
        content = content.child(actions);
        if let Some(problem) = &flow.problem {
            content = content.child(div().child(problem.clone()));
        }
        if let Some(active) = &flow.active {
            if let Some(attention) = &active.attention {
                content = content.child(div().child(attention.clone()));
            }
        }
        if let Some(decision) = flow.unanswered().first() {
            content = content.child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .border_1()
                    .border_color(theme::border())
                    .child(format!(
                        "Your decision · {} remaining",
                        flow.unanswered().len()
                    ))
                    .child(div().text_size(px(14.0)).child(decision.question.clone()))
                    .child(decision.context.clone())
                    .child(format!("Recommendation: {}", decision.recommendation))
                    .child(format!("Effect: {}", decision.consequence))
                    .child(Input::new(&self.decision_input))
                    .child(self.workflow_button(
                        "milestone-answer",
                        "Send answer",
                        CommandId::SubmitBoardDecision,
                        cx,
                    )),
            );
        }
        if let Some(plan) = &flow.plan {
            content = content
                .child(div().text_size(px(14.0)).child("Current plan"))
                .child(plan.summary.clone())
                .child(
                    v_flex().gap_1().children(
                        plan.acceptance
                            .iter()
                            .map(|criterion| div().child(format!("• {criterion}"))),
                    ),
                )
                .child("Tasks · priority order; dependencies run first");
            for task in &plan.tasks {
                let result = flow.results.iter().find(|r| r.key == task.key);
                let running = flow.active.as_ref().is_some_and(|a| {
                    a.work
                        == Work::Task {
                            key: task.key.clone(),
                        }
                });
                let status = if result.is_some() {
                    "implemented"
                } else if running {
                    "running"
                } else {
                    "pending"
                };
                let mut card = v_flex()
                    .gap_1()
                    .p_2()
                    .border_1()
                    .border_color(theme::border())
                    .child(format!("{} · {} · {status}", task.key, task.title))
                    .child(task.instructions.clone())
                    .children(
                        task.acceptance
                            .iter()
                            .map(|criterion| div().child(format!("• {criterion}"))),
                    );
                if !task.depends_on.is_empty() {
                    card = card.child(format!("Depends on: {}", task.depends_on.join(", ")));
                }
                if let Some(result) = result {
                    card = card
                        .child(result.summary.clone())
                        .child("Reported verification")
                        .children(result.checks.iter().map(|check| div().child(check.clone())));
                }
                content = content.child(card);
            }
        }
        if let Some(worker) = &flow.worker {
            content = content
                .child(format!("Worktree: {}", worker.worktree))
                .child(format!("Branch: {}", worker.branch));
        }
        if flow.label() == "review results" {
            content = content.child("Implementation results are ready to review. Checks above are reported by the implementation session. Integration and milestone acceptance remain separate.");
        }
        content
            .child(self.workflow_button(
                "milestone-history",
                if self.show_history {
                    "Hide discussion"
                } else {
                    "Show discussion"
                },
                CommandId::ToggleBoardHistory,
                cx,
            ))
            .into_any_element()
    }
}
