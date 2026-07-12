use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use crossbeam_channel::{Receiver, Sender};
use horizon_session_protocol::Envelope;
use horizon_terminal_core::{
    compute_frame_diff, encode_terminal_control, encode_terminal_update, run_terminal_core,
    CoreReceivers, CoreSenders, SelectionCommand, TerminalAttachResult, TerminalCommand,
    TerminalControl, TerminalCoreOptions, TerminalFrame, TerminalSpawnSpec, TerminalSummary,
    TerminalUpdate,
};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct TerminalHost {
    sessions: Arc<Mutex<HashMap<Uuid, HostedTerminal>>>,
    connection: Arc<Mutex<Option<TerminalConnection>>>,
}

#[derive(Clone)]
struct HostedTerminal {
    command_tx: Sender<TerminalCommand>,
    latest_frame: Arc<Mutex<Option<TerminalFrame>>>,
    pid: Option<u32>,
    killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
}

struct TerminalConnection {
    outgoing: UnboundedSender<Envelope>,
    attached: HashSet<Uuid>,
    baselines: HashMap<Uuid, TerminalFrame>,
}

impl TerminalHost {
    pub(crate) fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            connection: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn connect(&self, outgoing: UnboundedSender<Envelope>) {
        *self.connection.lock().unwrap() = Some(TerminalConnection {
            outgoing,
            attached: HashSet::new(),
            baselines: HashMap::new(),
        });
    }

    pub(crate) fn disconnect(&self) {
        *self.connection.lock().unwrap() = None;
    }

    pub(crate) fn handle_control(&self, session_id: Option<Uuid>, control: TerminalControl) {
        match control {
            TerminalControl::List { request_id } => self.list(request_id),
            TerminalControl::Create(spec) => {
                if let Some(session_id) = session_id {
                    self.create(session_id, *spec);
                }
            }
            TerminalControl::Attach { request_id } => {
                if let Some(session_id) = session_id {
                    self.attach(session_id, request_id);
                }
            }
            TerminalControl::ListResult { .. } | TerminalControl::AttachResult { .. } => {}
        }
    }

    pub(crate) fn handle_command(&self, session_id: Uuid, command: TerminalCommand) {
        let sender = self
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .map(|session| session.command_tx.clone());
        if let Some(sender) = sender {
            let _ = sender.send(command);
        }
    }

    pub(crate) fn shutdown_all(&self) {
        let sessions = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .map(|session| (session.command_tx.clone(), session.killer.clone()))
            .collect::<Vec<_>>();
        for (sender, killer) in sessions {
            let _ = killer.lock().unwrap().kill();
            let _ = sender.send(TerminalCommand::Shutdown);
        }
    }

    fn create(&self, session_id: Uuid, spec: TerminalSpawnSpec) {
        if self.sessions.lock().unwrap().contains_key(&session_id) {
            return;
        }
        self.mark_attached(session_id);
        let cwd = self.resolve_cwd(&spec);
        match spawn_terminal(session_id, &spec, &cwd) {
            Ok((session, update_rx)) => {
                self.sessions.lock().unwrap().insert(session_id, session);
                self.forward_updates(session_id, update_rx);
            }
            Err(error) => {
                self.send_update(session_id, TerminalUpdate::Error(error.to_string()));
            }
        }
    }

