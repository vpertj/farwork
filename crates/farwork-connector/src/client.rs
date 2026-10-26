use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use farwork_protocol::{ConnectorToServer, DeviceInfo, ServerToConnector};
use futures_util::{SinkExt, Stream, StreamExt};
use tokio::sync::mpsc;
use tokio::time::interval;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::cli::Config;
use crate::pty::TerminalPool;

type Error = Box<dyn std::error::Error + Send + Sync>;

pub const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(25);

/// Connect once and serve until the socket closes. Returns only on a clean end;
/// any failure is reported by the caller as the trigger for a reconnect.
pub async fn run_session(
    config: &Config,
    device: &DeviceInfo,
    backoff: &mut Duration,
) -> Result<(), Error> {
    let url = config.url();
    let (socket, _response) = connect_async(url.as_str()).await?;
    tracing::info!(server = %config.server, "connected to server");

    let (mut sink, stream) = socket.split();
    let (outgoing, mut inbox) = mpsc::unbounded_channel::<ConnectorToServer>();
    let writer = tokio::spawn(async move {
        while let Some(message) = inbox.recv().await {
            let payload = message.to_json();
            if sink.send(Message::Text(payload.into())).await.is_err() {
                break;
            }
        }
    });

    let pool = Arc::new(TerminalPool::new(outgoing.clone()));
    let _ = outgoing.send(ConnectorToServer::Hello {
        device: device.clone(),
    });

    let heartbeat = {
        let outgoing = outgoing.clone();
        tokio::spawn(async move {
            let mut ticker = interval(HEARTBEAT_INTERVAL);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if outgoing.send(ConnectorToServer::Ping).is_err() {
                    break;
                }
            }
        })
    };

    let outcome = read_loop(stream, &pool, &outgoing, backoff).await;

    heartbeat.abort();
    pool.close_all();
    drop(outgoing);
    writer.abort();
    outcome
}

async fn read_loop<S>(
    mut stream: S,
    pool: &TerminalPool,
    outgoing: &mpsc::UnboundedSender<ConnectorToServer>,
    backoff: &mut Duration,
) -> Result<(), Error>
where
    S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(frame) = stream.next().await {
        let frame = frame?;
        let text = match frame {
            Message::Text(text) => text,
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {
                continue
            }
        };

        match ServerToConnector::parse(&text) {
            Ok(ServerToConnector::Welcome { server_version }) => {
                tracing::info!(%server_version, "server accepted this connector");
                *backoff = INITIAL_BACKOFF;
            }
            Ok(ServerToConnector::TerminalOpen {
                stream_id,
                cols,
                rows,
            }) => {
                let _ = outgoing.send(ConnectorToServer::TerminalOpened {
                    stream_id: stream_id.clone(),
                });
                pool.open(stream_id, cols, rows);
            }
            Ok(ServerToConnector::TerminalData { stream_id, data }) => match STANDARD.decode(&data)
            {
                Ok(bytes) => pool.write(&stream_id, bytes),
                Err(error) => tracing::warn!(%error, "server sent invalid base64"),
            },
            Ok(ServerToConnector::TerminalResize {
                stream_id,
                cols,
                rows,
            }) => pool.resize(&stream_id, cols, rows),
            Ok(ServerToConnector::TerminalClose { stream_id }) => {
                pool.close(&stream_id);
                let _ = outgoing.send(ConnectorToServer::TerminalExit { stream_id, code: 0 });
            }
            Ok(ServerToConnector::Ping) => {
                let _ = outgoing.send(ConnectorToServer::Pong);
            }
            Ok(ServerToConnector::Pong) => {}
            Err(error) => tracing::warn!(%error, "dropping malformed server frame"),
        }
    }
    Ok(())
}
