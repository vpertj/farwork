//! PTY-backed terminals behind the `terminal.*` ingress frames.
//!
//! The hub mints `terminalId` and addresses one terminal per id; this side owns
//! one shell process per id and streams its output as base64 `terminal.data`.
//! Everything here is blocking on purpose: `portable-pty` read/write are
//! blocking syscalls, so each terminal gets two OS threads (one pumps the master
//! out, one owns the child plus its control channel) instead of parking tokio
//! workers in front of a shell prompt.
//!
//! Bytes stay bytes: output is encoded straight out of the read buffer and input
//! is decoded straight into the master. Nothing on this path assumes UTF-8.
//!
//! The `--workdir` sandbox bounds where a terminal *starts*, not what the shell
//! can reach afterwards: a shell is a shell. `terminal.open.cwd` is therefore
//! checked with the same canonicalize + component-prefix test the file browser
//! uses, and a refusal is reported as `terminal.error`, never as silence.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{mpsc as std_mpsc, Arc, Mutex, MutexGuard};
use std::thread;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc::UnboundedSender;

use crate::fsops::{FsFailure, Sandbox};
use crate::protocol::{ClientFrame, FsErrorCode};

/// Largest cols/rows accepted. A PTY happily takes 60000 columns and then hands
/// the browser a screen nothing can render, so the bound is a refusal instead.
pub const MAX_DIMENSION: u16 = 2000;

/// One read from the master. A burst of output becomes a handful of frames
/// rather than one frame per byte.
pub const READ_CHUNK: usize = 8192;

/// Failure codes the agent may put on a `terminal.error`. Closed set: the hub
/// relays the code verbatim, so an invented one would be unactionable.
pub const PERMISSION_DENIED: &str = "permission_denied";
pub const INVALID_SIZE: &str = "invalid_size";
pub const INVALID_TERMINAL: &str = "invalid_terminal";
pub const ALREADY_OPEN: &str = "already_open";
pub const SPAWN_FAILED: &str = "spawn_failed";

/// A `terminal.open` the agent refused, in the vocabulary the hub relays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalFailure {
    pub code: &'static str,
    pub message: String,
}

impl TerminalFailure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn from_fs(failure: FsFailure) -> Self {
        Self::new(fs_code(failure.code), failure.message)
    }

    /// The `terminal.error` frame this refusal turns into.
    pub fn frame(&self, terminal_id: &str) -> ClientFrame {
        ClientFrame::TerminalError {
            terminal_id: terminal_id.to_string(),
            message: self.message.clone(),
            code: self.code.to_string(),
        }
    }
}

fn fs_code(code: FsErrorCode) -> &'static str {
    match code {
        FsErrorCode::NotFound => "not_found",
        FsErrorCode::PermissionDenied => PERMISSION_DENIED,
        FsErrorCode::IoError => "io_error",
        FsErrorCode::TooLarge => "too_large",
    }
}

/// One `terminal.open`. `cwd` absent means "the agent workdir".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSpec {
    pub terminal_id: String,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<String>,
    pub shell: Option<String>,
}

impl TerminalSpec {
    pub fn new(terminal_id: impl Into<String>, cols: u16, rows: u16) -> Self {
        Self {
            terminal_id: terminal_id.into(),
            cols,
            rows,
            cwd: None,
            shell: None,
        }
    }
}

