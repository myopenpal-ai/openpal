//! Path jailing for `read_file`/`write_file`: a universal, category-broadened
//! denylist plus a TOCTOU-safe open. There is no OS sandbox here — OpenPal's
//! own Rust process makes the `open`/`read`/`write` calls directly, so
//! containment is entirely in this module.
//!
//! The denylist runs on **every** resolved canonical path, whether it lands
//! inside the configured agent workspace or is an absolute path elsewhere —
//! a coding-agent workspace is, by design, often pointed at a real cloned
//! project full of `.env`/`.git/config`/`id_rsa` files, so "inside the
//! workspace" must not be treated as "trusted". It is a living,
//! defense-in-depth list, not a completeness proof.

use super::tool::{ResolvedPath, ToolError};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// How `write_file` should open its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Create or truncate.
    Overwrite,
    /// Create if missing, append otherwise.
    Append,
    /// Fail if the file already exists — atomic `O_CREAT|O_EXCL`.
    CreateOnly,
}

/// Resolves `read_file`/`write_file` path arguments against a configured
/// workspace root, applying the universal denylist before anything else.
pub struct WorkspaceJail {
    /// Canonicalized workspace root.
    pub root: PathBuf,
    home_dir: PathBuf,
    data_dir: PathBuf,
}

impl WorkspaceJail {
    pub fn new(root: &Path, home_dir: &Path, data_dir: &Path) -> Result<Self, ToolError> {
        let root = root
            .canonicalize()
            .map_err(|e| ToolError::Rejected(format!("Agent workspace is not accessible: {e}")))?;
        // Canonicalize these too (best-effort) — comparisons below are
        // against fully-canonicalized target paths, and on macOS `/tmp` and
        // `/var` are themselves symlinks into `/private/...`, so comparing
        // an un-canonicalized data/home dir against a canonicalized target
        // would silently fail to match even for genuinely identical paths.
        let home_dir = home_dir
            .canonicalize()
            .unwrap_or_else(|_| home_dir.to_path_buf());
        let data_dir = data_dir
            .canonicalize()
            .unwrap_or_else(|_| data_dir.to_path_buf());
        Ok(Self {
            root,
            home_dir,
            data_dir,
        })
    }

    /// Resolve a `read_file` argument: relative to the workspace root,
    /// absolute as given. The target must already exist.
    pub fn resolve_for_read(&self, raw: &str) -> Result<ResolvedPath, ToolError> {
        let candidate = self.candidate_path(raw);
        let canonical = candidate
            .canonicalize()
            .map_err(|e| ToolError::Rejected(format!("Can't read \"{raw}\": {e}")))?;
        self.finish(raw, canonical)
    }

    /// Resolve a `write_file` argument. Unlike read, the target may not
    /// exist yet (for `CreateOnly`/`Overwrite` of a new file) — the parent
    /// directory must exist and must itself pass the denylist.
    pub fn resolve_for_write(&self, raw: &str) -> Result<ResolvedPath, ToolError> {
        let candidate = self.candidate_path(raw);
        let canonical = if candidate.exists() {
            candidate
                .canonicalize()
                .map_err(|e| ToolError::Rejected(format!("Can't write \"{raw}\": {e}")))?
        } else {
            let parent = candidate
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .ok_or_else(|| ToolError::Rejected(format!("\"{raw}\" isn't a valid path.")))?;
            let file_name = candidate
                .file_name()
                .ok_or_else(|| ToolError::Rejected(format!("\"{raw}\" isn't a valid path.")))?;
            let parent_canonical = parent.canonicalize().map_err(|e| {
                ToolError::Rejected(format!("Can't write \"{raw}\": parent directory: {e}"))
            })?;
            parent_canonical.join(file_name)
        };
        self.finish(raw, canonical)
    }

    /// Validate a `run_shell`/`run_code` working directory: must canonicalize
    /// to the workspace root or a descendant of it.
    pub fn require_cwd_in_workspace(&self, cwd: Option<&str>) -> Result<PathBuf, ToolError> {
        let candidate = match cwd {
            Some(c) if !c.is_empty() => self.candidate_path(c),
            _ => self.root.clone(),
        };
        let canonical = candidate
            .canonicalize()
            .map_err(|e| ToolError::Rejected(format!("Invalid working directory: {e}")))?;
        if canonical != self.root && !canonical.starts_with(&self.root) {
            return Err(ToolError::Rejected(
                "The working directory must be inside the agent workspace.".to_string(),
            ));
        }
        Ok(canonical)
    }

