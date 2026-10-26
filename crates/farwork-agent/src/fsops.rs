//! Local filesystem access behind the `fs.request` ingress frame.
//!
//! The agent is reachable from the hub, so an unconstrained read would hand a
//! remote user every file the agent's OS user can see. Every request path is
//! therefore resolved with `canonicalize()` — which follows symlinks —
//! and must land under the `--workdir` root. A symlink pointing outside
//! resolves outside and is refused for exactly the same reason a `../` escape
//! is.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::protocol::{FsEntry, FsEntryKind, FsErrorCode, FsOp, FsResponse};
use crate::timeutil;

/// Upper bound for `readText`. Larger files are refused with `too_large`
/// rather than returned partially: a truncated body that claims to be whole is
/// worse than an error the caller can see.
pub const MAX_READ_BYTES: u64 = 1024 * 1024;

/// One filesystem failure, in the contract's vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsFailure {
    pub code: FsErrorCode,
    pub message: String,
}

impl FsFailure {
    fn new(code: FsErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self::new(FsErrorCode::PermissionDenied, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(FsErrorCode::NotFound, message)
    }

    fn io_error(message: impl Into<String>) -> Self {
        Self::new(FsErrorCode::IoError, message)
    }

    fn too_large(message: impl Into<String>) -> Self {
        Self::new(FsErrorCode::TooLarge, message)
    }
}

/// The set of paths this agent is allowed to read: the canonical root and
/// everything canonical under it.
#[derive(Debug, Clone)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            root: root.as_ref().canonicalize()?,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Canonicalize `raw` and prove it stays inside the root.
    fn resolve(&self, raw: &str) -> Result<PathBuf, FsFailure> {
        if raw.is_empty() {
            return Err(FsFailure::permission_denied("path must not be empty"));
        }
        let requested = Path::new(raw);
        if !requested.is_absolute() {
            return Err(FsFailure::permission_denied(format!(
                "{raw:?} is not an absolute path"
            )));
        }
        let canonical = fs::canonicalize(requested).map_err(|error| map_io(&error, raw))?;
        // Component-wise: "/srv/work-evil" is not under "/srv/work".
        if !canonical.starts_with(&self.root) {
            return Err(FsFailure::permission_denied(format!(
                "{raw:?} is outside the agent workdir {}",
                self.root.display()
            )));
        }
        Ok(canonical)
    }

    /// One level of `raw`: directories first, then files, each by name.
    /// Returns the canonical directory path alongside the entries, because that
    /// is the path the caller must use to descend.
    pub fn list(&self, raw: &str) -> Result<(String, Vec<FsEntry>), FsFailure> {
        let directory = self.resolve(raw)?;
        let reader = fs::read_dir(&directory).map_err(|error| map_io(&error, raw))?;
        let mut entries = Vec::new();
        for item in reader {
            let item = match item {
                Ok(item) => item,
                Err(error) => {
                    tracing::warn!(%error, directory = %directory.display(), "skipping an unreadable directory entry");
                    continue;
                }
            };
            let path = item.path();
            let metadata = match item.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "skipping an entry that cannot be stat-ed");
                    continue;
                }
            };
            let modified_at = metadata
                .modified()
                .map(timeutil::format_system_time)
                .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"));
            entries.push(FsEntry {
                name: item.file_name().to_string_lossy().into_owned(),
                path: path.to_string_lossy().into_owned(),
                kind: if metadata.is_dir() {
                    FsEntryKind::Dir
                } else {
                    FsEntryKind::File
                },
                size_bytes: metadata.len(),
                modified_at,
            });
        }
        entries.sort_by(|left, right| {
            rank(left.kind)
                .cmp(&rank(right.kind))
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok((path_text(&directory), entries))
    }

    /// The whole file as UTF-8 text, or a refusal that says why.
    pub fn read_text(&self, raw: &str) -> Result<(String, String, u64), FsFailure> {
        let path = self.resolve(raw)?;
        let metadata = fs::metadata(&path).map_err(|error| map_io(&error, raw))?;
        if metadata.is_dir() {
            return Err(FsFailure::io_error(format!(
                "{raw:?} is a directory, not a text file"
            )));
        }
        if metadata.len() > MAX_READ_BYTES {
            return Err(too_large_for(raw, metadata.len()));
        }
        let bytes = fs::read(&path).map_err(|error| map_io(&error, raw))?;
        // The size was checked before the read; re-check after, because the file
        // may have grown in between.
        if bytes.len() as u64 > MAX_READ_BYTES {
            return Err(too_large_for(raw, bytes.len() as u64));
        }
        let size_bytes = bytes.len() as u64;
        let content = String::from_utf8(bytes).map_err(|error| {
            FsFailure::io_error(format!(
                "{raw:?} is not valid UTF-8 (byte {}): {}",
                error.utf8_error().valid_up_to(),
                error
            ))
        })?;
        Ok((path_text(&path), content, size_bytes))
    }

    /// Build the wire reply for one `fs.request`.
    pub fn respond(&self, request_id: &str, op: FsOp, raw: &str) -> FsResponse {
        match op {
            FsOp::List => match self.list(raw) {
                Ok((path, entries)) => FsResponse::list(request_id, path, entries),
                Err(failure) => FsResponse::failure(request_id, failure.code, failure.message),
            },
            FsOp::ReadText => match self.read_text(raw) {
                Ok((path, content, size_bytes)) => {
                    FsResponse::read_text(request_id, path, content, size_bytes)
                }
                Err(failure) => FsResponse::failure(request_id, failure.code, failure.message),
            },
        }
    }
}

