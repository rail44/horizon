//! `horizon-agent-probe`: runs the agent in a `horizon-agent-acp` of its
//! own and prints what it does, so a change to the agent can be seen
//! without restarting the running Horizon and without a window.
//!
//! The agent is the `horizon-agent-acp` beside this binary (the same build),
//! started as this process's child with its session logs and projection
//! under a data home of its own; it reads the user's `config.toml` like any
//! other. One session is started in `--dir`, each prompt is sent once the
//! previous turn ends, and every message, tool call and approval of the
//! session is printed as one JSON line. The session is ended and the child
//! stopped at the end: a session that entered a worktree leaves the
//! worktree behind only when it holds changes, as a terminated session does.
//!
//! ```text
//! horizon-agent-probe [--dir DIR] [--home DIR] [--approve | --deny REASON]
//!                     [--timeout SECS] PROMPT...
//! ```
//!
//! An approval is answered by the probe: denied by default, with a reason
//! the model reads, or approved with `--approve` -- which runs what the
//! agent asked for on this machine. `--home` keeps the agent's files (its
//! session logs are in `<home>/data/horizon/sessions`, its diagnostics in
//! `<home>/agent.log`) where given, otherwise in a fresh directory under the
//! temp directory, printed first.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use horizon_shell_runtime::agent_model::{
    AgentEvent, AgentFrameItem, AgentModel, Permission, ToolCall,
};
use horizon_shell_runtime::{AgentAcpHandle, AgentCommand, AgentUpdate, AttachmentState};
use horizon_wire::SessionId;
use serde_json::{Value, json};

const USAGE: &str = "usage: horizon-agent-probe [--dir DIR] [--home DIR] \
                     [--approve | --deny REASON] [--timeout SECS] PROMPT...";

/// How the probe answers an approval.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
    Approve,
    Deny(String),
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    dir: PathBuf,
    home: Option<PathBuf>,
    answer: Answer,
    timeout: Duration,
    prompts: Vec<String>,
}

fn parse(args: impl IntoIterator<Item = String>, cwd: PathBuf) -> Result<Args, String> {
    let mut parsed = Args {
        dir: cwd,
        home: None,
        answer: Answer::Deny("The probe running this session denies every approval.".into()),
        timeout: Duration::from_secs(600),
        prompts: Vec::new(),
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--dir" => parsed.dir = PathBuf::from(value("--dir")?),
            "--home" => parsed.home = Some(PathBuf::from(value("--home")?)),
            "--approve" => parsed.answer = Answer::Approve,
            "--deny" => parsed.answer = Answer::Deny(value("--deny")?),
            "--timeout" => {
                let secs = value("--timeout")?;
                parsed.timeout = Duration::from_secs(
                    secs.parse()
                        .map_err(|_| format!("--timeout takes seconds, not {secs:?}"))?,
                );
            }
            "-h" | "--help" => return Err(USAGE.into()),
            flag if flag.starts_with("--") => return Err(format!("unknown flag {flag}")),
            prompt => parsed.prompts.push(prompt.to_string()),
        }
    }
    if parsed.prompts.is_empty() {
        return Err(USAGE.into());
    }
    Ok(parsed)
}

fn main() -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let args = match parse(std::env::args().skip(1), cwd) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            print(json!({ "failed": message }));
            ExitCode::FAILURE
        }
    }
}

fn print(line: Value) {
    println!("{line}");
}

fn run(args: Args) -> Result<(), String> {
    let home = match &args.home {
        Some(home) => home.clone(),
        None => std::env::temp_dir()
            .join("horizon-agent-probe")
            .join(uuid::Uuid::new_v4().to_string()),
    };
    let data = home.join("data");
    std::fs::create_dir_all(&data).map_err(|error| format!("{}: {error}", data.display()))?;
    let dir = args
        .dir
        .canonicalize()
        .map_err(|error| format!("{}: {error}", args.dir.display()))?;
    // The agent this starts inherits this process's environment: its
    // session logs and projection go under `data`, not the user's own.
    // SAFETY: set before any thread of this process exists.
    unsafe { std::env::set_var("XDG_DATA_HOME", &data) };
    let control = home.join("control.sock");
    print(json!({
        "home": home,
        "dir": dir,
        "sessions": data.join("horizon/sessions"),
    }));

    let (agent_acp, _roots) = AgentAcpHandle::start(
        horizon_shell_runtime::AgentProgram::horizon(),
        &control,
        &home.join("agent.log"),
    );
    let outcome = drive(&agent_acp, &dir, &args);
    agent_acp.stop_and_wait();
    outcome
}

