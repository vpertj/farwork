//! Browser stand-in for manual smoke tests: watch devices, open a terminal on
//! the first connected device, run a command and print what comes back.
//!
//! cargo run -p farwork-server --example smoke_client -- --command "echo hi"

use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use farwork_protocol::{ClientToServer, DeviceView, ServerToClient};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

type Error = Box<dyn std::error::Error + Send + Sync>;
type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const PROMPT_QUIET: Duration = Duration::from_millis(700);

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut url = "ws://127.0.0.1:7300/ws/client".to_string();
    let mut command = "echo hi".to_string();
    // The PTY echoes what we type, so a plain substring check would match our
    // own keystrokes; require a whole output line equal to the expected value.
    let mut expected = "hi".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--url" => url = args.next().ok_or("--url needs a value")?,
            "--command" => command = args.next().ok_or("--command needs a value")?,
            "--expect" => expected = args.next().ok_or("--expect needs a value")?,
            other => return Err(format!("unexpected argument: {other}").into()),
        }
    }
    if let Ok(token) = std::env::var("FARWORK_TOKEN") {
        if !token.is_empty() {
            let separator = if url.contains('?') { '&' } else { '?' };
            url = format!("{url}{separator}token={token}");
        }
    }

    let (mut socket, _) = connect_async(url.as_str()).await?;
    println!("[smoke] connected to {url}");
    send(&mut socket, &ClientToServer::DevicesWatch).await;

    let device = wait_for_connected_device(&mut socket).await?;
    println!(
        "[smoke] device {} ({}) is connected",
        device.name, device.id
    );

    let stream_id = Uuid::new_v4().to_string();
    send(
        &mut socket,
        &ClientToServer::TerminalOpen {
            device_id: device.id.clone(),
            stream_id: stream_id.clone(),
            cols: 80,
            rows: 24,
        },
    )
    .await;
    println!("[smoke] terminal.open sent for stream {stream_id}");

    wait_for_opened(&mut socket, &stream_id).await?;
    println!("[smoke] terminal.opened received");

    // An interactive shell drops keystrokes sent before it owns the line
    // editor, so wait for the prompt to stop streaming first.
    let mut transcript = String::new();
    pump(
        &mut socket,
        &stream_id,
        &mut transcript,
        None,
        Instant::now() + Duration::from_secs(15),
        Some(PROMPT_QUIET),
    )
    .await?;

    let payload = STANDARD.encode(format!("{command}\n"));
    let mut matched = false;
    for attempt in 1..=2 {
        send(
            &mut socket,
            &ClientToServer::TerminalInput {
                stream_id: stream_id.clone(),
                data: payload.clone(),
            },
        )
        .await;
        println!("[smoke] terminal.input sent (attempt {attempt}): {command}");
        pump(
            &mut socket,
            &stream_id,
            &mut transcript,
            Some(&expected),
            Instant::now() + Duration::from_secs(10),
            None,
        )
        .await?;
        matched = has_output_line(&transcript, &expected);
        if matched {
            break;
        }
    }

    send(
        &mut socket,
        &ClientToServer::TerminalClose {
            stream_id: stream_id.clone(),
        },
    )
    .await;

    if !matched {
        return Err(format!("terminal never printed the line \"{expected}\" ").into());
    }
    println!("[smoke] OK: terminal printed the line \"{expected}\"");
    Ok(())
}

fn has_output_line(transcript: &str, expected: &str) -> bool {
    transcript
        .lines()
        .any(|line| line.trim() == expected.trim())
}

async fn send(socket: &mut Ws, message: &ClientToServer) {
    let _ = socket.send(Message::Text(message.to_json().into())).await;
}

/// Read terminal frames until `needle` shows up, `quiet` elapses without data,
/// or `deadline` passes.
async fn pump(
    socket: &mut Ws,
    stream_id: &str,
    transcript: &mut String,
    needle: Option<&str>,
    deadline: Instant,
    quiet: Option<Duration>,
) -> Result<(), Error> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        let wait = quiet.map_or(remaining, |quiet| quiet.min(remaining));
        match tokio::time::timeout(wait, socket.next()).await {
            Err(_) => return Ok(()),
            Ok(None) => return Ok(()),
            Ok(Some(Err(error))) => return Err(error.into()),
            Ok(Some(Ok(frame))) => {
                let Message::Text(text) = frame else { continue };
                match ServerToClient::parse(text.as_str())? {
                    ServerToClient::TerminalData {
                        stream_id: received,
                        data,
                    } if received == stream_id => {
                        let bytes = STANDARD.decode(&data)?;
                        let chunk = String::from_utf8_lossy(&bytes).to_string();
                        print!("{chunk}");
                        transcript.push_str(&chunk);
                        if needle.is_some_and(|needle| has_output_line(transcript, needle)) {
                            return Ok(());
                        }
                    }
                    ServerToClient::TerminalError { message, .. } => {
                        return Err(format!("terminal.error: {message}").into())
                    }
                    ServerToClient::TerminalExit { code, .. } => {
                        println!("[smoke] terminal.exit code={code}");
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn next_client_frame(socket: &mut Ws) -> Result<ServerToClient, Error> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .map_err(|_| "timed out waiting for a server frame")?
            .ok_or("server closed the connection")??;
        let Message::Text(text) = frame else { continue };
        return Ok(ServerToClient::parse(text.as_str())?);
    }
}

async fn wait_for_connected_device(socket: &mut Ws) -> Result<DeviceView, Error> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let ServerToClient::Devices { devices } = next_client_frame(socket).await? {
            println!("[smoke] devices push: {} entries", devices.len());
            if let Some(device) = devices.into_iter().find(|device| device.connected) {
                return Ok(device);
            }
        }
    }
    Err("no connected device appeared".into())
}

async fn wait_for_opened(socket: &mut Ws, stream_id: &str) -> Result<(), Error> {
    loop {
        match next_client_frame(socket).await? {
            ServerToClient::TerminalOpened {
                stream_id: received,
            } if received == stream_id => return Ok(()),
            ServerToClient::TerminalError {
                stream_id: received,
                message,
            } if received == stream_id => {
                return Err(format!("server refused the stream: {message}").into())
            }
            _ => {}
        }
    }
}
