//! The filesystem scratch helpers every test module in this workspace was
//! writing out for itself.
//!
//! Sixteen modules across six crates carried the same `tempdir`, and ten the
//! same `write` — identical but for a name prefix, which is exactly the kind of
//! duplication that drifts. When one copy learns to create a parent directory
//! and the others do not, a test fails for a reason that has nothing to do with
//! what it is testing.
//!
//! Deliberately dependency-free, and deliberately unaware of prov. It knows
//! about directories and bytes; a helper that built a `Workspace` would have to
//! depend on `prov`, which would put a cycle between this crate and the read
//! core it is meant to test. The `ws(dir)` one-liners stay where they are, next
//! to the types they name.
//!
//! `publish = false`: this is scaffolding, so it never goes to crates.io, and
//! the path-only dev-dependency on it is stripped from a published manifest.

use std::ffi::OsStr;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A fresh, empty directory for one test to make a mess in.
///
/// `scope` names the module (`"attach"`, `"census"`) and `tag` the test; they
/// are there for whoever reads the path of a directory a failing test left. The
/// process id and a count of this process's directories are what keep tests out
/// of each other's way: `cargo test` runs one process per crate and many
/// threads per process, and a [`Scratch`] removes what it holds, so no two may
/// hold one path. Anything already at the path, left by a process that had this
/// id before, is removed rather than reused, so a test never inherits state.
pub fn scratch(scope: &str, tag: &str) -> Scratch {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let made = MADE.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("prov-{scope}-{tag}-{}-{made}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    Scratch { path }
}

/// A directory from [`scratch`], which goes when this does.
///
/// Derefs to [`Path`], so it reads as one. Hold it for as long as the test
/// needs the directory: `scratch(..).join("x")` keeps a path and drops the
/// directory. A thread that is panicking keeps the directory, because what a
/// failing test left there is the state that says why.
#[derive(Debug)]
pub struct Scratch {
    path: PathBuf,
}

impl Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<OsStr> for Scratch {
    fn as_ref(&self) -> &OsStr {
        self.path.as_os_str()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Write `contents` to `rel` under `dir`, creating any directories on the way.
///
/// Takes `impl AsRef<[u8]>` so a fixture can be written as a `&str` or as raw
/// bytes without two functions; the callers that needed each were the reason
/// there were two spellings of this to begin with.
pub fn write(dir: &Path, rel: &str, contents: impl AsRef<[u8]>) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// Read `rel` under `dir` as UTF-8. Panics if it is missing or not text —
/// which in a test is the report you want, at the line that expected it.
pub fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

/// Read `rel` under `dir` as raw bytes, for the fixtures that are not text.
pub fn read_bytes(dir: &Path, rel: &str) -> Vec<u8> {
    std::fs::read(dir.join(rel)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_passing_test_leaves_nothing_behind() {
        let dir = scratch("testkit", "passing");
        let path = dir.to_path_buf();
        write(&dir, "a/b.txt", "kept until the guard goes");
        assert!(path.join("a/b.txt").is_file());
        drop(dir);
        assert!(!path.exists(), "{} outlived its guard", path.display());
    }

    #[test]
    fn a_failing_test_keeps_its_directory_to_be_read() {
        let mut kept = None;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let dir = scratch("testkit", "failing");
            write(&dir, "evidence.txt", "why it failed");
            kept = Some(dir.to_path_buf());
            panic!("the assertion that failed");
        }));
        assert!(outcome.is_err());
        let kept = kept.unwrap();
        let survived = kept.join("evidence.txt").is_file();
        let _ = std::fs::remove_dir_all(&kept);
        assert!(survived, "a failing test lost {}", kept.display());
    }

    #[test]
    fn two_directories_with_one_name_stay_apart() {
        // A guard removes what it holds, so sharing a path would let one test
        // pull the directory out from under another.
        let first = scratch("testkit", "same");
        let second = scratch("testkit", "same");
        assert_ne!(first.to_path_buf(), second.to_path_buf());
        drop(first);
        assert!(second.is_dir());
    }
}
