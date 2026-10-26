//! Live read-only probe of a running DSH bridge.
//!
//! Ignored by default: it needs a DSH on this machine and must never run in CI or
//! touch a service the developer did not start. It exists so "the DSH runtime
//! really works" can be re-checked with this agent's own transport instead of a
//! one-off script, and it is read-only on purpose — `initialize`, `session.list`
//! and the catalogs only. Creating a session would run a real turn in somebody's
//! DSH, which is not something a test may do on its own.
//!
//! Run it with:
//!   cargo test -p farwork-agent --test dsh_live -- --ignored --nocapture

use farwork_agent::cli;
use farwork_agent::dsh::client::DshClient;
use farwork_agent::dsh::discovery;
use farwork_agent::dsh::handshake::InitializeParams;
use farwork_agent::dsh::rpc;
use farwork_agent::dsh::selections;

#[tokio::test]
#[ignore = "needs a running DSH bridge; read-only"]
async fn initialize_and_list_against_a_real_bridge() {
    let env = |key: &str| std::env::var(key).ok();
    let endpoint = match discovery::resolve(None, &env) {
        Ok(located) => located.endpoint,
        Err(error) => {
            println!("PROBE SKIP: no DSH bridge endpoint ({error})");
            return;
        }
    };
    println!(
        "PROBE endpoint {} pid={:?}",
        endpoint.address(),
        endpoint.pid
    );

    let connection = rpc::dial(&endpoint.address()).await.expect("dial");
    let client = DshClient::new(connection.client);
    let params = InitializeParams {
        auth_token: &endpoint.token,
        connector_id: "farwork-agent-probe",
        session_namespace: "farwork-probe",
        client_version: env!("CARGO_PKG_VERSION"),
    };
    let handshake = client
        .initialize(&endpoint.address(), &params)
        .await
        .expect("initialize");
    println!("PROBE identity {:?}", handshake.identity);
    println!("PROBE features {:?}", handshake.features);
    assert_eq!(handshake.identity.runtime, "dsh");
    assert!(
        handshake.features.events(),
        "the event stream must be offered"
    );
    assert_eq!(
        handshake.features.projection_version(),
        if handshake.features.projection_version == Some(2) {
            2
        } else {
            3
        }
    );

    let sessions = client.list_sessions(5).await.expect("session.list");
    println!("PROBE session.list count={}", sessions.len());
    for session in sessions.iter().take(5) {
        println!(
            "PROBE session {} external={:?} title={:?} cwd={:?}",
            session
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("-"),
            session.get("externalSessionId").and_then(|v| v.as_str()),
            session.get("title").and_then(|v| v.as_str()),
            session.get("cwd").and_then(|v| v.as_str()),
        );
    }

    let models = client.list_models().await.expect("catalog.listModels");
    println!("PROBE catalog.listModels count={}", models.len());
    for model in models.iter().take(3) {
        println!("PROBE model {} = {}", model.id, model.title);
    }
    let permissions = client
        .list_permissions()
        .await
        .expect("catalog.listPermissions");
    println!("PROBE catalog.listPermissions count={}", permissions.len());
    for permission in permissions.iter().take(4) {
        println!("PROBE permission {} = {}", permission.id, permission.title);
    }

    client.rpc().close().await;
}

