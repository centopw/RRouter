//! Shared helpers for the gateway integration tests.
//!
//! Every test that needs a data directory gets a unique one under `$TMPDIR`. `TempDir` owns
//! that directory and removes it when the test's harness drops, so a second `cargo test` run
//! starts where the first one did instead of finding the previous run's trees.

// Each test binary compiles this module on its own and uses a different subset of it.
#![allow(dead_code)]

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A unique `$TMPDIR/rrouter-<tag>-<uuid>` path. Nothing is created here.
pub fn temp_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("rrouter-{tag}-{}", uuid::Uuid::new_v4()))
}

/// A test data directory that removes itself.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// A fresh directory, unique per call.
    pub fn new(tag: &str) -> Self {
        Self::adopt(temp_path(tag))
    }

    /// Own `path`, creating it if needed, and remove it on drop.
    ///
    /// A harness that builds a whole `Config` (and with it a `data_dir: PathBuf`) before the
    /// harness itself exists adopts that path, so a test still names only one directory.
    pub fn adopt(path: PathBuf) -> Self {
        std::fs::create_dir_all(&path).expect("temp dir");
        Self { path }
    }

    /// The directory as a path, for the `Config`s whose field is a `PathBuf`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn purge(&self) {
        // Best effort: a test may have removed the tree itself, or never written anything.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        self.purge();
    }
}

#[cfg(test)]
mod tests {
    use super::TempDir;

    #[test]
    fn the_directory_exists_while_guarded_and_is_removed_on_drop() {
        let dir = TempDir::new("guard");
        let path = dir.path().to_path_buf();
        assert!(path.is_dir(), "{} must exist", path.display());
        std::fs::write(dir.join("rrouter.db"), b"x").expect("write");
        drop(dir);
        assert!(!path.exists(), "{} must be gone", path.display());
    }

    #[test]
    fn removing_twice_is_safe() {
        let dir = TempDir::new("guard");
        dir.purge();
        assert!(!dir.path().exists());
        dir.purge(); // Already gone: tolerated.
        drop(dir); // `Drop` purges once more.
    }

    #[test]
    fn a_directory_that_vanished_before_drop_is_tolerated() {
        let dir = TempDir::new("guard");
        let path = dir.path().to_path_buf();
        std::fs::remove_dir_all(&path).expect("remove");
        drop(dir);
        assert!(!path.exists());
    }
}
