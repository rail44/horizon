use super::*;
use crate::outbox::log::Index;
use crate::outbox::tests::record;
use horizon_agent::contract::{Command, Event};
use horizon_wiki::annotation::Agent;

fn wiki() -> (tempfile::TempDir, Store, Uuid, Annotation) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::at(dir.path().join("wiki"));
    store
        .write_page(
            "sandbox",
            "---\ntitle: Sandbox\n---\nThe helper runs nono first.\n",
        )
        .unwrap();
    let (page, question) = store
        .ask(
            "sandbox",
            4..10,
            Agent::person(Uuid::new_v4(), "owner"),
            markdown_body("Which helper?"),
        )
        .unwrap();
    (dir, store, page, question)
}

fn source_for(dir: &tempfile::TempDir, store: &Store) -> WikiSource {
    WikiSource {
        project: dir.path().to_path_buf(),
        store: store.clone(),
        lengths: HashMap::new(),
        waiting: HashSet::new(),
    }
}

fn delivered(receiver: &crossbeam_channel::Receiver<Command>) -> Vec<SessionInput> {
    receiver
        .try_iter()
        .filter_map(|command| match command {
            Command::SessionInput(input) => Some(input),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_owner_comment_reaches_its_thread_session_until_accepted() {
    let (dir, store, page, question) = wiki();
    let state = crate::session::test_support::state_with_rig_config(false, "test");
    let session = SessionId::from_uuid(agent::thread_session(&question.id));
    let receiver = state.install_test_session(session);
    let mut source = source_for(&dir, &store);
    let mut index = Index::default();

    source
        .tick(Context {
            state: &state,
            index: &index,
        })
        .await
        .unwrap();
    let inputs = delivered(&receiver);
    assert_eq!(inputs.len(), 1);
    let input = &inputs[0];
    assert_eq!(input.id, format!("wiki:{}", question.id));
    assert!(input.resume_work);
    assert!(input.text.contains("> helper"), "{}", input.text);
    let address: WikiAddress = serde_json::from_str(input.reply_to.as_deref().unwrap()).unwrap();
    assert_eq!(
        (address.page, address.parent.as_str()),
        (page, question.id.as_str())
    );

    // Unchanged log, not yet accepted: sent again.
    source
        .tick(Context {
            state: &state,
            index: &index,
        })
        .await
        .unwrap();
    assert_eq!(delivered(&receiver).len(), 1);

    index.fold(record(session, Event::InputAccepted(input.clone())));
    source
        .tick(Context {
            state: &state,
            index: &index,
        })
        .await
        .unwrap();
    assert!(delivered(&receiver).is_empty());
}

#[tokio::test]
async fn an_answer_is_recorded_once_and_suspends_an_answered_thread() {
    let (_dir, store, page, question) = wiki();
    let state = crate::session::test_support::state_with_rig_config(false, "test");
    let session = SessionId::from_uuid(agent::thread_session(&question.id));
    let receiver = state.install_test_session(session);
    let index = Index::default();
    let address = serde_json::to_string(&WikiAddress {
        kind: KIND.into(),
        store: store.dir().to_path_buf(),
        page,
        parent: question.id.clone(),
    })
    .unwrap();
    let reply = || Reply {
        source: session,
        id: "answer-1",
        address: &address,
        author: format!("session:{}", session.as_uuid()),
        text: "The sandbox helper.".into(),
        at: 0,
    };
    let ctx = Context {
        state: &state,
        index: &index,
    };
    assert!(WikiSink.deliver(ctx, reply()).await.unwrap());
    assert!(WikiSink.deliver(ctx, reply()).await.unwrap());

    let threads = store.threads(page).unwrap();
    assert_eq!(threads[0].replies.len(), 1);
    let answer = &threads[0].replies[0];
    assert_eq!(answer.body.value, "The sandbox helper.");
    assert_eq!(answer.creator, Agent::session(session.as_uuid()));
    assert!(agent::unanswered(&threads[0]).is_empty());
    assert!(receiver
        .try_iter()
        .any(|command| matches!(command, Command::Suspend)));
}

#[tokio::test]
async fn a_thread_with_a_newer_comment_is_not_suspended() {
    let (_dir, store, page, question) = wiki();
    let owner = Agent::person(Uuid::new_v4(), "owner");
    store
        .reply(page, &question.id, owner, markdown_body("And why first?"))
        .unwrap();
    let state = crate::session::test_support::state_with_rig_config(false, "test");
    let session = SessionId::from_uuid(agent::thread_session(&question.id));
    let receiver = state.install_test_session(session);
    let index = Index::default();
    let address = serde_json::to_string(&WikiAddress {
        kind: KIND.into(),
        store: store.dir().to_path_buf(),
        page,
        parent: question.id.clone(),
    })
    .unwrap();
    WikiSink
        .deliver(
            Context {
                state: &state,
                index: &index,
            },
            Reply {
                source: session,
                id: "answer-1",
                address: &address,
                author: String::new(),
                text: "The sandbox helper.".into(),
                at: 0,
            },
        )
        .await
        .unwrap();
    assert!(!receiver
        .try_iter()
        .any(|command| matches!(command, Command::Suspend)));
}

#[test]
fn registration_adds_the_role_skill_and_tools_once() {
    register();
    register();
    let role = horizon_agent::roles::resolve(&RoleId(THREAD_ROLE_ID.into())).unwrap();
    assert!(role.unattended);
    assert!(!horizon_agent::roles::user_launchable()
        .iter()
        .any(|role| role.id == THREAD_ROLE_ID));
    // Each wiki tool id is taken now.
    for tool in ["wiki.list", "wiki.read", "wiki.write", "wiki.edit"] {
        let again = ExternalTool {
            id: tool.into(),
            title: String::new(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
            requires_trusted_project: true,
            output: None,
            execute: Arc::new(|_| Ok(serde_json::Value::Null)),
        };
        assert!(
            horizon_agent::tools::register_external_tools(vec![again]).is_err(),
            "{tool}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_thread_session_is_spawned_then_resumed_after_a_suspend() {
    use horizon_agent::persistence::event_log::{WriterHandle, WriterInit};
    let (dir, store, page, question) = wiki();
    let state = crate::session::test_support::state_with_rig_config(false, "mock");
    let path = dir.path().join("agent-events.jsonl");
    state
        .agent_config
        .lock()
        .unwrap()
        .persistence
        .event_log_path = path.clone();
    let (writer, ready) = WriterHandle::open(&path);
    assert!(matches!(ready.recv().unwrap(), WriterInit::Ready(_)));
    state.set_writer(Some(writer));
    register();
    let session = SessionId::from_uuid(agent::thread_session(&question.id));
    let source = source_for(&dir, &store);

    source.ensure_started(&state, session).unwrap();
    assert!(state.session_exists(session));
    assert_eq!(
        crate::session::Connection::new(state.clone())
            .session_list()
            .into_iter()
            .find(|listed| listed.session_id == session)
            .and_then(|listed| listed.role_id)
            .map(|role| role.0),
        Some(THREAD_ROLE_ID.to_string())
    );

    assert!(state.suspend_session(session));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while state.session_exists(session) {
        assert!(
            std::time::Instant::now() < deadline,
            "the suspend did not end the session"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    store
        .reply(
            page,
            &question.id,
            Agent::person(Uuid::new_v4(), "owner"),
            markdown_body("And why first?"),
        )
        .unwrap();
    source.ensure_started(&state, session).unwrap();
    assert!(state.session_exists(session));
    state.writer().unwrap().flush().unwrap();
    let history = state.flushed_session_records(session).unwrap();
    assert!(history
        .iter()
        .any(|record| matches!(record.event, Event::SessionResumed)));
    state.send_command(session, Command::Shutdown);
}