    fn list(&self, request_id: Uuid) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap()
            .keys()
            .copied()
            .map(|session_id| TerminalSummary { session_id })
            .collect::<Vec<_>>();
        sessions.sort_unstable_by_key(|summary| summary.session_id);
        self.send_control(
            None,
            TerminalControl::ListResult {
                request_id,
                sessions,
            },
        );
    }

    fn attach(&self, session_id: Uuid, request_id: Uuid) {
        let sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.get(&session_id) else {
            drop(sessions);
            self.send_control(
                Some(session_id),
                TerminalControl::AttachResult {
                    request_id,
                    result: TerminalAttachResult::NotFound,
                },
            );
            return;
        };
        let latest = session.latest_frame.lock().unwrap();
        let mut connection = self.connection.lock().unwrap();
        let Some(connection) = connection.as_mut() else {
            return;
        };
        connection.attached.insert(session_id);
        connection.baselines.remove(&session_id);
        if let Ok(envelope) = encode_terminal_control(
            Some(session_id),
            &TerminalControl::AttachResult {
                request_id,
                result: TerminalAttachResult::Attached,
            },
        ) {
            let _ = connection.outgoing.send(envelope);
        }
        if let Some(frame) = latest.as_ref() {
            if let Ok(envelope) =
                encode_terminal_update(session_id, &TerminalUpdate::Snapshot(frame.clone()))
            {
                connection.baselines.insert(session_id, frame.clone());
                let _ = connection.outgoing.send(envelope);
            }
        }
    }

    fn resolve_cwd(&self, spec: &TerminalSpawnSpec) -> PathBuf {
        spec.spawn_source_session_id
            .and_then(|source| {
                self.sessions
                    .lock()
                    .unwrap()
                    .get(&source)
                    .and_then(|session| session.pid)
            })
            .and_then(sample_cwd)
            .unwrap_or_else(|| spec.fallback_cwd.clone())
    }

    fn mark_attached(&self, session_id: Uuid) {
        if let Some(connection) = self.connection.lock().unwrap().as_mut() {
            connection.attached.insert(session_id);
            connection.baselines.remove(&session_id);
        }
    }

    fn send_update(&self, session_id: Uuid, update: TerminalUpdate) {
        if let Ok(envelope) = encode_terminal_update(session_id, &update) {
            if let Some(connection) = self.connection.lock().unwrap().as_ref() {
                if connection.attached.contains(&session_id) {
                    let _ = connection.outgoing.send(envelope);
                }
            }
        }
    }

    fn send_control(&self, session_id: Option<Uuid>, control: TerminalControl) {
        if let Ok(envelope) = encode_terminal_control(session_id, &control) {
            if let Some(connection) = self.connection.lock().unwrap().as_ref() {
                let _ = connection.outgoing.send(envelope);
            }
        }
    }

    fn forward_updates(&self, session_id: Uuid, update_rx: Receiver<TerminalUpdate>) {
        let host = self.clone();
        thread::spawn(move || {
            while let Ok(update) = update_rx.recv() {
                match update {
                    TerminalUpdate::Snapshot(frame) => {
                        if let Some(session) = host.sessions.lock().unwrap().get(&session_id) {
                            *session.latest_frame.lock().unwrap() = Some(frame.clone());
                        }
                        let mut connection = host.connection.lock().unwrap();
                        let Some(connection) = connection.as_mut() else {
                            continue;
                        };
                        if !connection.attached.contains(&session_id) {
                            continue;
                        }
                        let update = match connection.baselines.get(&session_id) {
                            Some(old) => TerminalUpdate::FrameDiff(compute_frame_diff(old, &frame)),
                            None => TerminalUpdate::Snapshot(frame.clone()),
                        };
                        if let Ok(envelope) = encode_terminal_update(session_id, &update) {
                            connection.baselines.insert(session_id, frame);
                            let _ = connection.outgoing.send(envelope);
                        }
                    }
                    TerminalUpdate::FrameDiff(_) => {}
                    TerminalUpdate::Exited => {
                        host.send_update(session_id, TerminalUpdate::Exited);
                        host.sessions.lock().unwrap().remove(&session_id);
                        if let Some(connection) = host.connection.lock().unwrap().as_mut() {
                            connection.attached.remove(&session_id);
                            connection.baselines.remove(&session_id);
                        }
                        return;
                    }
                    other => host.send_update(session_id, other),
                }
            }
        });
    }
}

