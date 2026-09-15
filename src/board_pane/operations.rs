use super::*;

impl BoardPaneView {
    pub(crate) fn finish_inventory_refresh(&mut self, sessions: &[horizon_workspace::SessionId]) {
        for id in sessions {
            self.inventory_pending.remove(id);
        }
    }

    pub(crate) fn navigation_epoch(&self) -> u64 {
        self.navigation_epoch
    }

    pub(crate) fn root(&self) -> Option<PathBuf> {
        self.root.clone()
    }

    pub(crate) fn set_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    pub(super) fn open_item_id(&self) -> Option<u64> {
        match &self.mode {
            BoardPaneMode::List => None,
            BoardPaneMode::Detail { item, .. } => Some(item.id),
        }
    }

    pub(crate) fn task_session(&self) -> Option<horizon_workspace::SessionId> {
        let BoardPaneMode::Detail { item, .. } = &self.mode else {
            return None;
        };
        uuid::Uuid::parse_str(item.session_id.as_deref()?)
            .ok()
            .map(horizon_workspace::SessionId::from_uuid)
    }

    pub(crate) fn board_command(
        &mut self,
        command: CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            CommandId::OpenBoardRelatedItem => {
                if let Some(id) = self.navigation_item.take() {
                    let item = self
                        .list
                        .read(cx)
                        .delegate()
                        .all
                        .iter()
                        .find(|item| item.id == id)
                        .cloned();
                    if let Some(item) = item {
                        self.open_detail(item, window, cx);
                    }
                }
            }
            CommandId::BackBoardList => self.back_to_list(window, cx),
            CommandId::AddBoardTask => self.add_item(window, cx),
            CommandId::PostBoardMessage => self.post_comment(window, cx),
            CommandId::ReorderBoardTask => {
                if let Some((id, position)) = self.pending_move.take() {
                    self.spawn_move(id, position, cx);
                }
            }
            CommandId::MoveBoardTaskUp | CommandId::MoveBoardTaskDown => {
                let delegate = self.list.read(cx).delegate();
                let selected = self
                    .navigation_item
                    .take()
                    .or(self.open_item_id())
                    .or_else(|| {
                        delegate
                            .selected
                            .and_then(|index| delegate.item_at(index).map(|item| item.id))
                    });
                if let Some(id) = selected {
                    if let Some(position) =
                        sibling_move(&delegate.all, id, command == CommandId::MoveBoardTaskUp)
                    {
                        self.spawn_move(id, position, cx);
                    }
                }
            }
            CommandId::SaveBoardState => {
                if let Some(id) = self.open_item_id() {
                    self.spawn_set_status(id, self.state_input.read(cx).value().to_string(), cx);
                }
            }
            CommandId::ToggleBoardCompleted
            | CommandId::AddBoardDependency
            | CommandId::RemoveBoardDependency => {
                let BoardPaneMode::Detail { item, .. } = &self.mode else {
                    return;
                };
                let id = item.id;
                let completed = !item.completed;
                let mut dependencies = item.depends_on.clone();
                if command != CommandId::ToggleBoardCompleted {
                    let Some(dependency) = self.pending_dependency.take() else {
                        return;
                    };
                    if command == CommandId::AddBoardDependency {
                        dependencies.push(dependency);
                    } else {
                        dependencies.retain(|value| *value != dependency);
                    }
                }
                self.mutate(cx, move |store| {
                    Box::pin(async move {
                        if command == CommandId::ToggleBoardCompleted {
                            store.set_completed(id, completed).await
                        } else {
                            store.set_dependencies(id, dependencies).await
                        }
                    })
                });
            }
            _ => {}
        }
    }

    pub(super) fn mutate<F>(&self, cx: &mut Context<Self>, operation: F)
    where
        F: FnOnce(Store) -> Pin<Box<dyn Future<Output = Result<(), StoreError>> + Send>>
            + Send
            + 'static,
    {
        let Some(root) = self.root.clone() else {
            return;
        };
        let epoch = self.navigation_epoch;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(StoreError::Io)?;
                    runtime.block_on(operation(Store::from_dir(&root)?))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.spawn_load(cx);
                if epoch != view.navigation_epoch {
                    return;
                }
                match result {
                    Ok(()) => {
                        if let Some(id) = view.open_item_id() {
                            view.spawn_show(id, cx);
                        }
                    }
                    Err(error) => view.set_error(error.to_string(), cx),
                }
            });
        })
        .detach();
    }

    pub(super) fn mark_displayed(
        &mut self,
        id: u64,
        message: String,
        epoch: u64,
        cx: &mut Context<Self>,
    ) {
        if epoch != self.navigation_epoch || self.open_item_id() != Some(id) {
            return;
        }
        let BoardPaneMode::Detail { item, .. } = &self.mode else {
            return;
        };
        if !newly_displayed(item, self.read_messages.get(&id), &message) {
            return;
        }
        self.read_messages
            .entry(id)
            .or_default()
            .insert(message.clone());
        let unread = unread_tasks(&self.list.read(cx).delegate().all, &self.read_messages);
        self.list.update(cx, |list, cx| {
            list.delegate_mut().unread = unread;
            cx.notify();
        });
        self.mutate(cx, move |store| {
            Box::pin(async move { store.mark_read(id, "owner", &message).await })
        });
    }
}
