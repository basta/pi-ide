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

    tokio::spawn(run(
        client_read,
        client_write,
        cmd_rx,
        in_tx,
        Arc::new(log),
        None,
    ));
    tokio::spawn(async move {
        let mut lines = BufReader::new(pi_read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let cmd: Value = serde_json::from_str(&line).unwrap();
            let _ = rec_tx.send(cmd.clone());
            if cmd["type"] == "prompt" {
                for l in &extra {
                    pi_write
                        .write_all(format!("{l}\n").as_bytes())
                        .await
                        .unwrap();
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
    Harness {
        commands: cmd_tx,
        inbound: in_rx,
        received: rec_rx,
    }
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
    h.commands
        .send(json!({"id":"p1","type":"prompt","message":"read note.txt"}).to_string())
        .unwrap();
    let recs = collect_until_settled(&mut h.inbound).await;

    let events: Vec<&AgentEvent> = recs
        .iter()
        .filter_map(|r| {
            if let Inbound::Event(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .collect();

    // Responses to the recorded get_state and prompt came through.
    let responses: Vec<(&str, bool)> = recs
        .iter()
        .filter_map(|r| {
            if let Inbound::Response {
                command, success, ..
            } = r
            {
                Some((command.as_str(), *success))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(responses, vec![("get_state", true), ("prompt", true)]);

    // Extension status from the user's search extension.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Status { key, .. } if key == "search"))
    );

    // The tool call, start and end, with the file's contents.
    let start = events.iter().find_map(|e| match e {
        AgentEvent::ToolStart { id, name, args } => Some((id.clone(), name.clone(), args.clone())),
        _ => None,
    });
    let (id, name, args) = start.expect("tool start");
    assert_eq!(name, "read");
    assert_eq!(args["path"], "note.txt");
    let end = events.iter().find_map(|e| match e {
        AgentEvent::ToolEnd {
            id: end_id,
            result,
            is_error,
            ..
        } if *end_id == id => Some((result.clone(), *is_error)),
        _ => None,
    });
    let (result, is_error) = end.expect("tool end with matching id");
    assert!(!is_error);
    assert_eq!(result["content"][0]["text"], "hello from fixture\n");

    // Streamed answer text reassembles to the authoritative text_end content.
    let streamed: String = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::TextDelta { delta, .. } = e {
                Some(delta.as_str())
            } else {
                None
            }
        })
        .collect();
    let final_text = events.iter().find_map(|e| {
        if let AgentEvent::TextEnd { content, .. } = e {
            Some(content.clone())
        } else {
            None
        }
    });
    assert_eq!(final_text.as_deref(), Some("hello from fixture"));
    assert_eq!(streamed, "hello from fixture");

    // Two assistant messages (tool call, then answer), thinking streamed, clean settle.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::AssistantStart))
            .count(),
        2
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ThinkingDelta { .. }))
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Settled { aborted: false })
    ));
    // Nothing in a real session should be unknown.
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::Unknown(_))),
        "unexpected Unknown events"
    );
}

#[tokio::test]
async fn blocking_dialogs_reach_the_client_unanswered() {
    let dialog = json!({"type":"extension_ui_request","id":"ui-7","method":"confirm","title":"Allow rm -rf?","message":"target/debug"}).to_string();
    let mut h = start(vec![dialog], RawLog::disabled());
    h.commands
        .send(json!({"type":"prompt","message":"x"}).to_string())
        .unwrap();
    let recs = collect_until_settled(&mut h.inbound).await;
    let d = recs
        .iter()
        .find_map(|r| {
            if let Inbound::Dialog(d) = r {
                Some(d.clone())
            } else {
                None
            }
        })
        .expect("dialog forwarded");
    assert_eq!(d.id, "ui-7");
    assert_eq!(d.message.as_deref(), Some("target/debug"));
    // The connection must not answer on the user's behalf.
    while let Ok(cmd) = h.received.try_recv() {
        assert_ne!(cmd["type"], "extension_ui_response");
    }
}

#[tokio::test]
async fn raw_log_records_both_directions() {
    let dir = std::env::temp_dir().join(format!("pide-rawlog-{}", std::process::id()));
    let path = dir.join("pi-rpc.jsonl");
    let _ = std::fs::remove_file(&path);
    let mut h = start(vec![], RawLog::open(path.clone()));
    h.commands
        .send(json!({"type":"prompt","message":"x"}).to_string())
        .unwrap();
    collect_until_settled(&mut h.inbound).await;

    let log = std::fs::read_to_string(&path).unwrap();
    let recs: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(recs.iter().filter(|r| r["dir"] == "out").count(), 1);
    assert_eq!(
        recs.iter().filter(|r| r["dir"] == "in").count(),
        FIXTURE.lines().count()
    );
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
                Inbound::Event(AgentEvent::Queue {
                    steering,
                    follow_up,
                }) => queues.push((steering, follow_up)),
                Inbound::Event(AgentEvent::UserPrompt { text }) => users.push(text),
                Inbound::Response {
                    command,
                    success: false,
                    error,
                    ..
                } if command == "prompt" => rejected = error,
                _ => {}
            }
        }
    }
    assert_eq!(
        queues.first().unwrap().0,
        vec!["Also mention the word banana in your reply."]
    );
    assert!(
        queues
            .iter()
            .any(|(_, f)| f == &vec!["Now reply with just: follow-up received.".to_string()])
    );
    assert_eq!(queues.last().unwrap(), &(vec![], vec![]));
    // Delivered steer and follow-up show up as user messages.
    assert!(users.iter().any(|u| u.contains("banana")));
    assert!(users.iter().any(|u| u.contains("follow-up received")));
    // A plain prompt while busy is rejected — why the client must steer instead.
    assert!(rejected.unwrap().contains("streamingBehavior"));
}

