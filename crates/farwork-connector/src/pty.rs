use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use farwork_protocol::ConnectorToServer;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::sync::mpsc::UnboundedSender;

type Error = Box<dyn std::error::Error + Send + Sync>;
type CommandSender = std::sync::mpsc::Sender<TerminalCommand>;

enum TerminalCommand {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Close,
}

/// Live PTY-backed terminals for one connector session.
pub struct TerminalPool {
    terminals: Arc<Mutex<HashMap<String, CommandSender>>>,
    outgoing: UnboundedSender<ConnectorToServer>,
}

impl TerminalPool {
    pub fn new(outgoing: UnboundedSender<ConnectorToServer>) -> Self {
        Self {
            terminals: Arc::new(Mutex::new(HashMap::new())),
            outgoing,
        }
    }

    /// Spawn `$SHELL` on a fresh PTY and start streaming its output.
    pub fn open(&self, stream_id: String, cols: u16, rows: u16) {
        let terminals = Arc::clone(&self.terminals);
        let outgoing = self.outgoing.clone();
        let spawned_id = stream_id.clone();

        std::thread::spawn(move || {
            match spawn_terminal(
                &spawned_id,
                cols,
                rows,
                outgoing.clone(),
                Arc::clone(&terminals),
            ) {
                Ok(commands) => lock_terminals(&terminals).insert(spawned_id, commands),
                Err(error) => {
                    tracing::error!(%error, stream_id = %spawned_id, "failed to start terminal");
                    let _ = outgoing.send(ConnectorToServer::TerminalExit {
                        stream_id: spawned_id,
                        code: -1,
                    });
                    None
                }
            };
        });
    }

    pub fn write(&self, stream_id: &str, bytes: Vec<u8>) {
        self.send(stream_id, TerminalCommand::Data(bytes));
    }

    pub fn resize(&self, stream_id: &str, cols: u16, rows: u16) {
        self.send(stream_id, TerminalCommand::Resize { cols, rows });
    }

    pub fn close(&self, stream_id: &str) {
        self.send(stream_id, TerminalCommand::Close);
    }

    /// Tear down every terminal; called when the session ends.
    pub fn close_all(&self) {
        let mut terminals = lock_terminals(&self.terminals);
        for (_, commands) in terminals.drain() {
            let _ = commands.send(TerminalCommand::Close);
        }
    }

    fn send(&self, stream_id: &str, command: TerminalCommand) {
        let terminals = lock_terminals(&self.terminals);
        if let Some(commands) = terminals.get(stream_id) {
            let _ = commands.send(command);
        }
    }
}

fn lock_terminals(
    terminals: &Arc<Mutex<HashMap<String, CommandSender>>>,
) -> std::sync::MutexGuard<'_, HashMap<String, CommandSender>> {
    terminals
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn spawn_terminal(
    stream_id: &str,
    cols: u16,
    rows: u16,
    outgoing: UnboundedSender<ConnectorToServer>,
    terminals: Arc<Mutex<HashMap<String, CommandSender>>>,
) -> Result<CommandSender, Error> {
    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut command = CommandBuilder::new(default_shell());
    if let Ok(cwd) = std::env::current_dir() {
        command.cwd(cwd);
    }
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let master = pair.master;
    let (commands, inbox) = std::sync::mpsc::channel();

    let read_stream = stream_id.to_string();
    let read_outgoing = outgoing.clone();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let frame = ConnectorToServer::TerminalData {
                        stream_id: read_stream.clone(),
                        data: STANDARD.encode(&buffer[..read]),
                    };
                    if read_outgoing.send(frame).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "pty read ended");
                    break;
                }
            }
        }
        // Dropping the command sender makes the writer thread kill the shell.
        lock_terminals(&terminals).remove(&read_stream);
        let _ = read_outgoing.send(ConnectorToServer::TerminalExit {
            stream_id: read_stream,
            code: 0,
        });
    });

    std::thread::spawn(move || {
        while let Ok(command) = inbox.recv() {
            match command {
                TerminalCommand::Data(bytes) => {
                    if let Err(error) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                        tracing::debug!(%error, "pty write failed");
                        break;
                    }
                }
                TerminalCommand::Resize { cols, rows } => {
                    if let Err(error) = master.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    }) {
                        tracing::debug!(%error, "pty resize failed");
                    }
                }
                TerminalCommand::Close => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    });

    Ok(commands)
}

fn default_shell() -> String {
    let variable = if cfg!(windows) { "COMSPEC" } else { "SHELL" };
    std::env::var(variable)
        .ok()
        .filter(|shell| !shell.trim().is_empty())
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "cmd.exe".to_string()
            } else {
                "/bin/sh".to_string()
            }
        })
}