fn drive(agent_acp: &AgentAcpHandle, dir: &Path, args: &Args) -> Result<(), String> {
    let session_id = SessionId::new();
    let mut session = agent_acp.start_session(session_id, Some(dir.to_path_buf()));
    let commands = session.sender();
    let mut updates = session.take_events();
    let mut model = AgentModel::default();
    let mut printed = 0;

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match receive(&mut updates, deadline)? {
            AgentUpdate::State(AttachmentState::Ready) => break,
            AgentUpdate::State(
                AttachmentState::Failed(why) | AttachmentState::Disconnected(why),
            ) => {
                return Err(format!("the session did not open: {why}"));
            }
            AgentUpdate::State(_) => {}
            AgentUpdate::Event(event) => model.apply(*event),
        }
    }
    printed = model.frame.items.len().max(printed);

    for prompt in &args.prompts {
        print(json!({ "prompt": prompt }));
        commands
            .send(AgentCommand::Prompt {
                text: prompt.clone(),
            })
            .map_err(|_| "the session stopped taking commands".to_string())?;
        let deadline = Instant::now() + args.timeout;
        loop {
            match receive(&mut updates, deadline)? {
                AgentUpdate::State(AttachmentState::Failed(why))
                | AgentUpdate::State(AttachmentState::Disconnected(why)) => {
                    return Err(format!("the session ended: {why}"));
                }
                AgentUpdate::State(_) => {}
                AgentUpdate::Event(event) => {
                    if let AgentEvent::PermissionRequested(permission) = &*event {
                        answer(permission, &args.answer, &commands)?;
                    }
                    model.apply(*event);
                }
            }
            let ended = model.frame.items[printed.min(model.frame.items.len())..]
                .iter()
                .any(|item| matches!(item, AgentFrameItem::TurnEnded { .. }));
            if ended {
                for item in &model.frame.items[printed..] {
                    if let Some(line) = describe(item) {
                        print(line);
                    }
                }
                printed = model.frame.items.len();
                break;
            }
        }
    }
    // Ending the session is what removes its worktree (when clean) and its
    // checkpoint refs from the repository; wait for it before the daemon is
    // drained.
    let _ = commands.send(AgentCommand::Close);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match receive(&mut updates, deadline) {
            Ok(AgentUpdate::State(AttachmentState::Disconnected(why))) => {
                print(json!({ "closed": why }));
                break;
            }
            Ok(AgentUpdate::State(AttachmentState::Failed(why))) | Err(why) => {
                print(json!({ "close_failed": why }));
                break;
            }
            Ok(_) => {}
        }
    }
    // The daemon tidies an ended session after saying it ended, and a drain
    // ends the daemon without waiting for that: wait for the session's
    // checkpoint refs, the last thing it removes, to be gone.
    let deadline = Instant::now() + Duration::from_secs(15);
    while checkpoints_remain(dir, session_id) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// Whether `session_id`'s checkpoint refs are still in the repository `dir`
/// is in; `false` outside a repository.
fn checkpoints_remain(dir: &Path, session_id: SessionId) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "for-each-ref",
            "--count=1",
            &format!("refs/horizon/checkpoints/{}/", session_id.as_uuid()),
        ])
        .output()
        .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
}