#[test]
fn recorded_dialogs_parse() {
    // Recorded from pi 1.5.0 running dev/pi-ask-test.ts: `/ask-test` asks one of each.
    use pide_core::DialogKind;
    let fixture = include_str!("fixtures/dialogs_session.jsonl");
    let dialogs: Vec<_> = fixture
        .lines()
        .flat_map(pide_agent_pi::wire::parse_line)
        .filter_map(|r| {
            if let Inbound::Dialog(d) = r {
                Some(d)
            } else {
                None
            }
        })
        .collect();
    let kinds: Vec<DialogKind> = dialogs.iter().map(|d| d.kind).collect();
    assert_eq!(
        kinds,
        [
            DialogKind::Confirm,
            DialogKind::Select,
            DialogKind::Input,
            DialogKind::Editor
        ]
    );
    assert_eq!(
        dialogs[0].message.as_deref(),
        Some("rm -rf target/debug/incremental")
    );
    assert_eq!(dialogs[1].options, ["Kimi K2.6", "GLM 5.2", "Sonnet 5.5"]);
    assert_eq!(
        dialogs[2].placeholder.as_deref(),
        Some("e.g. crlf-save-fix")
    );
    assert!(
        dialogs[3]
            .prefill
            .as_deref()
            .unwrap()
            .starts_with("Keep CRLF")
    );
    assert!(dialogs.iter().all(|d| !d.id.is_empty()));
}

