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

use std::path::{Path, PathBuf};

/// A fresh, empty directory for one test to make a mess in.
///
/// `scope` names the module (`"attach"`, `"census"`) and `tag` the test. The
/// two, plus the process id, are what keep concurrently-running test binaries
/// out of each other's way: `cargo test` runs one process per crate and many
/// threads per process, so the pid alone separates the crates and the
/// scope/tag pair separates the threads. A stale directory from a previous run
/// is removed rather than reused, so a test never inherits state — which is
/// also why the path is stable across runs instead of random.
pub fn scratch(scope: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-{scope}-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
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