enum Command {
    Data(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    /// The reader saw EOF: the shell ended on its own, reap it and report why.
    Exited,
    /// The hub asked to close: kill the shell now.
    Shutdown,
}

/// Every live terminal of this agent, keyed by the hub-minted id.
pub struct TerminalHost {
    terminals: Arc<Mutex<HashMap<String, std_mpsc::Sender<Command>>>>,
    /// `--shell`, overridable per terminal by `terminal.open.shell`.
    shell: Option<String>,
    /// `None` when `--workdir` cannot be resolved: every open is refused rather
    /// than defaulting to an unbounded cwd.
    sandbox: Option<Arc<Sandbox>>,
}

impl TerminalHost {
    pub fn new(sandbox: Option<Arc<Sandbox>>, shell: Option<String>) -> Self {
        Self {
            terminals: Arc::new(Mutex::new(HashMap::new())),
            shell,
            sandbox,
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, std_mpsc::Sender<Command>>> {
        match self.terminals.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Spawn the shell and start streaming. `out` is this connection's frame
    /// channel; every terminal dies with the connection it was opened on.
    pub fn open(
        &self,
        spec: TerminalSpec,
        out: UnboundedSender<ClientFrame>,
    ) -> Result<(), TerminalFailure> {
        let terminal_id = spec.terminal_id.trim().to_string();
        if terminal_id.is_empty() {
            return Err(TerminalFailure::new(
                INVALID_TERMINAL,
                "terminalId must not be empty",
            ));
        }
        let size = checked_size(spec.cols, spec.rows)?;
        let cwd = self.resolve_cwd(spec.cwd.as_deref())?;
        let shell = shell_command(spec.shell.as_deref(), self.shell.as_deref());
        if self.lock().contains_key(&terminal_id) {
            return Err(TerminalFailure::new(
                ALREADY_OPEN,
                format!("terminal {terminal_id} is already open"),
            ));
        }

        let pair = native_pty_system().openpty(size).map_err(|error| {
            TerminalFailure::new(SPAWN_FAILED, format!("cannot allocate a pty: {error}"))
        })?;
        let mut command = CommandBuilder::new(&shell);
        command.cwd(&cwd);
        // Without TERM, programs decide they are on a dumb terminal and strip
        // colour and cursor control the browser would have rendered fine.
        command.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(command).map_err(|error| {
            TerminalFailure::new(
                SPAWN_FAILED,
                format!("cannot start {shell} in {}: {error}", cwd.display()),
            )
        })?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(|error| {
            TerminalFailure::new(SPAWN_FAILED, format!("cannot read the pty: {error}"))
        })?;
        let writer = pair.master.take_writer().map_err(|error| {
            TerminalFailure::new(SPAWN_FAILED, format!("cannot write the pty: {error}"))
        })?;

        let (commands, inbox) = std_mpsc::channel::<Command>();
        self.lock().insert(terminal_id.clone(), commands.clone());
        tracing::info!(
            %terminal_id,
            shell = %shell,
            cwd = %cwd.display(),
            cols = spec.cols,
            rows = spec.rows,
            pid = child.process_id().unwrap_or(0),
            "terminal started"
        );

        let terminals = Arc::clone(&self.terminals);
        let exit_id = terminal_id.clone();
        let exit_out = out.clone();
        thread::spawn(move || {
            supervise(
                exit_id,
                child,
                pair.master,
                writer,
                inbox,
                exit_out,
                terminals,
            )
        });
        let pump_id = terminal_id.clone();
        thread::spawn(move || pump(pump_id, reader, out, commands));

        Ok(())
    }

    /// `terminal.opened`, sent once the shell exists.
    pub fn opened_frame(terminal_id: &str) -> ClientFrame {
        ClientFrame::TerminalOpened {
            terminal_id: terminal_id.to_string(),
        }
    }

    pub fn write(&self, terminal_id: &str, bytes: Vec<u8>) -> bool {
        self.send(terminal_id, Command::Data(bytes))
    }

    pub fn resize(&self, terminal_id: &str, cols: u16, rows: u16) -> bool {
        self.send(terminal_id, Command::Resize { cols, rows })
    }

    /// Kill one terminal. `false` for an id this agent does not know.
    pub fn close(&self, terminal_id: &str) -> bool {
        let sender = self.lock().remove(terminal_id);
        match sender {
            Some(sender) => sender.send(Command::Shutdown).is_ok(),
            None => false,
        }
    }

    /// Kill every terminal. Called when the connector socket ends: nobody is
    /// reading the output any more, and an orphaned shell is a process leak.
    pub fn close_all(&self) {
        let senders: Vec<std_mpsc::Sender<Command>> = {
            let mut guard = self.lock();
            guard.drain().map(|(_, sender)| sender).collect()
        };
        for sender in senders {
            let _ = sender.send(Command::Shutdown);
        }
    }

    /// Ids with a live shell. Diagnostics and tests only.
    pub fn open_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.lock().keys().cloned().collect();
        ids.sort();
        ids
    }

    fn send(&self, terminal_id: &str, command: Command) -> bool {
        let guard = self.lock();
        match guard.get(terminal_id) {
            Some(sender) => sender.send(command).is_ok(),
            None => false,
        }
    }

    /// An absent `cwd` starts in the sandbox root; a present one must canonicalize
    /// inside it. Either way the answer is an absolute, canonical directory.
    fn resolve_cwd(&self, requested: Option<&str>) -> Result<PathBuf, TerminalFailure> {
        let Some(sandbox) = &self.sandbox else {
            return Err(TerminalFailure::new(
                PERMISSION_DENIED,
                "the agent has no usable workdir; terminals are disabled",
            ));
        };
        match requested {
            Some(raw) => sandbox.canonical_dir(raw).map_err(TerminalFailure::from_fs),
            None => Ok(sandbox.root().to_path_buf()),
        }
    }
}

/// Owns the child. Every path out of the loop kills and reaps, so a closed
/// terminal can never leave a zombie behind.
fn supervise(
    terminal_id: String,
    mut child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    mut writer: Box<dyn Write + Send>,
    inbox: std_mpsc::Receiver<Command>,
    out: UnboundedSender<ClientFrame>,
    terminals: Arc<Mutex<HashMap<String, std_mpsc::Sender<Command>>>>,
) {
    while let Ok(command) = inbox.recv() {
        match command {
            Command::Data(bytes) => {
                if let Err(error) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                    tracing::debug!(%error, terminal_id = %terminal_id, "pty write failed");
                    break;
                }
            }
            Command::Resize { cols, rows } => {
                if let Err(error) = master.resize(size(cols, rows)) {
                    tracing::debug!(%error, terminal_id = %terminal_id, "pty resize failed");
                }
            }
            Command::Exited => break,
            Command::Shutdown => break,
        }
    }
    // Whatever ended the loop, the shell must not outlive it.
    let _ = child.kill();
    let code = match child.wait() {
        // A signal death has no exit code: the contract spells that as null.
        Ok(status) if status.signal().is_some() => None,
        Ok(status) => Some(status.exit_code() as i32),
        Err(error) => {
            tracing::debug!(%error, terminal_id = %terminal_id, "cannot reap the shell");
            None
        }
    };
    // Dropping the master hangs up the tty, which is what tells any child still
    // holding it that the session is over.
    drop(master);
    lock(&terminals).remove(&terminal_id);
    let _ = out.send(ClientFrame::TerminalExit { terminal_id, code });
}

/// Master -> frames. Ends by telling the supervisor to reap the child, so the
/// exit status is reported exactly once, from one place.
fn pump(
    terminal_id: String,
    mut reader: Box<dyn Read + Send>,
    out: UnboundedSender<ClientFrame>,
    signal: std_mpsc::Sender<Command>,
) {
    let mut buffer = vec![0u8; READ_CHUNK];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let frame = ClientFrame::TerminalData {
                    terminal_id: terminal_id.clone(),
                    data: STANDARD.encode(&buffer[..read]),
                };
                if out.send(frame).is_err() {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                // EIO is how Linux reports "the last slave descriptor closed".
                tracing::debug!(%error, terminal_id = %terminal_id, "pty read ended");
                break;
            }
        }
    }
    let _ = signal.send(Command::Exited);
}

