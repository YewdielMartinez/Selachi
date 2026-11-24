use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use agent_harness::{ApprovalBroker, ApprovalRequest, PROTOCOL_VERSION, PipeBroker};
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};

use super::*;

static N: AtomicU32 = AtomicU32::new(0);

fn unique_pipe() -> String {
    format!(
        r"\\.\pipe\selachi-approvals-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )
}

fn request(id: &str, timeout_secs: u64) -> ApprovalRequest {
    ApprovalRequest {
        v: PROTOCOL_VERSION,
        request_id: id.into(),
        run_id: "12".into(),
        thread_id: 12,
        parent_thread_id: Some(7),
        agent: "codex".into(),
        tool: "Bash".into(),
        detail: "rm -rf build".into(),
        permission: "ask".into(),
        depth: 1,
        timeout_secs,
    }
}

/// Blocks (test thread) for the next server event, at most 10 s.
fn next_event(rx: &mut UnboundedReceiver<ServerEvent>) -> ServerEvent {
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < end {
        match rx.try_recv() {
            Ok(ev) => return ev,
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("no server event within 10 s");
}

fn serve() -> (String, UnboundedReceiver<ServerEvent>) {
    let path = unique_pipe();
    let (tx, rx) = unbounded();
    start(&path, tx).unwrap_or_else(|e| panic!("the pipe {path:?} starts: {e:?}"));
    (path, rx)
}

#[test]
fn the_pipe_answers_ping_like_a_selachi_window() {
    let (path, _rx) = serve();
    assert!(PipeBroker::new(path).ping());
}

#[test]
fn a_request_reaches_the_gui_and_the_decision_goes_back() {
    let (path, mut rx) = serve();
    for (decision, id) in [
        (Decision::Allow, "r-allow"),
        (Decision::Always, "r-always"),
        (Decision::Deny, "r-deny"),
    ] {
        let broker = PipeBroker::new(path.clone());
        let req = request(id, 30);
        let client = std::thread::spawn(move || broker.decide(&req));
        let ServerEvent::Request(incoming) = next_event(&mut rx) else {
            panic!("expected a request");
        };
        assert_eq!(incoming.request.request_id, id);
        assert_eq!(incoming.request.parent_thread_id, Some(7));
        incoming.answer(
            decision,
            (decision == Decision::Deny).then(|| "no".to_string()),
        );
        let verdict = client.join().unwrap();
        assert_eq!(verdict.decision, decision);
        assert!(verdict.forwarded, "the user decided, not the harness");
    }
}

#[test]
fn no_answer_is_a_denial_and_the_card_is_told_to_go() {
    let (path, mut rx) = serve();
    let broker = PipeBroker::new(path);
    let req = request("slow", 1); // the server grants at least MIN_WAIT_SECS
    let client = std::thread::spawn(move || broker.decide(&req));
    let ServerEvent::Request(_incoming_kept_unanswered) = next_event(&mut rx) else {
        panic!("expected a request");
    };
    // The client's own timeout (1 s) fires first and denies; the server keeps waiting 5 s and then
    // announces the expiry.
    let verdict = client.join().unwrap();
    assert_eq!(verdict.decision, Decision::Deny);
    match next_event(&mut rx) {
        ServerEvent::Expired(id) => assert_eq!(id, "slow"),
        other => panic!("expected Expired, got {other:?}"),
    }
}

#[test]
fn a_dropped_request_is_denied_with_a_reason() {
    let (path, mut rx) = serve();
    let broker = PipeBroker::new(path);
    let req = request("dropped", 30);
    let client = std::thread::spawn(move || broker.decide(&req));
    let ServerEvent::Request(incoming) = next_event(&mut rx) else {
        panic!("expected a request");
    };
    drop(incoming);
    let verdict = client.join().unwrap();
    assert_eq!(verdict.decision, Decision::Deny);
}

#[test]
fn bad_requests_are_refused_without_reaching_the_gui() {
    let (tx, mut rx) = unbounded();
    let mut bad = request("x", 30);
    bad.v = 99;
    let reply = handle_message(ClientMessage::ApprovalRequest(bad), &tx, wait_for);
    assert!(matches!(reply, ServerMessage::Error { .. }));
    let mut long = request("y", 30);
    long.detail = "a".repeat(20_000);
    let reply = handle_message(ClientMessage::ApprovalRequest(long), &tx, wait_for);
    assert!(matches!(reply, ServerMessage::Error { .. }));
    let empty_id = request("", 30);
    assert!(matches!(
        handle_message(ClientMessage::ApprovalRequest(empty_id), &tx, wait_for),
        ServerMessage::Error { .. }
    ));
    drop(tx);
    assert!(
        futures::executor::block_on(rx.next()).is_none(),
        "nothing was forwarded"
    );
}

#[test]
fn the_wait_is_clamped() {
    assert_eq!(
        wait_for(&request("a", 0)),
        Duration::from_secs(MIN_WAIT_SECS)
    );
    assert_eq!(wait_for(&request("a", 120)), Duration::from_secs(120));
    assert_eq!(
        wait_for(&request("a", 999_999)),
        Duration::from_secs(MAX_WAIT_SECS)
    );
}

#[test]
fn garbage_and_oversized_lines_get_an_error_not_a_hang() {
    use std::io::{Read as _, Write as _};
    let (path, _rx) = serve();
    for payload in [b"not json\n".to_vec(), {
        let mut v = vec![b'a'; MAX_LINE_BYTES + 10];
        v.push(b'\n');
        v
    }] {
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        f.write_all(&payload).unwrap();
        let mut reply = String::new();
        f.read_to_string(&mut reply).ok();
        let msg: ServerMessage = serde_json::from_str(reply.trim()).expect("an error line");
        assert!(matches!(msg, ServerMessage::Error { .. }), "{msg:?}");
    }
}

#[test]
fn a_second_server_cannot_squat_the_name() {
    let (path, _rx) = serve();
    let (tx2, _rx2) = unbounded();
    assert!(start(&path, tx2).is_err(), "FILE_FLAG_FIRST_PIPE_INSTANCE");
}