/// Reply used when the agent has no usable sandbox at all: deny, never fall back
/// to an unconstrained read.
pub fn no_sandbox(request_id: &str, message: &str) -> FsResponse {
    FsResponse::failure(request_id, FsErrorCode::PermissionDenied, message)
}

fn rank(kind: FsEntryKind) -> u8 {
    match kind {
        FsEntryKind::Dir => 0,
        FsEntryKind::File => 1,
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn too_large_for(raw: &str, size: u64) -> FsFailure {
    FsFailure::too_large(format!(
        "{raw:?} is {size} bytes; the readText limit is {MAX_READ_BYTES} bytes"
    ))
}

fn map_io(error: &io::Error, raw: &str) -> FsFailure {
    match error.kind() {
        io::ErrorKind::NotFound => FsFailure::not_found(format!("{raw:?} does not exist")),
        io::ErrorKind::PermissionDenied => {
            FsFailure::permission_denied(format!("{raw:?} cannot be read: {error}"))
        }
        _ => FsFailure::io_error(format!("{raw:?}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("farwork-fs-{}", uuid::Uuid::new_v4().simple()));
            fs::create_dir_all(&root).expect("create temp root");
            Self { root }
        }

        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.root.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("parent");
            }
            fs::write(&path, bytes).expect("write");
            path
        }

        fn dir(&self, name: &str) -> PathBuf {
            let path = self.root.join(name);
            fs::create_dir_all(&path).expect("mkdir");
            path
        }

        fn sandbox(&self) -> Sandbox {
            Sandbox::new(&self.root).expect("sandbox")
        }

        fn canonical(&self) -> PathBuf {
            self.root.canonicalize().expect("canonical root")
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn list_returns_one_level_directories_first_then_files_by_name() {
        let tree = TempTree::new();
        tree.write("zeta.txt", b"z");
        tree.write("alpha.txt", b"a");
        tree.dir("tools");
        tree.dir("app");
        tree.write("tools/nested.txt", b"nested");
        let sandbox = tree.sandbox();

        let (path, entries) = sandbox
            .list(&tree.canonical().to_string_lossy())
            .expect("list");
        assert_eq!(path, tree.canonical().to_string_lossy());
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["app", "tools", "alpha.txt", "zeta.txt"],
            "directories first, then files, each sorted by name"
        );
        assert_eq!(entries[0].kind, FsEntryKind::Dir);
        assert_eq!(entries[2].kind, FsEntryKind::File);
        assert_eq!(entries[2].size_bytes, 1, "sizeBytes is the real file size");
        assert!(
            entries[0].path.starts_with(path.as_str()),
            "entry paths are absolute and inside the listed directory: {entries:?}"
        );
        assert!(
            entries[2].modified_at.ends_with('Z') && entries[2].modified_at.contains('T'),
            "modifiedAt is RFC3339: {}",
            entries[2].modified_at
        );
        assert!(
            !names.contains(&"nested.txt"),
            "listing must not descend into subdirectories"
        );
    }

    #[test]
    fn read_text_returns_content_and_byte_size() {
        let tree = TempTree::new();
        let file = tree.write("notes.md", "héllo\n".as_bytes());
        let sandbox = tree.sandbox();

        let (path, content, size) = sandbox
            .read_text(&file.canonicalize().expect("file").to_string_lossy())
            .expect("read");
        assert_eq!(path, file.canonicalize().expect("file").to_string_lossy());
        assert_eq!(content, "héllo\n");
        assert_eq!(size, "héllo\n".len() as u64);
    }

    #[test]
    fn a_path_outside_the_root_is_refused_even_when_it_exists() {
        let tree = TempTree::new();
        let sandbox = tree.sandbox();

        for raw in ["/etc/passwd", "/etc", "/tmp"] {
            let failure = sandbox.list(raw).expect_err(raw);
            assert_eq!(
                failure.code,
                FsErrorCode::PermissionDenied,
                "{raw} must be denied, got {failure:?}"
            );
        }
        let failure = sandbox.read_text("/etc/passwd").expect_err("/etc/passwd");
        assert_eq!(failure.code, FsErrorCode::PermissionDenied);
    }

    #[test]
    fn parent_directory_escapes_are_refused() {
        let tree = TempTree::new();
        tree.dir("inside");
        // A real directory next to the root, reachable through "../" and
        // therefore a meaningful escape target rather than a missing path.
        let outside = tree.root.parent().expect("parent").join(format!(
            "{}-outside",
            tree.root.file_name().expect("name").to_string_lossy()
        ));
        fs::create_dir_all(&outside).expect("outside dir");
        let sandbox = tree.sandbox();

        for escape in [
            tree.canonical().join("../").join(
                outside
                    .file_name()
                    .expect("outside name")
                    .to_string_lossy()
                    .as_ref(),
            ),
            tree.canonical().join("inside/../../").join(
                outside
                    .file_name()
                    .expect("outside name")
                    .to_string_lossy()
                    .as_ref(),
            ),
        ] {
            let failure = sandbox.list(&escape.to_string_lossy()).expect_err("escape");
            assert_eq!(
                failure.code,
                FsErrorCode::PermissionDenied,
                "{escape:?} must be denied: {failure:?}"
            );
            assert!(
                failure.message.contains("outside the agent workdir"),
                "the refusal must name the boundary: {}",
                failure.message
            );
        }
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_sibling_directory_sharing_the_root_prefix_is_refused() {
        let tree = TempTree::new();
        let sibling = tree.root.parent().expect("parent").join(format!(
            "{}-evil",
            tree.root.file_name().expect("name").to_string_lossy()
        ));
        fs::create_dir_all(&sibling).expect("sibling");
        let sandbox = tree.sandbox();

        let failure = sandbox
            .list(&sibling.to_string_lossy())
            .expect_err("sibling");
        assert_eq!(
            failure.code,
            FsErrorCode::PermissionDenied,
            "a prefix match must be component-wise: {sibling:?}"
        );
        let _ = fs::remove_dir_all(&sibling);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_pointing_outside_the_root_is_refused() {
        let tree = TempTree::new();
        let sandbox = tree.sandbox();
        let link = tree.root.join("escape-link");
        std::os::unix::fs::symlink("/etc", &link).expect("symlink");

        let failure = sandbox.list(&link.to_string_lossy()).expect_err("symlink");
        assert_eq!(
            failure.code,
            FsErrorCode::PermissionDenied,
            "canonicalize must resolve the link before the prefix check: {failure:?}"
        );

        // The same link through a file is refused too.
        let file_link = tree.root.join("passwd-link");
        std::os::unix::fs::symlink("/etc/passwd", &file_link).expect("symlink");
        let failure = sandbox
            .read_text(&file_link.to_string_lossy())
            .expect_err("symlink file");
        assert_eq!(failure.code, FsErrorCode::PermissionDenied);
    }

    #[test]
    fn files_above_the_read_limit_are_refused_not_truncated() {
        let tree = TempTree::new();
        let file = tree.write("big.txt", &vec![b'x'; MAX_READ_BYTES as usize + 1]);
        let sandbox = tree.sandbox();

        let failure = sandbox
            .read_text(&file.to_string_lossy())
            .expect_err("too large");
        assert_eq!(failure.code, FsErrorCode::TooLarge);
        assert!(
            failure.message.contains("1048576"),
            "the limit must be stated: {}",
            failure.message
        );

        // Exactly at the limit is still readable.
        let at_limit = tree.write("limit.txt", &vec![b'y'; MAX_READ_BYTES as usize]);
        let (_, content, size) = sandbox
            .read_text(&at_limit.to_string_lossy())
            .expect("at limit");
        assert_eq!(size, MAX_READ_BYTES);
        assert_eq!(content.len() as u64, MAX_READ_BYTES);
    }

    #[test]
    fn a_non_utf8_file_is_an_io_error_that_names_the_reason() {
        let tree = TempTree::new();
        let file = tree.write("binary.bin", &[0xff, 0xfe, 0x00, 0x41]);
        let sandbox = tree.sandbox();

        let failure = sandbox
            .read_text(&file.to_string_lossy())
            .expect_err("non utf8");
        assert_eq!(failure.code, FsErrorCode::IoError);
        assert!(
            failure.message.contains("UTF-8"),
            "the reason must be reported: {}",
            failure.message
        );
    }

    #[test]
    fn a_missing_path_is_reported_as_not_found() {
        let tree = TempTree::new();
        let sandbox = tree.sandbox();
        let missing = tree.canonical().join("nope.txt");

        let failure = sandbox
            .read_text(&missing.to_string_lossy())
            .expect_err("missing");
        assert_eq!(failure.code, FsErrorCode::NotFound);
    }

    #[test]
    fn relative_paths_and_directories_as_text_are_refused() {
        let tree = TempTree::new();
        tree.dir("sub");
        let sandbox = tree.sandbox();

        let failure = sandbox.list("sub").expect_err("relative");
        assert_eq!(failure.code, FsErrorCode::PermissionDenied);

        let failure = sandbox
            .read_text(&tree.canonical().join("sub").to_string_lossy())
            .expect_err("directory");
        assert_eq!(failure.code, FsErrorCode::IoError);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_directory_is_permission_denied() {
        use std::os::unix::fs::PermissionsExt as _;
        let tree = TempTree::new();
        let locked = tree.dir("locked");
        let sandbox = tree.sandbox();
        let mut permissions = fs::metadata(&locked).expect("metadata").permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&locked, permissions).expect("chmod");

        let listed = sandbox.list(&locked.to_string_lossy());
        let mut permissions = fs::metadata(&locked).expect("metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&locked, permissions).expect("chmod back");

        match listed {
            Ok(_) => {
                // Running as root bypasses mode bits; there is nothing to assert.
                eprintln!("SKIP mode-bit assertion: this process can read 0o000 directories");
            }
            Err(failure) => assert_eq!(
                failure.code,
                FsErrorCode::PermissionDenied,
                "unreadable directory: {failure:?}"
            ),
        }
    }

    #[test]
    fn respond_builds_the_contract_shapes() {
        let tree = TempTree::new();
        let file = tree.write("one.txt", b"one");
        let sandbox = tree.sandbox();

        let listed = sandbox.respond("req_1", FsOp::List, &tree.canonical().to_string_lossy());
        assert!(listed.ok);
        assert_eq!(listed.op, Some(FsOp::List));
        assert_eq!(listed.request_id, "req_1");
        assert!(listed.entries.is_some());
        assert!(listed.content.is_none() && listed.error.is_none());

        let read = sandbox.respond("req_2", FsOp::ReadText, &file.to_string_lossy());
        assert!(read.ok);
        assert_eq!(read.op, Some(FsOp::ReadText));
        assert_eq!(read.content.as_deref(), Some("one"));
        assert_eq!(read.size_bytes, Some(3));
        assert_eq!(read.truncated, Some(false));

        let denied = sandbox.respond("req_3", FsOp::ReadText, "/etc/passwd");
        assert!(!denied.ok);
        assert_eq!(
            denied.error.as_ref().map(|error| error.code),
            Some(FsErrorCode::PermissionDenied)
        );
        assert!(denied.content.is_none() && denied.entries.is_none());
    }
}