    fn candidate_path(&self, raw: &str) -> PathBuf {
        let p = Path::new(raw);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        }
    }

    fn finish(&self, raw: &str, canonical: PathBuf) -> Result<ResolvedPath, ToolError> {
        if is_denylisted(&canonical, &self.home_dir, &self.data_dir) {
            return Err(ToolError::Rejected(format!(
                "Refusing to touch \"{raw}\": it looks like a credential or system file."
            )));
        }
        let inside_workspace = canonical == self.root || canonical.starts_with(&self.root);
        Ok(ResolvedPath {
            raw: raw.to_string(),
            canonical,
            inside_workspace,
        })
    }
}

/// Category-broadened, universally-applied denylist. Checked against every
/// resolved canonical path regardless of workspace containment. Living list —
/// meant to be extended opportunistically, not a completeness guarantee.
fn is_denylisted(canonical: &Path, home_dir: &Path, data_dir: &Path) -> bool {
    // The app's own data (settings, grants, conversations, logs) is
    // categorically unreachable — a tool must never be able to write its own
    // future "always allow" grant file.
    if canonical == data_dir || canonical.starts_with(data_dir) {
        return true;
    }

    let file_name = canonical.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let rel_to_home = canonical
        .strip_prefix(home_dir)
        .ok()
        .map(|p| p.to_path_buf());

    // Credentials & secrets — matched on basename so they're caught anywhere,
    // not just directly under $HOME.
    const CREDENTIAL_BASENAME_SUFFIXES: &[&str] = &[".pem", ".key", ".p12", ".pfx"];
    const CREDENTIAL_BASENAMES: &[&str] = &[
        ".npmrc",
        ".git-credentials",
        ".netrc",
        ".pgpass",
        ".env",
        ".bash_history",
        ".zsh_history",
        "id_rsa",
        "id_ed25519",
        "id_rsa.pub",
        "id_ed25519.pub",
    ];
    if CREDENTIAL_BASENAME_SUFFIXES
        .iter()
        .any(|s| file_name.ends_with(s))
        || CREDENTIAL_BASENAMES.contains(&file_name)
        || file_name.starts_with(".env.")
        || file_name.to_ascii_lowercase().contains("credential")
        || file_name.to_ascii_lowercase().contains("token")
    {
        return true;
    }

    // Credential/config directories, wherever they sit relative to $HOME.
    const CREDENTIAL_DIR_PREFIXES: &[&str] = &[
        ".ssh",
        ".aws",
        ".config/gcloud",
        ".docker",
        ".kube",
        ".gnupg",
        ".azure",
        ".config/gh",
    ];
    if let Some(rel) = &rel_to_home {
        if CREDENTIAL_DIR_PREFIXES
            .iter()
            .any(|prefix| rel.starts_with(prefix))
        {
            return true;
        }
        // OS persistence / execution surfaces under $HOME.
        const HOME_PERSISTENCE: &[&str] = &[
            ".zshrc",
            ".zshenv",
            ".zprofile",
            ".bashrc",
            ".bash_profile",
            ".profile",
            "Library/LaunchAgents",
            "Library/Keychains",
        ];
        if HOME_PERSISTENCE.iter().any(|p| rel.starts_with(p)) {
            return true;
        }
    }

    // System-wide persistence / privilege surfaces, absolute regardless of home.
    const SYSTEM_PERSISTENCE: &[&str] = &[
        "/Library/LaunchDaemons",
        "/usr/lib/cron/tabs",
        "/etc/crontab",
        "/etc/cron.d",
        "/etc/sudoers",
        "/etc/sudoers.d",
    ];
    if SYSTEM_PERSISTENCE.iter().any(|p| canonical.starts_with(p)) {
        return true;
    }

    false
}

fn reject_if_symlink(path: &Path) -> Result<(), ToolError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(ToolError::Rejected(
            "This path is a symlink, which agent mode never follows.".to_string(),
        )),
        _ => Ok(()),
    }
}

