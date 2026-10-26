//! Real-binary smoke test for `farwork-agent pair` against a local stub hub.
//!
//! The hub in this workspace has no /api/v2/pairing/* yet, so the stub speaks the
//! same contract (claim + connector ingress) and the assertions stay on the parts
//! this crate owns: agent.json contents and mode, the claim body, and the token
//! the connector actually dials with. Once the hub implements pairing, this test
//! is still the regression net for the agent half.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

const CLAIM_TOKEN: &str = "cxt_smoke_0000000000";
const CLAIM_CODE: &str = "4F2K-9QXM";

#[tokio::test]
async fn pair_saves_credentials_then_a_bare_restart_reuses_them() {
    let outcome = tokio::time::timeout(Duration::from_secs(90), scenario()).await;
    outcome.expect("pair smoke test timed out");
}

async fn scenario() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hub = format!("http://{addr}");
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let stub = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let head = peek_head(&stream).await;
            if head.starts_with("POST ") {
                let request = respond_claim(stream).await;
                let _ = tx.send(format!("claim {request}"));
            } else {
                let (request_line, hello) = respond_ws(stream).await;
                let _ = tx.send(format!("ws {request_line}"));
                let _ = tx.send(format!("hello {hello}"));
            }
        }
    });

    let home = temp_home();
    let workdir = home.join("work");
    std::fs::create_dir_all(&workdir).unwrap();

    // 1. pair: redeem the code, then connect with what was just saved.
    let paired = run_agent(
        &home,
        &[
            "pair",
            "--hub",
            &hub,
            "--code",
            "4f2k-9qxm",
            "--name",
            "smoke-mac",
            "--runtime",
            "echo",
            "--workdir",
            workdir.to_str().unwrap(),
            "--once",
        ],
    )
    .await;
    let paired_stderr = String::from_utf8_lossy(&paired.stderr).to_string();
    assert_eq!(
        paired.status.code(),
        Some(0),
        "pair failed: {paired_stderr}"
    );
    assert!(
        paired_stderr.contains("pairing code redeemed"),
        "{paired_stderr}"
    );
    assert!(
        paired_stderr.contains("hub welcome received"),
        "pair must start the connector too: {paired_stderr}"
    );
    // 5. the token is a secret: it may be stored and sent, never logged.
    assert!(
        !paired_stderr.contains(CLAIM_TOKEN),
        "connector token leaked into the log: {paired_stderr}"
    );

    // 1. agent.json holds hub + connector id + token, owner-only.
    let identity_file = home.join("agent.json");
    let raw = std::fs::read_to_string(&identity_file).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(saved["hub"], hub);
    assert_eq!(saved["connectorId"], "cn_smoke");
    assert_eq!(saved["connectorToken"], CLAIM_TOKEN);
    assert_eq!(saved["name"], "smoke-mac");
    assert!(saved["agentId"].as_str().unwrap().starts_with("agt_"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&identity_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "agent.json mode was {mode:o}");
    }

    // 2. no credentials on the command line: hub + token come from the file.
    let restarted = run_agent(
        &home,
        &[
            "--runtime",
            "echo",
            "--workdir",
            workdir.to_str().unwrap(),
            "--once",
        ],
    )
    .await;
    let restarted_stderr = String::from_utf8_lossy(&restarted.stderr).to_string();
    assert_eq!(
        restarted.status.code(),
        Some(0),
        "bare restart failed: {restarted_stderr}"
    );
    assert!(
        restarted_stderr.contains("hub welcome received"),
        "{restarted_stderr}"
    );

    stub.abort();

    let mut events = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
        events.push(event);
    }
    let claims: Vec<&String> = events
        .iter()
        .filter(|event| event.starts_with("claim "))
        .collect();
    assert_eq!(claims.len(), 1, "exactly one claim: {events:?}");
    let claim = claims[0];
    assert!(
        claim.starts_with("claim POST /api/v2/pairing/claim "),
        "{claim}"
    );
    let code_key = format!(r#""code":"{CLAIM_CODE}""#);
    assert!(claim.contains(&code_key), "{claim}");
    assert!(claim.contains(r#""name":"smoke-mac""#), "{claim}");
    assert!(claim.contains(r#""platform":""#), "{claim}");

    let sockets: Vec<&String> = events
        .iter()
        .filter(|event| event.starts_with("ws "))
        .collect();
    assert_eq!(sockets.len(), 2, "pair and restart each dialed: {events:?}");
    for socket in sockets {
        assert!(socket.contains("/api/v2/connector/ws?token="), "{socket}");
        assert!(
            socket.contains(CLAIM_TOKEN),
            "the stored token must be the one dialed: {socket}"
        );
    }
    let hellos = events
        .iter()
        .filter(|event| event.starts_with("hello "))
        .count();
    assert_eq!(hellos, 2, "each connection says hello: {events:?}");

    let _ = std::fs::remove_dir_all(&home);
}

fn temp_home() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home =
        std::env::temp_dir().join(format!("farwork-pair-smoke-{}-{stamp}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    home
}

async fn run_agent(home: &Path, args: &[&str]) -> std::process::Output {
    tokio::process::Command::new(env!("CARGO_BIN_EXE_farwork-agent"))
        .args(args)
        .env("FARWORK_HOME", home)
        // The token must come from --token or agent.json, never from the parent
        // shell: a stale value would make this test pass for the wrong reason.
        .env_remove("FARWORK_CONNECTOR_TOKEN")
        .output()
        .await
        .expect("farwork-agent failed to start")
}

/// Read the request head without consuming it, so the same bytes can still be
/// upgraded to a websocket afterwards.
async fn peek_head(stream: &TcpStream) -> String {
    let mut buf = [0u8; 4096];
    for _ in 0..200 {
        if let Ok(read) = stream.peek(&mut buf).await {
            if read > 0 {
                let text = String::from_utf8_lossy(&buf[..read]).to_string();
                if text.contains("\r\n") {
                    return text;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    String::from_utf8_lossy(&buf).to_string()
}

async fn respond_claim(mut stream: TcpStream) -> String {
    let request = read_request(&mut stream).await;
    let body = format!(
        concat!(
            r#"{{"connector":{{"id":"cn_smoke","name":"smoke-mac","deviceOs":"macos","#,
            r#""status":"offline"}},"connectorToken":"{}","#,
            r#""serverTime":"2026-01-01T00:00:00Z"}}"#
        ),
        CLAIM_TOKEN
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    request
}

async fn read_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .expect("claim request timed out")
            .unwrap();
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&request).to_string();
        if let Some(head_end) = text.find("\r\n\r\n") {
            let declared = text[..head_end]
                .to_ascii_lowercase()
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if request.len() >= head_end + 4 + declared {
                break;
            }
        }
    }
    String::from_utf8_lossy(&request).to_string()
}

async fn respond_ws(stream: TcpStream) -> (String, String) {
    let head = peek_head(&stream).await;
    let request_line = head.lines().next().unwrap_or_default().to_string();
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    let hello = match tokio::time::timeout(Duration::from_secs(15), socket.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => text.to_string(),
        other => panic!("expected a hello frame, got {other:?}"),
    };
    socket
        .send(Message::Text(
            r#"{"type":"welcome","serverVersion":"0.1.0","heartbeatSecs":15}"#.into(),
        ))
        .await
        .unwrap();
    // --once exits when the socket ends, so the stub closes after the handshake.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let _ = socket.close(None).await;
    (request_line, hello)
}
