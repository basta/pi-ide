//! Drives the real RPC connection (framing, logging, parsing, dialog handling)
//! against a session recorded from pi 1.5.0: a prompt that makes pi call the
//! `read` tool on note.txt and answer with its contents.

use std::sync::Arc;
use std::time::Duration;

use pide_agent_pi::conn::{RawLog, run};
use pide_agent_pi::wire::Inbound;
use pide_core::AgentEvent;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

const FIXTURE: &str = include_str!("fixtures/read_tool_session.jsonl");

struct Harness {
    commands: mpsc::UnboundedSender<String>,
    inbound: mpsc::UnboundedReceiver<Inbound>,
    /// What the fake pi received on stdin.
    received: mpsc::UnboundedReceiver<Value>,
}

/// A fake pi: answers `get_state`, and replays the fixture after `prompt`.
/// `extra` lines are written before the fixture (e.g. a dialog request).
fn start(extra: Vec<String>, log: RawLog) -> Harness {
    let (client_io, pi_io) = tokio::io::duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (pi_read, mut pi_write) = tokio::io::split(pi_io);

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (rec_tx, rec_rx) = mpsc::unbounded_channel();

    tokio::spawn(run(client_read, client_write, cmd_rx, in_tx, Arc::new(log)));
    tokio::spawn(async move {
        let mut lines = BufReader::new(pi_read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let cmd: Value = serde_json::from_str(&line).unwrap();
            let _ = rec_tx.send(cmd.clone());
            if cmd["type"] == "prompt" {
                for l in &extra {
                    pi_write.write_all(format!("{l}\n").as_bytes()).await.unwrap();
                }
                // Write in odd-sized chunks, with CRLF on one line, to exercise framing.
                let mut data = FIXTURE.replacen('\n', "\r\n", 1).into_bytes();
                while !data.is_empty() {
                    let n = data.len().min(777);
                    pi_write.write_all(&data[..n]).await.unwrap();
                    data.drain(..n);
                }
            }
        }
    });
    Harness { commands: cmd_tx, inbound: in_rx, received: rec_rx }
}

async fn collect_until_settled(rx: &mut mpsc::UnboundedReceiver<Inbound>) -> Vec<Inbound> {
    let mut out = Vec::new();
    loop {
        let rec = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for agent_settled")
            .expect("connection closed early");
        let done = matches!(rec, Inbound::Event(AgentEvent::Settled { .. }));
        out.push(rec);
        if done {
            return out;
        }
    }
}

#[tokio::test]
async fn replays_a_tool_call_session() {
    let mut h = start(vec![], RawLog::disabled());
    h.commands.send(json!({"id":"p1","type":"prompt","message":"read note.txt"}).to_string()).unwrap();
    let recs = collect_until_settled(&mut h.inbound).await;

    let events: Vec<&AgentEvent> = recs
        .iter()
        .filter_map(|r| if let Inbound::Event(e) = r { Some(e) } else { None })
        .collect();

    // Responses to the recorded get_state and prompt came through.
    let responses: Vec<(&str, bool)> = recs
        .iter()
        .filter_map(|r| if let Inbound::Response { command, success, .. } = r { Some((command.as_str(), *success)) } else { None })
        .collect();
    assert_eq!(responses, vec![("get_state", true), ("prompt", true)]);

    // Extension status from the user's search extension.
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Status { key, .. } if key == "search")));

    // The tool call, start and end, with the file's contents.
    let start = events.iter().find_map(|e| match e {
        AgentEvent::ToolStart { id, name, args } => Some((id.clone(), name.clone(), args.clone())),
        _ => None,
    });
    let (id, name, args) = start.expect("tool start");
    assert_eq!(name, "read");
    assert_eq!(args["path"], "note.txt");
    let end = events.iter().find_map(|e| match e {
        AgentEvent::ToolEnd { id: end_id, result, is_error, .. } if *end_id == id => Some((result.clone(), *is_error)),
        _ => None,
    });
    let (result, is_error) = end.expect("tool end with matching id");
    assert!(!is_error);
    assert_eq!(result["content"][0]["text"], "hello from fixture\n");

    // Streamed answer text reassembles to the authoritative text_end content.
    let streamed: String = events
        .iter()
        .filter_map(|e| if let AgentEvent::TextDelta { delta, .. } = e { Some(delta.as_str()) } else { None })
        .collect();
    let final_text = events.iter().find_map(|e| if let AgentEvent::TextEnd { content, .. } = e { Some(content.clone()) } else { None });
    assert_eq!(final_text.as_deref(), Some("hello from fixture"));
    assert_eq!(streamed, "hello from fixture");

    // Two assistant messages (tool call, then answer), thinking streamed, clean settle.
    assert_eq!(events.iter().filter(|e| matches!(e, AgentEvent::AssistantStart)).count(), 2);
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ThinkingDelta { .. })));
    assert!(matches!(events.last(), Some(AgentEvent::Settled { aborted: false })));
    // Nothing in a real session should be unknown.
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::Unknown(_))), "unexpected Unknown events");
}