/// After opening `file` by path, confirm the file descriptor still refers to
/// the same inode a fresh `lstat` of that path reports. This closes the
/// narrow window between our own `open()` call and this check (relevant on
/// filesystems where those two syscalls aren't effectively atomic) — it is
/// *not* what protects against a path being redirected between an earlier
/// consent-time resolution and this call; that's `resolve_for_read`/
/// `resolve_for_write` re-running the denylist fresh every time, regardless
/// of what an earlier resolution saw (see the workspace-jail tests).
fn verify_fd_matches(file: &File, canonical: &Path) -> Result<(), ToolError> {
    let fd_meta = file
        .metadata()
        .map_err(|e| ToolError::ExecutionFailed(format!("stat failed: {e}")))?;
    let path_meta = std::fs::symlink_metadata(canonical)
        .map_err(|_| ToolError::Rejected("This path changed since it was approved.".to_string()))?;
    if fd_meta.dev() != path_meta.dev() || fd_meta.ino() != path_meta.ino() {
        return Err(ToolError::Rejected(
            "This path changed since it was approved.".to_string(),
        ));
    }
    Ok(())
}

/// Open `canonical` for reading, TOCTOU-safe: reject if it's currently a
/// symlink, open with `O_NOFOLLOW`, then re-verify the opened fd's identity
/// against a fresh `lstat` of the same path. Runs unconditionally on every
/// call — including the "Always allow" grant-hit fast path — not only the
/// first time a path is approved.
pub fn secure_open_for_read(canonical: &Path) -> Result<File, ToolError> {
    reject_if_symlink(canonical)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(canonical)
        .map_err(|e| ToolError::ExecutionFailed(format!("Could not open file: {e}")))?;
    verify_fd_matches(&file, canonical)?;
    Ok(file)
}

