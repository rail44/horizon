use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};

#[derive(Clone, Debug, Default)]
pub(crate) struct TerminalEvents {
    pub(crate) pty_writes: Vec<Vec<u8>>,
    pub(crate) title: Option<String>,
    pub(crate) bell_count: usize,
}

#[derive(Clone, Debug, Default)]
pub(super) struct EventSink {
    events: Arc<Mutex<TerminalEvents>>,
}

impl EventSink {
    pub(super) fn drain(&self) -> TerminalEvents {
        std::mem::take(&mut *self.events.lock().expect("terminal event mutex poisoned"))
    }
}

impl EventListener for EventSink {
    fn send_event(&self, event: Event) {
        let mut events = self.events.lock().expect("terminal event mutex poisoned");
        match event {
            Event::PtyWrite(text) => events.pty_writes.push(text.into_bytes()),
            Event::Title(title) => events.title = Some(title),
            Event::ResetTitle => events.title = None,
            Event::Bell => events.bell_count += 1,
            _ => {}
        }
    }
}