fn spawn_terminal(
    session_id: Uuid,
    spec: &TerminalSpawnSpec,
    cwd: &Path,
) -> anyhow::Result<(HostedTerminal, Receiver<TerminalUpdate>)> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: spec.initial_size.rows,
        cols: spec.initial_size.cols,
        pixel_width: spec.initial_size.pixel_width,
        pixel_height: spec.initial_size.pixel_height,
    })?;

    let mut command = CommandBuilder::new(&spec.shell);
    command.args(&spec.args);
    command.env("TERM", &spec.term);
    command.env("HORIZON_SOCKET", &spec.control_socket);
    command.env("HORIZON_SESSION_ID", session_id.to_string());
    command.cwd(cwd);
    let child = pair.slave.spawn_command(command)?;
    let pid = child.process_id();
    let killer = Arc::new(Mutex::new(child.clone_killer()));
    drop(child);
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let master = pair.master;

    let (command_tx, command_rx) = crossbeam_channel::unbounded();
    let (update_tx, update_rx) = crossbeam_channel::unbounded();
    let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
    let (resize_tx, resize_rx) = crossbeam_channel::unbounded();
    let (scroll_tx, scroll_rx) = crossbeam_channel::unbounded();
    let (mouse_tx, mouse_rx) = crossbeam_channel::unbounded();
    let (paste_tx, paste_rx) = crossbeam_channel::unbounded();
    let (key_tx, key_rx) = crossbeam_channel::unbounded();
    let (selection_tx, selection_rx) = crossbeam_channel::unbounded();
    let (focus_tx, focus_rx) = crossbeam_channel::unbounded();

    let response_tx = command_tx.clone();
    let read_update_tx = update_tx.clone();
    thread::spawn(move || read_pty(&mut *reader, pty_tx, read_update_tx));
    let size = spec.initial_size;
    let options = TerminalCoreOptions {
        scrollback_lines: spec.scrollback_lines,
        color_scheme: spec.color_scheme,
    };
    thread::spawn(move || {
        run_terminal_core(
            size,
            options,
            pty_rx,
            CoreReceivers {
                resize_rx,
                scroll_rx,
                mouse_rx,
                paste_rx,
                key_rx,
                selection_rx,
                focus_rx,
            },
            response_tx,
            update_tx,
        );
    });
    let writer_killer = killer.clone();
    thread::spawn(move || {
        run_writer(
            master,
            &mut *writer,
            writer_killer,
            command_rx,
            CoreSenders {
                resize_tx,
                scroll_tx,
                mouse_tx,
                paste_tx,
                key_tx,
                selection_tx,
                focus_tx,
            },
        );
    });

    Ok((
        HostedTerminal {
            command_tx,
            latest_frame: Arc::new(Mutex::new(None)),
            pid,
            killer,
        },
        update_rx,
    ))
}

fn read_pty(reader: &mut dyn Read, pty_tx: Sender<Vec<u8>>, update_tx: Sender<TerminalUpdate>) {
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                let _ = update_tx.send(TerminalUpdate::Exited);
                return;
            }
            Ok(read) => {
                if pty_tx.send(buffer[..read].to_vec()).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = update_tx.send(TerminalUpdate::Error(error.to_string()));
                let _ = update_tx.send(TerminalUpdate::Exited);
                return;
            }
        }
    }
}

fn run_writer(
    master: Box<dyn MasterPty + Send>,
    writer: &mut dyn Write,
    killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
    command_rx: Receiver<TerminalCommand>,
    senders: CoreSenders,
) {
    let CoreSenders {
        resize_tx,
        scroll_tx,
        mouse_tx,
        paste_tx,
        key_tx,
        selection_tx,
        focus_tx,
    } = senders;
    let mut last_size = None;
    while let Ok(command) = command_rx.recv() {
        match command {
            TerminalCommand::Input(bytes) => {
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
            TerminalCommand::Key {
                key,
                modifiers,
                event,
            } => {
                let _ = key_tx.send((key, modifiers, event));
            }
            TerminalCommand::Paste(text) => {
                let _ = paste_tx.send(text);
            }
            TerminalCommand::Resize(size) => {
                if last_size != Some(size) {
                    last_size = Some(size);
                    let _ = master.resize(PtySize {
                        rows: size.rows,
                        cols: size.cols,
                        pixel_width: size.pixel_width,
                        pixel_height: size.pixel_height,
                    });
                }
                let _ = resize_tx.send(size);
            }
            TerminalCommand::Scroll(scroll) => {
                let _ = scroll_tx.send(scroll);
            }
            TerminalCommand::Mouse(report) => {
                let _ = mouse_tx.send(report);
            }
            TerminalCommand::SelectionStart(point) => {
                let _ = selection_tx.send(SelectionCommand::Start(point));
            }
            TerminalCommand::SelectionUpdate(point) => {
                let _ = selection_tx.send(SelectionCommand::Update(point));
            }
            TerminalCommand::CopySelection => {
                let _ = selection_tx.send(SelectionCommand::Copy);
            }
            TerminalCommand::Focus(focused) => {
                let _ = focus_tx.send(focused);
            }
            TerminalCommand::Shutdown => {
                let _ = killer.lock().unwrap().kill();
                return;
            }
        }
    }
}

fn sample_cwd(pid: u32) -> Option<PathBuf> {
    let sysinfo_pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[sysinfo_pid]),
        false,
        ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always),
    );
    system
        .process(sysinfo_pid)
        .and_then(|process| process.cwd())
        .map(Path::to_path_buf)
}
