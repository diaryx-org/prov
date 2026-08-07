//! Where this device keeps what it remembers about a workspace, and nothing
//! more than that.
//!
//! [`prov::FixityCache`] is deliberately ignorant of files: it decodes from
//! bytes and encodes to bytes, because the library has no business knowing about
//! a location outside the workspace it was pointed at. This module is the other
//! half — the CLI's answer to "outside where, exactly?".
//!
//! ## Why not in the workspace, and why not in `prov.yaml`
//!
//! The cache does not go **in** the workspace for the reason nothing derived and
//! device-specific does: it would be one more file every sync transport carries,
//! two devices would write it from opposite sides, and `check`'s orphan scan
//! would have to learn to forgive it. A prov workspace is an archive that
//! explains itself; a binary cache is not part of that explanation.
//!
//! Its *location* is likewise not a workspace setting. `prov.yaml` describes the
//! archive, and the archive is device-independent — it is read on the laptop,
//! the phone and the server that syncs it. A path baked in there is wrong on the
//! second machine that reads it. So the location is a property of the
//! invocation: a flag, an environment variable, or the platform's own convention
//! for cache data, in that order.
//!
//! ## What losing it costs
//!
//! One slow capture. Every failure here — no home directory, an unwritable
//! directory, a file this build cannot parse, a cache written for another
//! workspace — resolves to "no cache", never to an error the user has to deal
//! with. A cache that can fail a command is worse than no cache at all.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use prov::FixityCache;

/// The suffix that names these files for what they are, for anyone who looks in
/// the cache directory and wonders.
const EXT: &str = "fixity";

/// How this invocation was told to treat the cache. Resolved once in `main`,
/// before any command runs.
#[derive(Debug, Default)]
struct Policy {
    /// Where cache files live, or `None` when there is nowhere to put them.
    dir: Option<PathBuf>,
    /// `--no-cache`: read nothing, write nothing, hash everything.
    disabled: bool,
}

static POLICY: OnceLock<Policy> = OnceLock::new();

/// Resolve the cache policy for this invocation. Called once, from `main`.
///
/// The order is flag, environment, platform convention — the same shape as
/// `-C`/`PROV_ROOT`, so the two globals behave alike:
///
/// 1. `--cache-dir <DIR>`
/// 2. `PROV_CACHE_DIR`
/// 3. `XDG_CACHE_HOME/prov` — honored on every platform, because a user who has
///    set it has said where cache data goes
/// 4. `~/Library/Caches/prov` on macOS, `~/.cache/prov` elsewhere
/// 5. nothing, if none of those can be determined — the cache simply does not
///    happen
pub(crate) fn init(flag: Option<PathBuf>, no_cache: bool) {
    let dir = flag
        .or_else(|| std::env::var_os("PROV_CACHE_DIR").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join("prov"))
        })
        .or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from)?;
            Some(if cfg!(target_os = "macos") {
                home.join("Library/Caches/prov")
            } else {
                home.join(".cache/prov")
            })
        });
    let _ = POLICY.set(Policy {
        dir,
        disabled: no_cache,
    });
}

fn policy() -> &'static Policy {
    POLICY.get_or_init(Policy::default)
}

/// The cache file for the workspace rooted at `root_dir`, or `None` when this
/// invocation has nowhere to keep one.
///
/// The name carries both a readable stem and a digest of the canonical root
/// path. The digest is the actual key — two workspaces called `notes` must not
/// collide — and the stem is there so that a person listing the directory can
/// tell which of their workspaces each file belongs to, instead of facing a
/// column of hashes.
pub(crate) fn path_for(root_dir: &Path) -> Option<PathBuf> {
    let policy = policy();
    if policy.disabled {
        return None;
    }
    let dir = policy.dir.as_ref()?;
    let canonical = canonical(root_dir);
    let digest = prov::fixity::digest(canonical.to_string_lossy().as_bytes());
    let key = digest.strip_prefix("sha256:").unwrap_or(&digest);
    let stem = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .map(|n| {
            n.chars()
                .map(|c| if c.is_alphanumeric() { c } else { '-' })
                .collect::<String>()
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "workspace".to_string());
    Some(dir.join(format!("{stem}-{}.{EXT}", &key[..12])))
}

/// What this device remembers about the workspace at `root_dir`.
///
/// Always returns a usable cache when caching is on at all — an empty one when
/// there is nothing on disk, nothing readable, or nothing written for this
/// workspace. The three are the same answer, and none of them is a problem.
pub(crate) fn load(root_dir: &Path) -> Option<FixityCache> {
    let root = canonical(root_dir);
    let file = path_for(root_dir)?;
    let cache = std::fs::read(&file)
        .ok()
        .and_then(|bytes| FixityCache::decode(&bytes, &root))
        .unwrap_or_else(|| FixityCache::new(&root));
    Some(cache)
}

/// Write back what was learned, if anything was.
///
/// Silent on failure by design: an unwritable cache directory is not a reason to
/// fail the command a user actually asked for, it is a reason for the next one
/// to be slow.
pub(crate) fn store(root_dir: &Path, cache: Option<FixityCache>) {
    let Some(cache) = cache else { return };
    if !cache.is_dirty() {
        return;
    }
    let Some(file) = path_for(root_dir) else {
        return;
    };
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Through a temporary sibling and a rename, so an interrupted write leaves
    // the previous cache rather than a truncated one — the same reason
    // everything prov writes into a workspace goes through `write_atomic`. A
    // torn cache would be *detected* (the decoder bounds-checks everything), but
    // detected damage still costs the whole file.
    let tmp = file.with_extension(format!("{EXT}.tmp"));
    if std::fs::write(&tmp, cache.encode()).is_ok() && std::fs::rename(&tmp, &file).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Forget everything remembered about the workspace at `root_dir`. Returns
/// whether a file was actually removed.
pub(crate) fn clear(root_dir: &Path) -> bool {
    path_for(root_dir).is_some_and(|file| std::fs::remove_file(file).is_ok())
}

/// The root path as the cache keys itself on: resolved through symlinks where
/// possible, so the same workspace reached by two names is one cache rather than
/// two.
fn canonical(root_dir: &Path) -> PathBuf {
    std::fs::canonicalize(root_dir).unwrap_or_else(|_| root_dir.to_path_buf())
}