fn lock(
    terminals: &Arc<Mutex<HashMap<String, std_mpsc::Sender<Command>>>>,
) -> MutexGuard<'_, HashMap<String, std_mpsc::Sender<Command>>> {
    match terminals.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn checked_size(cols: u16, rows: u16) -> Result<PtySize, TerminalFailure> {
    if cols == 0 || rows == 0 || cols > MAX_DIMENSION || rows > MAX_DIMENSION {
        return Err(TerminalFailure::new(
            INVALID_SIZE,
            format!("cols/rows must be 1..={MAX_DIMENSION}, got {cols}x{rows}"),
        ));
    }
    Ok(size(cols, rows))
}

/// `terminal.open.shell` beats `--shell` beats `$SHELL` beats the platform default.
pub fn shell_command(requested: Option<&str>, default_shell: Option<&str>) -> String {
    let clean = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    if let Some(shell) = clean(requested).or_else(|| clean(default_shell)) {
        return shell;
    }
    let env_key = if cfg!(windows) { "COMSPEC" } else { "SHELL" };
    if let Some(shell) = clean(std::env::var(env_key).ok().as_deref()) {
        return shell;
    }
    String::from(if cfg!(windows) { "cmd.exe" } else { "/bin/sh" })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
    use tokio::time::timeout;

    use super::*;

    const WAIT: Duration = Duration::from_secs(20);
    const SHELL: &str = "/bin/sh";

    /// The shell under test. On Ubuntu /bin/sh is dash and on macOS it is bash in
    /// sh mode, and the two place the prompt differently — the difference that
    /// made these tests pass on one and fail on the other. Point the suite at a
    /// specific shell to reproduce either:
    ///   FARWORK_TEST_SHELL=/bin/dash cargo test -p farwork-agent term::tests
    fn test_shell() -> String {
        std::env::var("FARWORK_TEST_SHELL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| String::from(SHELL))
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("farwork-term-{}", uuid::Uuid::new_v4().simple()));
            fs::create_dir_all(&path).expect("temp dir");
            Self { path }
        }

        fn canonical(&self) -> PathBuf {
            self.path.canonicalize().expect("canonical temp dir")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// One host and the frame stream it writes into, with per-terminal output
    /// accumulated so a match split across two frames still counts.
    struct Harness {
        host: TerminalHost,
        sender: UnboundedSender<ClientFrame>,
        frames: UnboundedReceiver<ClientFrame>,
        output: HashMap<String, Vec<u8>>,
        exits: HashMap<String, Option<i32>>,
        tree: TempDir,
    }

    impl Harness {
        fn new() -> Self {
            Self::with_sandbox(true)
        }

        fn with_sandbox(sandboxed: bool) -> Self {
            let tree = TempDir::new();
            let sandbox = sandboxed.then(|| {
                Arc::new(Sandbox::new(tree.canonical()).expect("sandbox over the temp dir"))
            });
            let (sender, frames) = unbounded_channel();
            Self {
                host: TerminalHost::new(sandbox, Some(test_shell())),
                sender,
                frames,
                output: HashMap::new(),
                exits: HashMap::new(),
                tree,
            }
        }

        fn open(&self, spec: TerminalSpec) -> Result<(), TerminalFailure> {
            self.host.open(spec, self.sender.clone())
        }

        fn absorb(&mut self, frame: ClientFrame) {
            match frame {
                // Lifecycle frames carry no payload; the assertions below look at
                // the accumulated state instead.
                ClientFrame::TerminalOpened { .. } | ClientFrame::TerminalError { .. } => {}
                ClientFrame::TerminalData { terminal_id, data } => {
                    let bytes = STANDARD.decode(data.as_bytes()).expect("base64");
                    self.output
                        .entry(terminal_id)
                        .or_default()
                        .extend_from_slice(&bytes);
                }
                ClientFrame::TerminalExit { terminal_id, code } => {
                    self.exits.insert(terminal_id, code);
                }
                other => panic!("unexpected frame from the terminal host: {other:?}"),
            }
        }

        /// Read frames until this terminal's output satisfies `predicate`.
        async fn until_with<F>(&mut self, terminal_id: &str, predicate: F, what: &str) -> Vec<u8>
        where
            F: Fn(&[u8]) -> bool,
        {
            let deadline = timeout(WAIT, async {
                loop {
                    if let Some(output) = self.output.get(terminal_id) {
                        if predicate(output) {
                            return output.clone();
                        }
                    }
                    let frame = self.frames.recv().await.expect("frame channel closed");
                    self.absorb(frame);
                }
            })
            .await;
            deadline.unwrap_or_else(|_| {
                panic!(
                    "terminal {terminal_id} never {what}; have {:?}",
                    self.output
                        .get(terminal_id)
                        .map(|bytes| String::from_utf8_lossy(bytes))
                )
            })
        }

        /// Byte-substring wait, for output that has no line structure.
        async fn until(&mut self, terminal_id: &str, needle: &[u8]) -> Vec<u8> {
            let needle = needle.to_vec();
            let what = format!("produced {needle:?}");
            self.until_with(terminal_id, move |output| contains(output, &needle), &what)
                .await
        }

        /// Full-line wait. A terminal echoes what was typed, so "did the shell
        /// print X" must never degrade into "does X appear anywhere".
        async fn until_line(&mut self, terminal_id: &str, line: &str) -> Vec<u8> {
            let line = line.to_string();
            let what = format!("printed the line {line:?}");
            self.until_with(terminal_id, move |output| has_line(output, &line), &what)
                .await
        }

        /// The exit code reported for this terminal; panics if none arrives.
        async fn exit_of(&mut self, terminal_id: &str) -> Option<i32> {
            let deadline = timeout(WAIT, async {
                loop {
                    if let Some(code) = self.exits.get(terminal_id) {
                        return *code;
                    }
                    let frame = self.frames.recv().await.expect("frame channel closed");
                    self.absorb(frame);
                }
            })
            .await;
            deadline.unwrap_or_else(|_| panic!("terminal {terminal_id} never reported an exit"))
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty()
            && haystack
                .windows(needle.len())
                .any(|window| window == needle)
    }

    /// Interactive shells disagree about *where* the prompt goes: bash prints it
    /// before the echoed input, dash prints it after the echo and directly in front
    /// of the command's own output ("echo TERM_OK\r\n$ TERM_OK\r\n$ " on Ubuntu,
    /// where /bin/sh is dash). Strip at most one leading prompt marker, so a line
    /// comparison is about what the shell printed rather than which shell printed
    /// it. Exactly one: "$ echo TERM_OK" must stay a line that is not "TERM_OK".
    fn strip_prompt(line: &[u8]) -> &[u8] {
        for prompt in [
            b"$ ".as_slice(),
            b"# ".as_slice(),
            b"% ".as_slice(),
            b"> ".as_slice(),
        ] {
            if let Some(rest) = line.strip_prefix(prompt) {
                return rest;
            }
        }
        line
    }

    /// Output lines as a shell-independent comparison sees them.
    fn normalized_lines(output: &[u8]) -> impl Iterator<Item = &[u8]> {
        output
            .split(|byte| *byte == b'\n')
            .map(|candidate| candidate.strip_suffix(b"\r").unwrap_or(candidate))
            .map(strip_prompt)
    }

    /// Full-line match. The PTY echoes whatever was typed, so a substring test for
    /// "TERM_OK" also passes on the echoed "echo TERM_OK" and proves nothing.
    fn has_line(output: &[u8], line: &str) -> bool {
        normalized_lines(output).any(|candidate| candidate == line.as_bytes())
    }

    fn line_of(output: &[u8], prefix: &str) -> Option<String> {
        normalized_lines(output)
            .map(String::from_utf8_lossy)
            .find(|line| line.starts_with(prefix))
            .map(|line| line.into_owned())
    }

    #[tokio::test]
    async fn shell_output_is_streamed_and_the_exit_code_reported() {
        let mut harness = Harness::new();
        harness.open(TerminalSpec::new("t1", 80, 24)).expect("open");
        assert_eq!(harness.host.open_ids(), vec![String::from("t1")]);

        assert!(harness.host.write("t1", b"echo TERM_OK\n".to_vec()));
        let output = harness.until_line("t1", "TERM_OK").await;
        assert!(
            has_line(&output, "TERM_OK"),
            "the shell must print a whole TERM_OK line, got {:?}",
            String::from_utf8_lossy(&output)
        );

        assert!(harness.host.write("t1", b"exit 7\n".to_vec()));
        assert_eq!(harness.exit_of("t1").await, Some(7));
        assert!(
            harness.host.open_ids().is_empty(),
            "a self-exited terminal is released"
        );
    }

    #[tokio::test]
    async fn data_round_trips_arbitrary_bytes_including_binary() {
        let mut harness = Harness::new();
        harness.open(TerminalSpec::new("t1", 80, 24)).expect("open");

        // Octal escapes: the echoed command line must not contain the marker, or
        // the wait below would be satisfied by the echo instead of the output.
        harness.host.write(
            "t1",
            b"stty raw -echo; printf '\\122\\105\\101\\104\\131'; cat\n".to_vec(),
        );
        harness.until("t1", b"READY").await;

        let payload: Vec<u8> = vec![0x00, 0x01, 0x1b, 0x7f, 0x80, 0xfe, 0xff];
        assert!(harness.host.write("t1", payload.clone()));
        let output = harness.until("t1", &payload).await;
        assert!(
            contains(&output, &payload),
            "raw byte round trip lost data; got {:?}",
            output
        );
        harness.host.close("t1");
    }

    #[tokio::test]
    async fn resize_reaches_the_pty() {
        let mut harness = Harness::new();
        harness.open(TerminalSpec::new("t1", 80, 24)).expect("open");

        harness.host.write("t1", b"stty size\n".to_vec());
        harness.until_line("t1", "24 80").await;

        assert!(harness.host.resize("t1", 100, 30));
        harness.host.write("t1", b"stty size\n".to_vec());
        let output = harness.until_line("t1", "30 100").await;
        assert!(
            line_of(&output, "30 100").is_some(),
            "the kernel must report the new size: {:?}",
            String::from_utf8_lossy(&output)
        );
        // The first size must still be in the transcript: this is a resize, not a
        // fresh terminal.
        assert!(line_of(&output, "24 80").is_some());
        harness.host.close("t1");
    }

    #[tokio::test]
    async fn terminals_are_isolated_by_id() {
        let mut harness = Harness::new();
        harness
            .open(TerminalSpec::new("t1", 80, 24))
            .expect("open t1");
        harness
            .open(TerminalSpec::new("t2", 80, 24))
            .expect("open t2");
        assert_eq!(
            harness.host.open_ids(),
            vec![String::from("t1"), String::from("t2")]
        );

        harness.host.write("t1", b"echo ONLY_ONE\n".to_vec());
        harness.host.write("t2", b"echo ONLY_TWO\n".to_vec());
        let first = harness.until_line("t1", "ONLY_ONE").await;
        let second = harness.until_line("t2", "ONLY_TWO").await;
        assert!(has_line(&first, "ONLY_ONE"));
        assert!(has_line(&second, "ONLY_TWO"));
        assert!(
            !contains(&first, b"ONLY_TWO"),
            "terminal t1 must not see t2 output: {:?}",
            String::from_utf8_lossy(&first)
        );
        assert!(
            !contains(&second, b"ONLY_ONE"),
            "terminal t2 must not see t1 output: {:?}",
            String::from_utf8_lossy(&second)
        );
        harness.host.close_all();
    }

    #[tokio::test]
    async fn a_cwd_outside_the_workdir_is_refused() {
        let mut harness = Harness::new();
        let inside = harness.tree.canonical().join("inside");
        fs::create_dir_all(&inside).expect("inside");

        let mut escaped = TerminalSpec::new("evil", 80, 24);
        escaped.cwd = Some(String::from("/etc"));
        let failure = harness.open(escaped).expect_err("outside the workdir");
        assert_eq!(failure.code, PERMISSION_DENIED);
        assert!(
            failure.message.contains("outside the agent workdir"),
            "the refusal must name the boundary: {}",
            failure.message
        );
        assert!(
            harness.host.open_ids().is_empty(),
            "a refused open must not leave a terminal behind"
        );

        let mut accepted = TerminalSpec::new("inside", 80, 24);
        accepted.cwd = Some(inside.to_string_lossy().into_owned());
        harness.open(accepted).expect("inside the workdir");
        harness.host.write("inside", b"pwd\n".to_vec());
        let expected = inside.to_string_lossy().into_owned();
        let output = harness.until_line("inside", &expected).await;
        assert!(
            has_line(&output, &expected),
            "the shell must start in {expected}: {:?}",
            String::from_utf8_lossy(&output)
        );
        harness.host.close("inside");
    }

    #[tokio::test]
    async fn no_resolvable_workdir_refuses_every_terminal() {
        let harness = Harness::with_sandbox(false);
        let failure = harness
            .open(TerminalSpec::new("t1", 80, 24))
            .expect_err("no sandbox");
        assert_eq!(failure.code, PERMISSION_DENIED);
        assert!(harness.host.open_ids().is_empty());
    }

    #[tokio::test]
    async fn a_killed_shell_reports_an_unclean_exit() {
        let mut harness = Harness::new();
        harness.open(TerminalSpec::new("t1", 80, 24)).expect("open");
        harness.host.write("t1", b"echo UP\n".to_vec());
        harness.until_line("t1", "UP").await;

        assert!(harness.host.close("t1"), "the terminal was open");
        assert!(!harness.host.close("t1"), "closing twice is not a failure");
        assert!(!harness.host.write("t1", b"echo GONE\n".to_vec()));

        let code = harness.exit_of("t1").await;
        assert_ne!(code, Some(0), "a killed shell must not report a clean exit");
        assert!(
            harness.host.open_ids().is_empty(),
            "the id is released on close"
        );
    }

    #[tokio::test]
    async fn close_all_ends_every_terminal() {
        let mut harness = Harness::new();
        harness
            .open(TerminalSpec::new("t1", 80, 24))
            .expect("open t1");
        harness
            .open(TerminalSpec::new("t2", 80, 24))
            .expect("open t2");
        harness.host.close_all();
        harness.exit_of("t1").await;
        harness.exit_of("t2").await;
        assert!(harness.host.open_ids().is_empty());
    }

    #[tokio::test]
    async fn a_duplicate_id_and_an_absurd_size_are_refused() {
        let harness = Harness::new();
        harness.open(TerminalSpec::new("t1", 80, 24)).expect("open");
        let failure = harness
            .open(TerminalSpec::new("t1", 80, 24))
            .expect_err("duplicate");
        assert_eq!(failure.code, ALREADY_OPEN);

        let failure = harness
            .open(TerminalSpec::new("t2", 0, 24))
            .expect_err("zero columns");
        assert_eq!(failure.code, INVALID_SIZE);
        let failure = harness
            .open(TerminalSpec::new("t3", MAX_DIMENSION + 1, 24))
            .expect_err("too wide");
        assert_eq!(failure.code, INVALID_SIZE);
        harness.host.close_all();
    }

    #[test]
    fn prompt_normalization_keeps_line_matching_strict() {
        // The same fact, told by four prompts: bash puts the marker before the
        // echoed input, dash puts it in front of the command's own output.
        assert!(has_line(b"$ TERM_OK\n", "TERM_OK"));
        assert!(has_line(b"# TERM_OK\n", "TERM_OK"), "root prompt");
        assert!(has_line(b"% TERM_OK\n", "TERM_OK"), "zsh style prompt");
        assert!(has_line(b"> TERM_OK\n", "TERM_OK"), "continuation prompt");
        assert!(has_line(b"TERM_OK\n", "TERM_OK"), "no prompt at all");

        // Nothing about the strictness is given up: echoing the command is still
        // not the command's output, with or without a prompt in front of it.
        assert!(!has_line(b"echo TERM_OK\n", "TERM_OK"));
        assert!(!has_line(b"$ echo TERM_OK\n", "TERM_OK"));
        assert!(!has_line(b"$ echo TERM_OK\r\n", "TERM_OK"));
        assert!(!has_line(b"$ TERM_OK_extra\n", "TERM_OK"));

        // One marker, not a loop: a doubled prompt is not an output line either.
        assert!(has_line(b"$ $ TERM_OK\n", "$ TERM_OK"));
        assert!(!has_line(b"$ $ TERM_OK\n", "TERM_OK"));
    }

    /// The payloads CI reported for dash, quoted verbatim: this is the regression
    /// the test exists for (/bin/sh on Ubuntu is dash, which prints the prompt in
    /// front of the output instead of before the echo).
    #[test]
    fn dash_style_output_matches_the_same_lines_as_bash() {
        assert!(has_line(b"echo TERM_OK\r\n$ TERM_OK\r\n$ ", "TERM_OK"));

        let resize = b"stty size\r\n$ 24 80\r\n$ ";
        assert!(has_line(resize, "24 80"));
        assert_eq!(line_of(resize, "24 80").as_deref(), Some("24 80"));

        let pwd = b"pwd\r\n$ /srv/work/inside\r\n$ ";
        assert!(has_line(pwd, "/srv/work/inside"));
        assert_eq!(
            line_of(pwd, "/srv/work/inside").as_deref(),
            Some("/srv/work/inside")
        );

        // bash's shape keeps working from the same helper, including the terminal
        // capability report bash prints before the prompt.
        assert!(has_line(
            b"\x1b[?1034hsh-3.2$ echo TERM_OK\r\nTERM_OK\r\n",
            "TERM_OK"
        ));
        assert!(!has_line(b"\x1b[?1034hsh-3.2$ echo TERM_OK\r\n", "TERM_OK"));
    }

    #[test]
    fn shell_precedence_is_frame_then_flag_then_env_then_default() {
        assert_eq!(
            shell_command(Some("/bin/bash"), Some("/bin/zsh")),
            "/bin/bash"
        );
        assert_eq!(shell_command(Some("  "), Some("/bin/zsh")), "/bin/zsh");
        assert_eq!(shell_command(None, Some("/bin/zsh")), "/bin/zsh");
        let fallback = shell_command(None, None);
        assert!(!fallback.trim().is_empty());
    }
}