/// The proof that `--dsh-permission read-only` lands on a real catalog entry.
///
/// Still read-only: the flag is parsed by the real CLI parser and resolved by the
/// exact function startup uses, but no session is created — that would run a turn
/// in somebody's DSH.
#[tokio::test]
#[ignore = "needs a running DSH bridge; read-only"]
async fn read_only_permission_resolves_to_a_real_selection_id() {
    let env = |key: &str| std::env::var(key).ok();
    let endpoint = match discovery::resolve(None, &env) {
        Ok(located) => located.endpoint,
        Err(error) => {
            println!("PROBE SKIP: no DSH bridge endpoint ({error})");
            return;
        }
    };
    let cli = cli::parse(
        vec![
            String::from("--runtime"),
            String::from("dsh"),
            String::from("--dsh-permission"),
            String::from("read-only"),
        ],
        &|_: &str| None,
    )
    .expect("cli");
    assert_eq!(cli.dsh_permission.as_deref(), Some("read-only"));

    let selections = selections::resolve_remote(
        &endpoint,
        "farwork-agent-probe",
        "farwork-probe",
        env!("CARGO_PKG_VERSION"),
        cli.dsh_permission.as_deref(),
        cli.dsh_model.as_deref(),
        cli.dsh_preset.as_deref(),
    )
    .await
    .expect("resolve");
    let permission = selections
        .permission
        .clone()
        .expect("a permission was asked for");
    println!("PROBE --dsh-permission read-only -> {permission}");
    assert!(permission.starts_with("dsh:permission:"), "{permission}");

    // The resolved id must be one the bridge itself lists, not one we invented.
    let connection = rpc::dial(&endpoint.address()).await.expect("dial");
    let client = DshClient::new(connection.client);
    let params = InitializeParams {
        auth_token: &endpoint.token,
        connector_id: "farwork-agent-probe",
        session_namespace: "farwork-probe",
        client_version: env!("CARGO_PKG_VERSION"),
    };
    client
        .initialize(&endpoint.address(), &params)
        .await
        .expect("initialize");
    let permissions = client
        .list_permissions()
        .await
        .expect("catalog.listPermissions");
    let matched = permissions
        .iter()
        .find(|entry| entry.id == permission)
        .unwrap_or_else(|| panic!("{permission} is not in catalog.listPermissions"));
    println!(
        "PROBE resolved selection {} = {}",
        matched.id, matched.title
    );
    assert!(
        matched.title.eq_ignore_ascii_case("read-only"),
        "read-only must not resolve to {:?}",
        matched.title
    );
    client.rpc().close().await;
}

/// The Agent-mode half of the same proof: every real preset, and what a short
/// name resolves to.
///
/// Still read-only: `initialize` plus `catalog.listAgentPresets`. The refusal
/// this flag exists for lives in `createAndStart`, which a probe must not call.
#[tokio::test]
#[ignore = "needs a running DSH bridge; read-only"]
async fn agent_presets_resolve_to_real_selection_ids() {
    let env = |key: &str| std::env::var(key).ok();
    let endpoint = match discovery::resolve(None, &env) {
        Ok(located) => located.endpoint,
        Err(error) => {
            println!("PROBE SKIP: no DSH bridge endpoint ({error})");
            return;
        }
    };

    let cli = cli::parse(
        vec![
            String::from("--runtime"),
            String::from("dsh"),
            String::from("--dsh-preset"),
            String::from("ptc"),
        ],
        &|_: &str| None,
    )
    .expect("cli");
    assert_eq!(cli.dsh_preset.as_deref(), Some("ptc"));
    let selections = selections::resolve_remote(
        &endpoint,
        "farwork-agent-probe",
        "farwork-probe",
        env!("CARGO_PKG_VERSION"),
        cli.dsh_permission.as_deref(),
        cli.dsh_model.as_deref(),
        cli.dsh_preset.as_deref(),
    )
    .await
    .expect("resolve");
    let preset = selections.preset.clone().expect("a preset was asked for");
    println!("PROBE --dsh-preset ptc -> {preset}");
    assert!(!preset.is_empty());

    let connection = rpc::dial(&endpoint.address()).await.expect("dial");
    let client = DshClient::new(connection.client);
    let params = InitializeParams {
        auth_token: &endpoint.token,
        connector_id: "farwork-agent-probe",
        session_namespace: "farwork-probe",
        client_version: env!("CARGO_PKG_VERSION"),
    };
    client
        .initialize(&endpoint.address(), &params)
        .await
        .expect("initialize");
    let presets = client
        .list_agent_presets()
        .await
        .expect("catalog.listAgentPresets");
    println!("PROBE catalog.listAgentPresets count={}", presets.len());
    for entry in &presets {
        println!("PROBE preset id={:?} name={:?}", entry.id, entry.title);
    }
    assert!(
        presets.iter().any(|entry| entry.id == preset),
        "{preset} is not in catalog.listAgentPresets"
    );
    // Every short name the bridge itself advertises must round-trip.
    for entry in &presets {
        let by_id = selections::resolve(selections::Kind::Preset, &entry.id, &presets)
            .unwrap_or_else(|error| panic!("{} must resolve: {error}", entry.id));
        let by_name = selections::resolve(selections::Kind::Preset, &entry.title, &presets)
            .unwrap_or_else(|error| panic!("{} must resolve: {error}", entry.title));
        println!(
            "PROBE --dsh-preset {:?} -> {by_id} | by name {:?} -> {by_name}",
            entry.id, entry.title
        );
        assert_eq!(by_id, entry.id);
        assert_eq!(by_name, entry.id);
    }
    client.rpc().close().await;

    // The advisory an operator sees with no --dsh-preset at all.
    match selections::preset_hint_remote(
        &endpoint,
        "farwork-agent-probe",
        "farwork-probe",
        env!("CARGO_PKG_VERSION"),
    )
    .await
    {
        Some(hint) => println!("PROBE WARN {hint}"),
        None => println!("PROBE WARN <none: no presets or bridge unavailable>"),
    }
}