/// Open `canonical` for writing per `mode`, with the same TOCTOU-safe
/// discipline as [`secure_open_for_read`]. `CreateOnly` uses atomic
/// `O_CREAT|O_EXCL` (via `create_new`) rather than a separate existence
/// check followed by open.
pub fn secure_open_for_write(canonical: &Path, mode: WriteMode) -> Result<File, ToolError> {
    if canonical.exists() {
        reject_if_symlink(canonical)?;
    }
    let mut opts = OpenOptions::new();
    opts.write(true).custom_flags(libc::O_NOFOLLOW);
    match mode {
        WriteMode::Overwrite => {
            opts.create(true).truncate(true);
        }
        WriteMode::Append => {
            opts.create(true).append(true);
        }
        WriteMode::CreateOnly => {
            opts.create_new(true);
        }
    }
    let file = opts.open(canonical).map_err(|e| {
        if mode == WriteMode::CreateOnly && e.kind() == std::io::ErrorKind::AlreadyExists {
            ToolError::Rejected(format!("\"{}\" already exists.", canonical.display()))
        } else {
            ToolError::ExecutionFailed(format!("Could not open file for writing: {e}"))
        }
    })?;
    verify_fd_matches(&file, canonical)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn jail(root: &Path) -> WorkspaceJail {
        WorkspaceJail::new(
            root,
            &PathBuf::from("/nonexistent-home"),
            &PathBuf::from("/nonexistent-data"),
        )
        .unwrap()
    }

    #[test]
    fn resolves_a_relative_path_inside_the_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "hi").unwrap();
        let j = jail(tmp.path());
        let resolved = j.resolve_for_read("notes.txt").unwrap();
        assert!(resolved.inside_workspace);
        assert_eq!(
            resolved.canonical,
            tmp.path().canonicalize().unwrap().join("notes.txt")
        );
    }

    #[test]
    fn dot_dot_traversal_out_of_the_workspace_is_not_flagged_inside() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(tmp.path().join("secret.txt"), "outside").unwrap();
        let j = jail(&workspace);
        let resolved = j.resolve_for_read("../secret.txt").unwrap();
        assert!(
            !resolved.inside_workspace,
            "escaping .. must not read as inside the workspace"
        );
    }

    #[test]
    fn symlink_escape_is_rejected_at_open_time() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let secret = tmp.path().join("secret.txt");
        std::fs::write(&secret, "top secret").unwrap();
        let link = workspace.join("link.txt");
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        let j = jail(&workspace);
        // canonicalize() itself follows the symlink, so resolve_for_read
        // "succeeds" with the secret's real path — this is why the open must
        // independently reject a symlink leaf, not just trust resolve().
        let resolved = j.resolve_for_read("link.txt").unwrap();
        let err = secure_open_for_read(&link).unwrap_err();
        assert!(matches!(err, ToolError::Rejected(_)));
        // Sanity: the canonical path did resolve to the real secret file.
        assert_eq!(resolved.canonical, secret.canonicalize().unwrap());
    }

    /// The realistic TOCTOU threat isn't "the file's content changed at the
    /// same path" (re-reading changed content at an approved path is normal,
    /// intended behavior — that's the whole point of an "always allow
    /// reading this log file" grant) — it's "the path was redirected, via a
    /// symlink, to something that should never have been approved". Every
    /// call independently re-resolves (canonicalize + denylist) at execute
    /// time rather than trusting an earlier resolution, so a swap introduced
    /// between consent and execution is caught by the *second* resolve, not
    /// by comparing against the first one.
    #[test]
    fn a_symlink_swapped_in_after_first_resolution_is_caught_by_a_fresh_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let ssh = home.join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        std::fs::write(ssh.join("id_rsa"), "private key").unwrap();

        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let target = workspace.join("notes.txt");
        std::fs::write(&target, "hello").unwrap();

        let j = WorkspaceJail::new(&workspace, &home, Path::new("/nonexistent-data")).unwrap();
        let first = j.resolve_for_read("notes.txt").unwrap();
        assert!(first.inside_workspace);

        // Swap the approved path for a symlink into a denylisted credential
        // file — simulating what could happen during the gap between a
        // fresh consent prompt being shown and the user clicking Allow.
        std::fs::remove_file(&target).unwrap();
        std::os::unix::fs::symlink(ssh.join("id_rsa"), &target).unwrap();

        let second = j.resolve_for_read("notes.txt");
        assert!(
            second.is_err(),
            "a fresh resolve must re-run the denylist against whatever is actually there now"
        );
    }

    #[test]
    fn secure_open_does_not_spuriously_reject_a_stable_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.txt");
        std::fs::write(&path, "stable content").unwrap();
        let canonical = path.canonicalize().unwrap();
        let mut file = secure_open_for_read(&canonical).unwrap();
        let mut buf = String::new();
        use std::io::Read as _;
        file.read_to_string(&mut buf).unwrap();
        assert_eq!(buf, "stable content");
    }

    #[test]
    fn denylist_blocks_ssh_dir_even_when_pointed_at_as_a_workspace() {
        let home = tempfile::tempdir().unwrap();
        let ssh_dir = home.path().join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        std::fs::write(ssh_dir.join("id_rsa"), "private key").unwrap();

        let j = WorkspaceJail::new(&ssh_dir, home.path(), Path::new("/nonexistent-data")).unwrap();
        let err = j.resolve_for_read("id_rsa").unwrap_err();
        assert!(
            matches!(err, ToolError::Rejected(_)),
            "credentials must be denied even inside the configured workspace"
        );
    }

    #[test]
    fn denylist_blocks_env_files_outside_the_workspace_too() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(tmp.path().join(".env"), "SECRET=1").unwrap();

        let j = jail(&workspace);
        let err = j.resolve_for_read("../.env").unwrap_err();
        assert!(matches!(err, ToolError::Rejected(_)));
    }

    #[test]
    fn app_data_dir_is_always_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let data_dir = tmp.path().join("app-data");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("agent-grants.json"), "{}").unwrap();

        let j = WorkspaceJail::new(&workspace, Path::new("/nonexistent-home"), &data_dir).unwrap();
        let err = j
            .resolve_for_read("../app-data/agent-grants.json")
            .unwrap_err();
        assert!(matches!(err, ToolError::Rejected(_)));
    }

    #[test]
    fn write_create_only_fails_if_the_file_already_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.txt");
        std::fs::write(&path, "existing").unwrap();
        let err = secure_open_for_write(&path, WriteMode::CreateOnly).unwrap_err();
        assert!(matches!(err, ToolError::Rejected(_)));
    }

    #[test]
    fn write_create_only_succeeds_for_a_new_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("new.txt");
        let mut file = secure_open_for_write(&path, WriteMode::CreateOnly).unwrap();
        file.write_all(b"hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn cwd_must_be_inside_the_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let sub = workspace.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        let j = jail(&workspace);
        assert!(j.require_cwd_in_workspace(Some("sub")).is_ok());
        assert!(j.require_cwd_in_workspace(None).is_ok());
        assert!(j
            .require_cwd_in_workspace(Some(outside.to_str().unwrap()))
            .is_err());
    }
}
