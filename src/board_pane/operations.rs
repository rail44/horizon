use super::*;

/// What the shell asks the pane about. None of it is reachable from a
/// preview, which has no shell above it.
#[cfg(not(target_family = "wasm"))]
impl BoardPaneView {
    pub(crate) fn finish_inventory_refresh(&mut self, sessions: &[horizon_workspace::SessionId]) {
        for id in sessions {
            self.inventory_pending.remove(id);
        }
    }

    pub(crate) fn navigation_epoch(&self) -> u64 {
        self.navigation_epoch
    }

    /// The project directory the shell watches for this pane. Only a
    /// root-resolved store has one.
    pub(crate) fn root(&self) -> Option<PathBuf> {
        self.store
            .as_ref()
            .and_then(BoardStoreSource::root)
            .map(std::path::Path::to_path_buf)
    }

    pub(crate) fn task_session(&self) -> Option<horizon_workspace::SessionId> {
        let BoardPaneMode::Detail { item, .. } = &self.mode else {
            return None;
        };
        uuid::Uuid::parse_str(item.session_id.as_deref()?)
            .ok()
            .map(horizon_workspace::SessionId::from_uuid)
    }
}

impl BoardPaneView {
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
            CommandId::ToggleBoardClosed
            | CommandId::AddBoardDependency
            | CommandId::RemoveBoardDependency => {
                let BoardPaneMode::Detail { item, .. } = &self.mode else {
                    return;
                };
                let id = item.id;
                let is_closed = !item.is_closed;
                let mut dependencies = item.depends_on.clone();
                if command != CommandId::ToggleBoardClosed {
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
                        if command == CommandId::ToggleBoardClosed {
                            store.set_closed(id, is_closed, None).await
                        } else {
                            store.set_dependencies(id, dependencies).await
                        }
                    })
                });
            }
            _ => {}
        }
    }

    /// Runs one write against the pane's store, then reloads what is showing.
    /// A store that accepts no writes fails the operation immediately and the
    /// refusal lands in the pane's error line like any other store error.
    pub(super) fn mutate<F>(&self, cx: &mut Context<Self>, operation: F)
    where
        F: FnOnce(Store) -> StoreJob<()> + Send + 'static,
    {
        let Some(source) = self.store.clone() else {
            return;
        };
        let epoch = self.navigation_epoch;
        cx.spawn(async move |this, cx| {
            let result = run_store_job(cx, source, operation).await;
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
        if !horizon_board::read_position_advances(
            item,
            self.read_positions.get(&id).map(String::as_str),
            &message,
        ) {
            return;
        }
        self.read_positions.insert(id, message.clone());
        let unread = unread_tasks(&self.list.read(cx).delegate().all, &self.read_positions);
        self.list.update(cx, |list, cx| {
            list.delegate_mut().unread = unread;
            cx.notify();
        });
        self.mutate(cx, move |store| {
            Box::pin(async move { store.mark_read(id, "owner", &message).await })
        });
    }
}
