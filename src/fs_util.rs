//! Filesystem utilities — atomic writes with crash-safe temp-file-then-rename.

use std::path::Path;

/// RAII guard that removes a temp file on drop unless defused.
struct TempGuard<'a> {
    path: &'a Path,
    active: bool,
}

impl<'a> TempGuard<'a> {
    fn new(path: &'a Path) -> Self {
        Self { path, active: true }
    }

    /// Disarm the guard so the temp file is **not** deleted on drop.
    fn defuse(&mut self) {
        self.active = false;
    }
}

impl Drop for TempGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = std::fs::remove_file(self.path);
        }
    }
}

/// Atomically write `data` to `path` via a temp file and rename.
///
/// 1. Creates a temp file (`.tmp`) in the same directory as `path`.
/// 2. On Unix the temp file is opened with mode `0o600`.
/// 3. Writes `data`, calls `sync_all`, then renames into `path`.
/// 4. Fsyncs the parent directory so the rename is durable on crash.
/// 5. If any step after temp-file creation fails, the temp file is
///    cleaned up automatically (RAII guard).
///
/// Callers must ensure no concurrent writes target the same `path`.
pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no parent directory",
        )
    })?;

    // Build a deterministic temp name: .<filename>.tmp
    let file_name = path
        .file_name()
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name")
        })?
        .to_string_lossy();
    let tmp_path = parent.join(format!(".{file_name}.tmp"));

    let mut guard = TempGuard::new(&tmp_path);

    // Open + write + sync.
    write_tmp(&tmp_path, data)?;

    // Atomic rename into the final location.
    std::fs::rename(&tmp_path, path)?;
    guard.defuse();

    // Fsync the parent directory so the rename is durable even on hard crash.
    // Best-effort: the rename already committed, so a dir-fsync failure should
    // not cause callers to treat the write as failed.
    if let Err(e) = fsync_dir(parent) {
        tracing::warn!("fsync of parent directory failed (data is written): {e}");
    }

    Ok(())
}

/// Create and write the temp file with platform-appropriate options.
fn write_tmp(tmp_path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(tmp_path)?;
    f.write_all(data)?;
    f.sync_all()?;
    Ok(())
}

/// Fsync a directory to ensure metadata (renames) is persisted.
#[cfg(unix)]
fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    let d = std::fs::File::open(dir)?;
    d.sync_all()?;
    Ok(())
}

/// No-op on non-Unix — Windows flushes directory metadata on rename.
#[cfg(not(unix))]
fn fsync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn atomic_write_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("test.txt");

        atomic_write(&target, b"hello world").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "hello world");
        // Temp file should not linger.
        assert!(!dir.path().join(".test.txt.tmp").exists());
    }

    #[test]
    fn atomic_write_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("test.txt");

        fs::write(&target, "old content").unwrap();
        atomic_write(&target, b"new content").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "new content");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_sets_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("secret.txt");

        atomic_write(&target, b"secret").unwrap();

        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "file should be 0600, got {mode:o}");
    }

    #[test]
    fn temp_file_cleaned_on_missing_parent() {
        // Writing to a path whose parent doesn't exist should fail
        // and not leave debris.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nonexistent_dir").join("file.txt");

        assert!(atomic_write(&target, b"data").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn temp_file_cleaned_on_rename_failure() {
        // Make the rename target a *directory* so write_tmp succeeds
        // (creating .file.txt.tmp in the parent) but rename fails with
        // EISDIR. This exercises TempGuard cleanup after a real write.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.txt");
        let tmp = dir.path().join(".file.txt.tmp");

        // Create a directory at the target path — rename(file, dir) → EISDIR.
        fs::create_dir(&target).unwrap();

        let result = atomic_write(&target, b"data");
        assert!(result.is_err(), "expected rename to fail with EISDIR");
        // TempGuard should have cleaned up the temp file.
        assert!(!tmp.exists(), "temp file should be cleaned up by TempGuard");
        // The directory target should still be intact.
        assert!(target.is_dir(), "target directory should be untouched");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_tightens_permissions_on_overwrite() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("wide.txt");

        // Create file with wide permissions.
        fs::write(&target, "old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

        // Overwrite via atomic_write — should end up 0o600.
        atomic_write(&target, b"new").unwrap();

        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "overwritten file should be 0600, got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_rejects_symlink_temp_path() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.txt");
        let tmp = dir.path().join(".file.txt.tmp");

        // Pre-plant a symlink at the temp path.
        let decoy = dir.path().join("decoy.txt");
        std::os::unix::fs::symlink(&decoy, &tmp).unwrap();

        // atomic_write should fail because O_NOFOLLOW rejects the symlink.
        let result = atomic_write(&target, b"secret");
        assert!(result.is_err(), "should reject symlink at temp path");

        // Decoy should not have been written to.
        assert!(!decoy.exists(), "symlink target should be untouched");
    }
}