#[tokio::test]
async fn blocking_dialogs_are_cancelled() {
    let dialog = json!({"type":"extension_ui_request","id":"ui-7","method":"confirm","title":"Allow rm -rf?"}).to_string();
    let mut h = start(vec![dialog], RawLog::disabled());
    h.commands.send(json!({"type":"prompt","message":"x"}).to_string()).unwrap();
    let recs = collect_until_settled(&mut h.inbound).await;
    assert!(recs.iter().any(|r| matches!(r, Inbound::Event(AgentEvent::Notify { message, .. }) if message.contains("confirm"))));

    let mut answered = false;
    while let Ok(cmd) = h.received.try_recv() {
        if cmd["type"] == "extension_ui_response" {
            assert_eq!(cmd["id"], "ui-7");
            assert_eq!(cmd["cancelled"], true);
            answered = true;
        }
    }
    assert!(answered, "dialog was not answered");
}

#[tokio::test]
async fn raw_log_records_both_directions() {
    let dir = std::env::temp_dir().join(format!("pide-rawlog-{}", std::process::id()));
    let path = dir.join("pi-rpc.jsonl");
    let _ = std::fs::remove_file(&path);
    let mut h = start(vec![], RawLog::open(path.clone()));
    h.commands.send(json!({"type":"prompt","message":"x"}).to_string()).unwrap();
    collect_until_settled(&mut h.inbound).await;

    let log = std::fs::read_to_string(&path).unwrap();
    let recs: Vec<Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(recs.iter().filter(|r| r["dir"] == "out").count(), 1);
    assert_eq!(recs.iter().filter(|r| r["dir"] == "in").count(), FIXTURE.lines().count());
    // Lines are logged raw (CR stripped), so they still parse as the original records.
    for r in recs.iter().filter(|r| r["dir"] == "in") {
        let _: Value = serde_json::from_str(r["line"].as_str().unwrap()).unwrap();
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn steer_and_follow_up_queue_updates_parse() {
    // Recorded from pi 1.5.0: a prompt, then a steer and a follow-up while busy.
    let fixture = include_str!("fixtures/steer_followup_session.jsonl");
    let mut queues = Vec::new();
    let mut users = Vec::new();
    let mut rejected = None;
    for line in fixture.lines() {
        for rec in pide_agent_pi::wire::parse_line(line) {
            match rec {
                Inbound::Event(AgentEvent::Queue { steering, follow_up }) => queues.push((steering, follow_up)),
                Inbound::Event(AgentEvent::UserPrompt { text }) => users.push(text),
                Inbound::Response { command, success: false, error, .. } if command == "prompt" => rejected = error,
                _ => {}
            }
        }
    }
    assert_eq!(queues.first().unwrap().0, vec!["Also mention the word banana in your reply."]);
    assert!(queues.iter().any(|(_, f)| f == &vec!["Now reply with just: follow-up received.".to_string()]));
    assert_eq!(queues.last().unwrap(), &(vec![], vec![]));
    // Delivered steer and follow-up show up as user messages.
    assert!(users.iter().any(|u| u.contains("banana")));
    assert!(users.iter().any(|u| u.contains("follow-up received")));
    // A plain prompt while busy is rejected — why the client must steer instead.
    assert!(rejected.unwrap().contains("streamingBehavior"));
}
