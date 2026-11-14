//! The board's two views as plugins: the commands each carries out for
//! the pane it is in, and the state a pane of each keeps.

use horizon_board_view::model::{ListCommand, ThreadCommand};
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked};
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{Callable, Callback};
use horizon_workspace::SessionId;
use serde::{Deserialize, Serialize};

use super::{Activity, Sessions, Source, list, thread};
use crate::background::Background;
use crate::plugin::{Command, Mounted, Plugin};

/// The sessions a view's tasks are bound to, said through `ask` whenever
/// they change.
fn sessions<A: 'static>(
    activity: Activity,
    ask: Callback<A>,
    told: fn(Vec<SessionId>) -> A,
) -> Sessions {
    let bound = RwSignal::new(Vec::new());
    Effect::new(move |_| ask.run(told(bound.get())));
    Sessions {
        activity,
        bound: bound.write_only(),
    }
}

/// The task list.
pub struct BoardListPlugin {
    pub source: Source,
    pub background: Background,
    pub activity: Activity,
}

/// What the list asks of whoever shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum ListAsked {
    View(list::Asked),
    /// The sessions its tasks are bound to.
    Bound(Vec<SessionId>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoardListCommand {
    ToggleExpansion,
    ToggleFinished,
    OpenThread,
    AddTask,
    MoveUp,
    MoveDown,
    Reorder,
    OpenOrganizer,
}

impl Plugin for BoardListPlugin {
    const NAME: &'static str = "board-list";
    const TITLE: &'static str = "Board";
    type State = ();
    type Command = BoardListCommand;
    type Asked = ListAsked;

    fn commands() -> Vec<Command<BoardListCommand>> {
        use BoardListCommand::*;
        let command = |name, title, description, command| Command {
            name,
            title,
            description,
            destructive: false,
            command,
        };
        vec![
            command(
                "toggle-board-expansion",
                "Toggle Board Expansion",
                "Fold the selected task's children away, or open them again.",
                ToggleExpansion,
            ),
            command(
                "toggle-board-finished-band",
                "Show or Hide Finished Tasks",
                "Fold the board list's finished band open or shut.",
                ToggleFinished,
            ),
            command(
                "open-board-related-item",
                "Open Board Task",
                "Open the selected task's thread beside the board list.",
                OpenThread,
            ),
            command(
                "add-board-task",
                "Add Board Task",
                "Create a task from the name entered in the board.",
                AddTask,
            ),
            command(
                "move-board-task-up",
                "Move Board Task Up",
                "Move this task before its previous sibling.",
                MoveUp,
            ),
            command(
                "move-board-task-down",
                "Move Board Task Down",
                "Move this task after its next sibling.",
                MoveDown,
            ),
            command(
                "reorder-board-task",
                "Reorder Board Task",
                "Apply the dragged task position among its siblings.",
                Reorder,
            ),
            command(
                "open-board-organizer",
                "Open Board Organizer",
                "Create or resume this board’s organizer and open its agent view.",
                OpenOrganizer,
            ),
        ]
    }

    fn mount(self, _: (), ask: Callback<ListAsked>) -> Mounted<(), BoardListCommand> {
        let sessions = sessions(self.activity, ask, ListAsked::Bound);
        let (view, list) = list::view(self.source, self.background, sessions, move |asked| {
            ask.run(ListAsked::View(asked))
        });
        let run = Callback::new(move |command| {
            let asked = match command {
                BoardListCommand::ToggleExpansion => {
                    let Some(task) = list.selected.get_untracked() else {
                        return;
                    };
                    ListCommand::ToggleExpansion(task)
                }
                BoardListCommand::ToggleFinished => ListCommand::ToggleFinished,
                BoardListCommand::OpenThread => ListCommand::OpenThread,
                BoardListCommand::AddTask => ListCommand::AddTask,
                BoardListCommand::MoveUp => ListCommand::MoveUp,
                BoardListCommand::MoveDown => ListCommand::MoveDown,
                // (A drop carries its own move out where it lands.)
                BoardListCommand::Reorder => ListCommand::Reorder,
                BoardListCommand::OpenOrganizer => return list.open_organizer(),
            };
            list.execute(asked);
        });
        Mounted {
            view,
            run,
            enabled: None,
            state: Signal::stored_local(()),
        }
    }
}

/// One task's thread.
pub struct BoardThreadPlugin {
    pub source: Source,
    pub background: Background,
    pub activity: Activity,
}

/// What a thread pane keeps: the task it is on.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThreadState {
    pub task: u64,
}

/// What the thread asks of whoever shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum ThreadAsked {
    View(thread::Asked),
    /// The sessions its task is bound to.
    Bound(Vec<SessionId>),
    /// The list in the thread's tab, to go back to.
    BackToList,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoardThreadCommand {
    OpenTaskSession,
    BackToList,
    PostMessage,
    SaveStatus,
    ToggleClosed,
    OpenOrganizer,
}

impl Plugin for BoardThreadPlugin {
    const NAME: &'static str = "board-thread";
    const TITLE: &'static str = "Thread";
    type State = ThreadState;
    type Command = BoardThreadCommand;
    type Asked = ThreadAsked;

    fn commands() -> Vec<Command<BoardThreadCommand>> {
        use BoardThreadCommand::*;
        let command = |name, title, description, command| Command {
            name,
            title,
            description,
            destructive: false,
            command,
        };
        vec![
            command(
                "open-board-task-session",
                "Open Task Working History",
                "Open the ordinary agent session associated with this task.",
                OpenTaskSession,
            ),
            command(
                "back-board-list",
                "Back to Task List",
                "Move focus from this thread back to the board list beside it.",
                BackToList,
            ),
            command(
                "post-board-message",
                "Send Board Message",
                "Send the consultation input to this task session.",
                PostMessage,
            ),
            command(
                "save-board-state",
                "Save Task State",
                "Save the project-defined state entered for this task.",
                SaveStatus,
            ),
            command(
                "toggle-board-closed",
                "Close or Reopen Task",
                "Close or reopen the task independently of its project-defined state.",
                ToggleClosed,
            ),
            command(
                "open-board-organizer",
                "Open Board Organizer",
                "Create or resume this board’s organizer and open its agent view.",
                OpenOrganizer,
            ),
        ]
    }

    fn mount(
        self,
        state: ThreadState,
        ask: Callback<ThreadAsked>,
    ) -> Mounted<ThreadState, BoardThreadCommand> {
        let sessions = sessions(self.activity, ask, ThreadAsked::Bound);
        let (view, shown) = thread::view(
            self.source,
            self.background,
            Signal::stored(state.task),
            sessions,
            move |asked| ask.run(ThreadAsked::View(asked)),
        );
        let run = Callback::new(move |command| {
            shown.execute(match command {
                BoardThreadCommand::OpenTaskSession => ThreadCommand::OpenTaskSession,
                BoardThreadCommand::PostMessage => ThreadCommand::PostMessage,
                BoardThreadCommand::SaveStatus => ThreadCommand::SaveStatus,
                BoardThreadCommand::ToggleClosed => ThreadCommand::ToggleClosed,
                BoardThreadCommand::BackToList => return ask.run(ThreadAsked::BackToList),
                BoardThreadCommand::OpenOrganizer => return shown.open_organizer(),
            });
        });
        Mounted {
            view,
            run,
            enabled: None,
            state: Signal::stored_local(state),
        }
    }
}
