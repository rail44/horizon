use super::*;
use horizon_board::workflow::{Decision, Mutation, Workflow};

impl BoardPaneView {
    fn displayed_decision<'a>(&self, flow: &'a Workflow) -> Option<&'a Decision> {
        self.selected_decision
            .as_ref()
            .and_then(|key| {
                flow.plan
                    .as_ref()?
                    .decisions
                    .iter()
                    .find(|d| &d.key == key && !d.retired)
            })
            .or_else(|| flow.unanswered().first().copied())
    }
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
            .or_else(|| {
                flow.verifier
                    .as_ref()
                    .or(flow.worker.as_ref())
                    .map(|w| w.session.as_str())
            })?;
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
        if command == CommandId::OpenBoardRelatedItem {
            if let Some(id) = self.navigation_item.take() {
                let item = self
                    .list
                    .read(cx)
                    .delegate()
                    .all
                    .iter()
                    .find(|i| i.id == id)
                    .cloned();
                if let Some(item) = item {
                    self.open_detail(item, window, cx);
                }
            }
            return;
        }
        if command == CommandId::SelectBoardDecision {
            self.selected_decision = self.navigation_decision.take();
            self.decision_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            window.focus(&self.decision_input.read(cx).focus_handle(cx), cx);
            cx.notify();
            return;
        }
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
                    .and_then(|f| self.displayed_decision(f))
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

    fn related_item(&self, id: u64, label: String, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(SharedString::from(format!("board-item-{id}-{label}")))
            .p_2()
            .border_1()
            .border_color(theme::border())
            .child(label)
            .on_click(cx.listener(move |view, _, _, cx| {
                view.navigation_item = Some(id);
                cx.emit(BoardCommand(CommandId::OpenBoardRelatedItem));
            }))
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
                    "Plan, run and integrate into main",
                    CommandId::EnableBoardMilestone,
                    cx,
                ))
                .into_any_element();
        };
        content = content.child(div().text_size(px(14.0)).child(format!(
            "{} · {}",
            if flow.is_milestone() {
                "Milestone"
            } else {
                "Task"
            },
            flow.label()
        )));
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
        if let Some(parent) = item.parent {
            content =
                content.child(self.related_item(parent, format!("Open milestone #{parent}"), cx));
        }
        if let Some(plan) = &flow.plan {
            let mut choices = h_flex().gap_2().flex_wrap();
            for decision in plan
                .decisions
                .iter()
                .filter(|d| !d.retired && (d.resolution.is_none() || self.show_history))
            {
                let key = decision.key.clone();
                choices = choices.child(
                    div()
                        .id(SharedString::from(format!("decision-{key}")))
                        .p_2()
                        .border_1()
                        .border_color(theme::border())
                        .child(format!(
                            "{}: {}",
                            if decision.resolution.is_some() {
                                "Decided"
                            } else {
                                "Decision"
                            },
                            decision.question
                        ))
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.navigation_decision = Some(key.clone());
                            cx.emit(BoardCommand(CommandId::SelectBoardDecision));
                        })),
                );
            }
            content = content.child(choices);
        }
        if let Some(decision) = self.displayed_decision(flow) {
            let mut discussion = v_flex()
                .gap_2()
                .p_3()
                .border_1()
                .border_color(theme::border())
                .child(div().text_size(px(14.0)).child(decision.question.clone()))
                .child(decision.context.clone())
                .child(format!("Recommendation: {}", decision.recommendation))
                .child(format!("Effect: {}", decision.consequence));
            for id in &decision.affected_tasks {
                discussion =
                    discussion.child(self.related_item(*id, format!("Affected task #{id}"), cx));
            }
            if let Some(resolution) = &decision.resolution {
                discussion = discussion.child(format!("Decision: {resolution}"));
            }
            let messages = if self.show_history {
                decision.messages.as_slice()
            } else {
                &decision.messages[decision.messages.len().saturating_sub(2)..]
            };
            for message in messages {
                discussion = discussion.child(format!(
                    "{}: {}",
                    if message.owner { "You" } else { "AI" },
                    message.text
                ));
            }
            discussion =
                discussion
                    .child(Input::new(&self.decision_input))
                    .child(self.workflow_button(
                        "milestone-answer",
                        "Send message",
                        CommandId::SubmitBoardDecision,
                        cx,
                    ));
            content = content.child(discussion);
        }
        if let Some(plan) = &flow.plan {
            content = content
                .child(div().text_size(px(14.0)).child("Current plan"))
                .child(plan.summary.clone())
                .child(
                    v_flex().gap_1().children(
                        plan.acceptance
                            .iter()
                            .map(|c| div().child(format!("• {c}"))),
                    ),
                );
            let items = self.list.read(cx).delegate().all.clone();
            let running = plan
                .tasks
                .iter()
                .filter(|id| {
                    items
                        .iter()
                        .find(|i| i.id == **id)
                        .and_then(|i| i.workflow.as_ref())
                        .is_some_and(|w| w.active.is_some())
                })
                .count();
            let integrated = plan
                .tasks
                .iter()
                .filter(|id| {
                    items
                        .iter()
                        .find(|i| i.id == **id)
                        .and_then(|i| i.workflow.as_ref())
                        .is_some_and(|w| w.integrated.is_some())
                })
                .count();
            content = content.child(format!(
                "{running} running · {integrated}/{} integrated · {} decisions pending",
                plan.tasks.len(),
                flow.unanswered().len()
            ));
            for id in &plan.tasks {
                let label = items
                    .iter()
                    .find(|i| i.id == *id)
                    .map(|i| {
                        format!(
                            "#{} · {} · {}",
                            i.id,
                            i.title,
                            i.workflow.as_ref().map_or(i.status.as_str(), |w| w.label())
                        )
                    })
                    .unwrap_or_else(|| format!("Task #{id}"));
                content = content.child(self.related_item(*id, label, cx));
            }
        }
        if let Some(task) = &flow.task {
            content = content.child(
                v_flex().gap_1().children(
                    task.acceptance
                        .iter()
                        .map(|c| div().child(format!("• {c}"))),
                ),
            );
            for id in &item.depends_on {
                content = content.child(self.related_item(*id, format!("Dependency #{id}"), cx));
            }
            if self.show_history {
                content = content
                    .child(format!("Source scope: {}", task.scope.paths.join(", ")))
                    .child(format!(
                        "Functional scope: {}",
                        task.scope.functions.join(", ")
                    ));
            }
        }
        if let Some(result) = &flow.result {
            content = content.child(result.summary.clone());
        }
        if let Some(v) = &flow.verification {
            content = content.child(v.summary.clone());
            for e in &v.evidence {
                content = content.child(format!(
                    "{} · {}: {}",
                    if e.satisfied {
                        "Verified"
                    } else {
                        "Unverified"
                    },
                    e.criterion,
                    e.detail
                ));
            }
        }
        if let Some(commit) = &flow.integrated {
            content = content.child(format!("Integrated into main: {commit}"));
        }
        if self.show_history {
            for entry in &flow.history {
                content = content.child(entry.clone());
            }
            if let Some(result) = &flow.result {
                content = content.children(result.checks.iter().map(|c| div().child(c.clone())));
            }
        }
        if let Some(worker) = &flow.worker {
            content = content
                .child(format!("Worktree: {}", worker.worktree))
                .child(format!("Branch: {}", worker.branch));
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