/// Subscribe-only probe: never creates a session, never runs a turn.
///
/// It exists to answer one question with the wire instead of a guess — what the
/// bridge actually sends after `runtime.sync.subscribe`, and how it asks for a
/// checkpoint — plus whether a full inventory replay overflows its own outbound
/// queue (the `sync.failed` / `queuedEvents: 10000` failure).
///
/// Frames are printed raw on purpose: the point is to read the peer's own words,
/// not this crate's interpretation of them. Env knobs:
///   DSH_PROBE_SECONDS         how long to listen (default 180)
///   DSH_PROBE_REQUEST_MODE    `result` answers inbound requests with
///                             {"checkpoint": null}; `none` answers -32601
///   DSH_PROBE_ACK_CHECKPOINT  `omit` (default) or `null`
///
///   cargo test -p farwork-agent --test dsh_live -- --ignored --nocapture subscribe_only
#[tokio::test]
#[ignore = "needs a running DSH bridge; subscribes but never creates a session"]
async fn subscribe_only() {
    use farwork_agent::dsh::handshake::InitializeParams;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpStream;

    fn brief(value: &serde_json::Value, limit: usize) -> String {
        let text = value.to_string();
        if text.chars().count() <= limit {
            return text;
        }
        format!(
            "{}… ({} chars)",
            text.chars().take(limit).collect::<String>(),
            text.chars().count()
        )
    }

    let env = |key: &str| std::env::var(key).ok();
    let endpoint = match discovery::resolve(None, &env) {
        Ok(located) => located.endpoint,
        Err(error) => {
            println!("PROBE SKIP: no DSH bridge endpoint ({error})");
            return;
        }
    };
    let seconds: u64 = env("DSH_PROBE_SECONDS")
        .and_then(|value| value.parse().ok())
        .unwrap_or(180);
    let request_mode = env("DSH_PROBE_REQUEST_MODE").unwrap_or_else(|| String::from("result"));
    let ack_checkpoint_mode =
        env("DSH_PROBE_ACK_CHECKPOINT").unwrap_or_else(|| String::from("omit"));
    let resubscribe_on_error = env("DSH_PROBE_RESUBSCRIBE_ON_ERROR").as_deref() == Some("1");
    println!(
        "PROBE endpoint {} pid={:?} seconds={seconds} request_mode={request_mode} ack_checkpoint={ack_checkpoint_mode}",
        endpoint.address(),
        endpoint.pid
    );

    // The real store, not a stand-in: this probe exists to prove the production
    // checkpoint path against the production bridge.
    let checkpoint_dir = env("DSH_PROBE_CHECKPOINT_DIR");
    let mut checkpoints = match &checkpoint_dir {
        Some(dir) => farwork_agent::dsh::sync::Checkpoints::open(dir),
        None => farwork_agent::dsh::sync::Checkpoints::in_memory(),
    };
    println!(
        "PROBE checkpoints dir={:?} loaded={}",
        checkpoints.dir(),
        checkpoints.len()
    );

    let stream = TcpStream::connect(endpoint.address()).await.expect("dial");
    let _ = stream.set_nodelay(true);
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    let params = InitializeParams {
        auth_token: &endpoint.token,
        connector_id: "farwork-agent-probe",
        session_namespace: "farwork-probe",
        client_version: env!("CARGO_PKG_VERSION"),
    };
    let initialize = serde_json::json!({
        "jsonrpc": "2.0", "id": "probe-init", "method": "initialize", "params": params.to_value(),
    });
    write
        .write_all(format!("{initialize}\n").as_bytes())
        .await
        .expect("write initialize");

    let started = std::time::Instant::now();
    let mut seq = 0u32;
    let mut batches = 0u32;
    let mut acked = 0u32;
    let mut requests = 0u32;
    let mut checkpoint_ops = 0u32;
    let mut operations_seen: std::collections::BTreeMap<String, u32> =
        std::collections::BTreeMap::new();
    let mut subscribed = false;
    let mut errors = 0u32;
    let mut resubscribes = 0u32;

    loop {
        let left = std::time::Duration::from_secs(seconds).saturating_sub(started.elapsed());
        if left.is_zero() {
            println!(
                "PROBE DONE elapsed={}s: time limit reached",
                started.elapsed().as_secs()
            );
            break;
        }
        let line = match tokio::time::timeout(left, lines.next_line()).await {
            Err(_) => {
                println!(
                    "PROBE DONE elapsed={}s: time limit reached",
                    started.elapsed().as_secs()
                );
                break;
            }
            Ok(Err(error)) => {
                println!(
                    "PROBE STOP elapsed={}s: read failed: {error}",
                    started.elapsed().as_secs()
                );
                break;
            }
            Ok(Ok(None)) => {
                println!(
                    "PROBE STOP elapsed={}s: bridge closed the socket",
                    started.elapsed().as_secs()
                );
                break;
            }
            Ok(Ok(Some(line))) => line,
        };
        if line.trim().is_empty() {
            continue;
        }
        let frame: serde_json::Value = match serde_json::from_str(&line) {
            Ok(frame) => frame,
            Err(error) => {
                println!(
                    "PROBE RAW unparsable ({error}): {}",
                    brief(&serde_json::Value::String(line.clone()), 300)
                );
                continue;
            }
        };
        seq += 1;
        let method = frame
            .get("method")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        let id = frame.get("id").cloned().filter(|id| !id.is_null());

        if let Some(error) = frame.get("error") {
            errors += 1;
            println!(
                "PROBE [{seq}] ERROR elapsed={}s id={:?} {}",
                started.elapsed().as_secs(),
                id,
                brief(error, 600)
            );
            continue;
        }
        if !method.is_empty() && id.is_some() {
            requests += 1;
            println!(
                "PROBE [{seq}] REQUEST elapsed={}s id={:?} method={method} params={}",
                started.elapsed().as_secs(),
                id,
                brief(frame.get("params").unwrap_or(&serde_json::Value::Null), 900)
            );
            let answer = if request_mode == "none" {
                serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": format!("method not found: {method}") },
                })
            } else {
                serde_json::json!({
                    "jsonrpc": "2.0", "id": id, "result": { "checkpoint": serde_json::Value::Null },
                })
            };
            write
                .write_all(format!("{answer}\n").as_bytes())
                .await
                .expect("write answer");
            println!("PROBE [{seq}] ANSWERED {answer}");
            continue;
        }
        if method.is_empty() {
            // A response to one of ours.
            println!(
                "PROBE [{seq}] RESULT elapsed={}s id={:?} {}",
                started.elapsed().as_secs(),
                id,
                brief(frame.get("result").unwrap_or(&serde_json::Value::Null), 900)
            );
            if id.as_ref().and_then(|id| id.as_str()) == Some("probe-init") {
                let subscribe = serde_json::json!({
                    "jsonrpc": "2.0", "id": "probe-sub", "method": "runtime.sync.subscribe",
                    "params": {
                        "subscriptionId": uuid::Uuid::new_v4().simple().to_string(),
                        "checkpointVersion": 1,
                        "uploadProgressVersion": 1,
                    },
                });
                write
                    .write_all(format!("{subscribe}\n").as_bytes())
                    .await
                    .expect("write subscribe");
                println!("PROBE [{seq}] SENT subscribe {subscribe}");
            }
            continue;
        }
        if method == "runtime.error" {
            errors += 1;
            println!(
                "PROBE [{seq}] NOTIFY elapsed={}s {method} {}",
                started.elapsed().as_secs(),
                brief(&frame, 1200)
            );
            // The agent answers a dead sync feed with a bounded resubscribe, on the
            // bet that the checkpoints it just persisted let the bridge resume. This
            // switch runs exactly that, so the bet can be checked against the real
            // bridge instead of only in a scripted unit test.
            let scope = frame
                .get("params")
                .and_then(|params| params.get("data"))
                .and_then(|data| data.get("scope"))
                .and_then(|scope| scope.as_str())
                .unwrap_or_default();
            if resubscribe_on_error && scope == "sync" {
                resubscribes += 1;
                let subscribe = serde_json::json!({
                    "jsonrpc": "2.0", "id": format!("probe-sub2-{resubscribes}"),
                    "method": "runtime.sync.subscribe",
                    "params": {
                        "subscriptionId": uuid::Uuid::new_v4().simple().to_string(),
                        "checkpointVersion": 1,
                        "uploadProgressVersion": 1,
                    },
                });
                write
                    .write_all(format!("{subscribe}\n").as_bytes())
                    .await
                    .expect("write resubscribe");
                println!(
                    "PROBE [{seq}] RESUBSCRIBE #{resubscribes} loaded={} {subscribe}",
                    checkpoints.len()
                );
            }
            continue;
        }
        if method != "runtime.sync.batch" {
            println!(
                "PROBE [{seq}] NOTIFY elapsed={}s {method} {}",
                started.elapsed().as_secs(),
                brief(&frame, 1200)
            );
            continue;
        }

        let params = frame
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let stream_id = params
            .get("streamId")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        let batch_seq = params
            .get("batchSeq")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        batches += 1;
        let operations = params
            .get("operations")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        let mut kinds: Vec<String> = Vec::new();
        let mut ack_checkpoint: Option<serde_json::Value> = None;
        for operation in &operations {
            let kind = operation
                .get("kind")
                .and_then(|value| value.as_str())
                .unwrap_or("?")
                .to_string();
            *operations_seen.entry(kind.clone()).or_insert(0) += 1;
            let external = operation
                .get("externalSessionId")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            match kind.as_str() {
                "checkpoint.load" => {
                    checkpoint_ops += 1;
                    let found = checkpoints.load(external);
                    println!(
                        "PROBE [{seq}] CHECKPOINT-LOAD elapsed={}s batch={batch_seq} external={external} found={}",
                        started.elapsed().as_secs(),
                        found.is_some()
                    );
                    ack_checkpoint = Some(found.unwrap_or(serde_json::Value::Null));
                }
                "checkpoint.save" => {
                    checkpoint_ops += 1;
                    if let Some(value) = operation.get("checkpoint") {
                        checkpoints.save(external, value.clone());
                        println!(
                            "PROBE [{seq}] CHECKPOINT-SAVE elapsed={}s batch={batch_seq} external={external} {}",
                            started.elapsed().as_secs(),
                            brief(value, 400)
                        );
                    }
                }
                "checkpoint.delete" => {
                    checkpoint_ops += 1;
                    checkpoints.delete(external);
                    println!(
                        "PROBE [{seq}] CHECKPOINT-DELETE elapsed={}s batch={batch_seq} external={external}",
                        started.elapsed().as_secs()
                    );
                }
                _ => {}
            }
            if kind == "snapshot.begin" && kinds.len() < 6 {
                println!(
                    "PROBE [{seq}] SNAPSHOT-BEGIN elapsed={}s batch={batch_seq} {}",
                    started.elapsed().as_secs(),
                    brief(operation, 700)
                );
            }
            kinds.push(kind);
        }
        if batches <= 12 {
            println!(
                "PROBE [{seq}] BATCH elapsed={}s stream={stream_id} seq={batch_seq} projection={:?} kinds={kinds:?}",
                started.elapsed().as_secs(),
                params.get("projectionVersion"),
            );
        }
        if !subscribed {
            subscribed = true;
        }
        let mut ack = serde_json::json!({
            "streamId": stream_id, "batchSeq": batch_seq,
        });
        match ack_checkpoint {
            Some(checkpoint) => ack["checkpoint"] = checkpoint,
            None if ack_checkpoint_mode == "null" => ack["checkpoint"] = serde_json::Value::Null,
            None => {}
        }
        let ack_frame = serde_json::json!({
            "jsonrpc": "2.0", "id": format!("probe-ack-{batch_seq}"), "method": "runtime.sync.ack",
            "params": ack,
        });
        write
            .write_all(format!("{ack_frame}\n").as_bytes())
            .await
            .expect("write ack");
        acked += 1;
        if batches.is_multiple_of(50) {
            println!(
                "PROBE [{seq}] PROGRESS elapsed={}s batches={batches} acked={acked} requests={requests} checkpoint_ops={checkpoint_ops} errors={errors}",
                started.elapsed().as_secs()
            );
        }
    }

    println!(
        "PROBE SUMMARY elapsed={}s frames={seq} batches={batches} acked={acked} inbound_requests={requests} checkpoint_ops={checkpoint_ops} errors={errors} resubscribes={resubscribes}",
        started.elapsed().as_secs()
    );
    println!("PROBE OPERATIONS {operations_seen:?}");
    let _ = write.shutdown().await;
}