fn receive(
    updates: &mut tokio::sync::mpsc::Receiver<AgentUpdate>,
    deadline: Instant,
) -> Result<AgentUpdate, String> {
    loop {
        match updates.try_recv() {
            Ok(update) => return Ok(update),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                return Err("the session's updates stopped".into());
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for the session".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn answer(
    permission: &Permission,
    answer: &Answer,
    commands: &crossbeam_channel::Sender<AgentCommand>,
) -> Result<(), String> {
    print(json!({
        "approval": permission.reason,
        "call": permission.identity.occurrence_id,
        "answer": match answer {
            Answer::Approve => "approve".to_string(),
            Answer::Deny(reason) => format!("deny: {reason}"),
        },
    }));
    let identity = permission.identity.clone();
    let command = match answer {
        Answer::Approve => AgentCommand::Approve { identity },
        Answer::Deny(reason) => AgentCommand::Deny {
            identity,
            reason: Some(reason.clone()),
        },
    };
    commands
        .send(command)
        .map_err(|_| "the session stopped taking commands".to_string())
}

/// One frame item as a JSON line; `None` for the ones not worth a line.
fn describe(item: &AgentFrameItem) -> Option<Value> {
    Some(match item {
        AgentFrameItem::Message(message) => json!({
            "message": format!("{:?}", message.role),
            "text": message.text,
        }),
        AgentFrameItem::ToolCall(call) => tool_call(call),
        AgentFrameItem::Error(error) => json!({ "error": error }),
        AgentFrameItem::Exited(why) => json!({ "exited": why }),
        AgentFrameItem::TurnEnded { reason, elapsed } => json!({
            "turn_ended": format!("{reason:?}"),
            "seconds": elapsed.as_secs_f64(),
        }),
        AgentFrameItem::Notice(notice) => json!({ "notice": notice.text }),
        AgentFrameItem::Thought(_)
        | AgentFrameItem::Permission(_)
        | AgentFrameItem::Compaction(_) => return None,
    })
}

fn tool_call(call: &ToolCall) -> Value {
    json!({
        "tool": call.tool_id,
        "call": call.occurrence_id,
        "input": call.input,
        "status": format!("{:?}", call.status),
        "auto_approved": call.output.as_ref().and_then(|output| output.get("auto_approved")),
        "policy_tier": call.output.as_ref().and_then(|output| output.get("policy_tier")),
        "output": call.output.as_ref().map(shorten),
    })
}

/// A tool's output, with long strings cut so one line stays readable; the
/// whole of it is in the event log.
fn shorten(output: &Value) -> Value {
    match output {
        Value::String(text) if text.chars().count() > 2000 => {
            let head: String = text.chars().take(2000).collect();
            Value::String(format!("{head}… ({} chars)", text.chars().count()))
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), shorten(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(shorten).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, String> {
        parse(
            list.iter().map(|arg| arg.to_string()),
            PathBuf::from("/work"),
        )
    }

    #[test]
    fn prompts_are_sent_in_order_and_approvals_are_denied_by_default() {
        let parsed = args(&["first", "second"]).unwrap();
        assert_eq!(parsed.prompts, ["first", "second"]);
        assert_eq!(parsed.dir, PathBuf::from("/work"));
        assert!(matches!(parsed.answer, Answer::Deny(_)));
    }

    #[test]
    fn flags_set_the_directory_home_answer_and_timeout() {
        let parsed = args(&[
            "--dir",
            "/repo",
            "--home",
            "/probe",
            "--approve",
            "--timeout",
            "30",
            "go",
        ])
        .unwrap();
        assert_eq!(parsed.dir, PathBuf::from("/repo"));
        assert_eq!(parsed.home, Some(PathBuf::from("/probe")));
        assert_eq!(parsed.answer, Answer::Approve);
        assert_eq!(parsed.timeout, Duration::from_secs(30));
        assert_eq!(
            args(&["--deny", "not here", "go"]).unwrap().answer,
            Answer::Deny("not here".into())
        );
    }

    #[test]
    fn no_prompt_or_an_unknown_flag_is_refused() {
        assert!(args(&[]).is_err());
        assert!(args(&["--what", "go"]).is_err());
        assert!(args(&["--timeout", "soon", "go"]).is_err());
    }

    #[test]
    fn long_output_is_cut_and_says_how_long_it_was() {
        let output = json!({ "output": "x".repeat(2500), "exit_code": 0 });
        let short = shorten(&output);
        let text = short["output"].as_str().unwrap();
        assert!(text.ends_with("(2500 chars)"), "{text}");
        assert_eq!(short["exit_code"], 0);
    }
}