#[tokio::test]
async fn file_tools_get_a_before_snapshot() {
    // Shapes recorded from pi 1.5.0 (edit + write); the fake pi writes the file
    // only after announcing the call, as real pi does.
    let dir = std::env::temp_dir().join(format!("pide-snap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "alpha\nbeta\n").unwrap();
    let _ = std::fs::remove_file(dir.join("new.txt"));

    let (client_io, pi_io) = tokio::io::duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (_pi_read, mut pi_write) = tokio::io::split(pi_io);
    let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (in_tx, mut in_rx) = mpsc::unbounded_channel();
    tokio::spawn(run(
        client_read,
        client_write,
        cmd_rx,
        in_tx,
        Arc::new(RawLog::disabled()),
        Some(dir.clone()),
    ));

    let lines = [
        r#"{"type":"tool_execution_start","toolCallId":"functions.edit:1","toolName":"edit","args":{"path":"a.txt","edits":[{"oldText":"beta","newText":"BETA"}]}}"#,
        r#"{"type":"tool_execution_start","toolCallId":"functions.write:2","toolName":"write","args":{"path":"new.txt","content":"hello"}}"#,
        r#"{"type":"tool_execution_start","toolCallId":"functions.read:3","toolName":"read","args":{"path":"a.txt"}}"#,
    ];
    for (i, l) in lines.iter().enumerate() {
        pi_write
            .write_all(format!("{l}\n").as_bytes())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        // "pi" now runs that tool:
        match i {
            0 => std::fs::write(dir.join("a.txt"), "alpha\nBETA\n").unwrap(),
            1 => std::fs::write(dir.join("new.txt"), "hello").unwrap(),
            _ => {}
        }
    }
    let mut touched = Vec::new();
    while let Ok(Some(rec)) = tokio::time::timeout(Duration::from_millis(300), in_rx.recv()).await {
        if let Inbound::Event(AgentEvent::FileTouched {
            tool, path, before, ..
        }) = rec
        {
            touched.push((
                tool,
                path.file_name().unwrap().to_string_lossy().to_string(),
                before,
            ));
        }
    }
    assert_eq!(
        touched.len(),
        2,
        "only edit and write are file tools: {touched:?}"
    );
    assert_eq!(
        touched[0],
        ("edit".into(), "a.txt".into(), Some("alpha\nbeta\n".into()))
    );
    assert_eq!(touched[1], ("write".into(), "new.txt".into(), None));
    let _ = std::fs::remove_dir_all(dir);
}

/// get_state / set_model / get_available_models as pi 1.5.0 answered them.
#[test]
fn model_responses_parse() {
    let lines: Vec<serde_json::Value> = include_str!("fixtures/model_rpc_responses.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let by = |c: &str| lines.iter().find(|v| v["command"] == c).unwrap()["data"].clone();
    let m = pide_agent_pi::wire::model_info(&by("get_state")["model"]).unwrap();
    assert_eq!(
        (m.provider.as_str(), m.id.as_str(), m.reasoning),
        ("openrouter", "moonshotai/kimi-k2.6", true)
    );
    assert_eq!(by("get_state")["thinkingLevel"], "minimal");
    let set = pide_agent_pi::wire::model_info(&by("set_model")).unwrap();
    assert_eq!(set.id, "~anthropic/claude-haiku-latest");
    let models = by("get_available_models")["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(pide_agent_pi::wire::model_info)
        .count();
    assert_eq!(models, 3);
}

#[test]
fn slash_commands_parse() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/get_commands_response.json").trim()).unwrap();
    let cmds = pide_agent_pi::wire::slash_commands(&v["data"]);
    assert_eq!(cmds.len(), 4);
    assert_eq!(
        (cmds[0].name.as_str(), cmds[0].source.as_str()),
        ("search-setup", "extension")
    );
    assert!(!cmds[0].description.is_empty());
}

#[test]
fn session_stats_parse() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/session_stats_response.json").trim()).unwrap();
    let s = pide_agent_pi::wire::session_stats(&v["data"]);
    assert_eq!(s.tokens_total, 33965);
    assert!((s.cost - 0.0073).abs() < 0.0001);
    assert_eq!(s.context_window, Some(262144));
    assert_eq!(s.context_tokens, Some(3499));
    assert!(s.context_percent.unwrap() > 1.0);
}

#[test]
fn thinking_levels_parse() {
    let lines: Vec<serde_json::Value> = include_str!("fixtures/thinking_rpc_responses.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let levels = pide_agent_pi::wire::thinking_levels(&lines[0]["data"]);
    assert_eq!(levels, ["off", "minimal", "low", "medium", "high"]);
    assert_eq!(lines[1]["command"], "set_thinking_level");
    assert_eq!(lines[2]["data"]["thinkingLevel"], "medium");
}

/// Collaborator mode (docs/COLLAB.md): a recorded run with pide's extension loaded.
/// Its tools' payloads arrive in `tool_execution_end`'s `result.details`.
#[test]
fn collab_tool_results_carry_details() {
    let fixture = include_str!("fixtures/collab_tools_session.jsonl");
    let mut kinds = Vec::new();
    for line in fixture.lines() {
        for rec in pide_agent_pi::wire::parse_line(line) {
            if let Inbound::Event(AgentEvent::ToolEnd { name, result, is_error, .. }) = rec
                && name.starts_with("ide_")
            {
                assert!(!is_error);
                assert_eq!(result["details"]["pide"], 1);
                kinds.push(result["details"]["kind"].as_str().unwrap().to_string());
            }
        }
    }
    assert_eq!(kinds, ["progress", "progress", "explain", "pin"]);
}

#[test]
fn extension_source_is_embedded() {
    let src = pide_agent_pi::backend::EXTENSION_SOURCE;
    for tool in ["ide_pin", "ide_explain_change", "ide_progress"] {
        assert!(src.contains(&format!("name: \"{tool}\"")), "{tool}");
    }
}

#[test]
fn collab_events_parse_from_the_recording() {
    let fixture = include_str!("fixtures/collab_tools_session.jsonl");
    let events: Vec<AgentEvent> = fixture
        .lines()
        .flat_map(pide_agent_pi::wire::parse_line)
        .filter_map(|r| match r {
            Inbound::Event(e @ (AgentEvent::Pin(_) | AgentEvent::ExplainChange { .. } | AgentEvent::Progress(_))) => Some(e),
            _ => None,
        })
        .collect();
    assert_eq!(events.len(), 4);
    match &events[1] {
        AgentEvent::Progress(steps) => {
            assert_eq!(steps.len(), 2);
            assert!(steps.iter().all(|s| s.done));
            assert_eq!(steps[0].text, "Guard divide against zero divisor");
        }
        e => panic!("{e:?}"),
    }
    match &events[2] {
        AgentEvent::ExplainChange { path, summary, hunks } => {
            assert_eq!(path, std::path::Path::new("calc.py"));
            assert!(summary.starts_with("Raise a clear ValueError"));
            assert_eq!(hunks.len(), 2);
            assert_eq!((hunks[0].anchor.line, hunks[0].anchor.text.as_str()), (6, "    if b == 0:"));
            assert_eq!(hunks[1].anchor.line, 12);
        }
        e => panic!("{e:?}"),
    }
    match &events[3] {
        AgentEvent::Pin(pin) => {
            assert_eq!(pin.path, std::path::Path::new("calc.py"));
            assert_eq!((pin.anchor.line, pin.end_line), (11, 14));
            assert_eq!(pin.anchor.text, "def mean(xs):");
            assert_eq!(pin.id, "toolu_01PogxJhsPE9Nfd1b7PAYd3E");
            assert!(pin.note.contains("empty list"));
        }
        e => panic!("{e:?}"),
    }
    // A refused pin (over the cap) is an error result: no event.
    let refused = r#"{"type":"tool_execution_end","toolCallId":"x","toolName":"ide_pin","result":{"content":[{"type":"text","text":"Not pinned"}],"details":{"pide":1,"kind":"refused"}},"isError":true}"#;
    assert_eq!(pide_agent_pi::wire::parse_line(refused).len(), 1);
}
