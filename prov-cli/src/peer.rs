//! Where the *other* workspaces are — the half of cross-workspace linking that
//! is a fact about this device rather than about any archive.
//!
//! [`prov::Target::Foreign`] is where the library stops. It can tell you that a
//! reference names the workspace `notes` and the id `ajp7eq`; it cannot tell you
//! where `notes` is, and deliberately does not try. This module is the other
//! half — the CLI's answer to "which workspace, exactly?".
//!
//! ## Why the map is not in `prov.yaml`
//!
//! For the same reason the fixity cache's *location* is not (see [`crate::cache`]):
//! `prov.yaml` describes the archive, and the archive is device-independent — it
//! is read on the laptop, the phone, and the server that syncs it. `notes =
//! ../notes` is true on exactly one machine. Worse than being wrong elsewhere, it
//! would be wrong *silently*, since a peer that resolves to the wrong directory
//! resolves to real documents.
//!
//! The one piece that **is** device-independent is what a workspace calls
//! *itself* — [`WorkspaceConfig::workspace_id`](prov::WorkspaceConfig::workspace_id)
//! — and that is exactly the piece the library keeps. A name is a fact about an
//! archive; a location is a fact about a disk.
//!
//! ## The file
//!
//! One line per peer, `<name> <path>`, `#` for comments:
//!
//! ```text
//! # prov peer map
//! notes    /Users/me/vaults/notes
//! diaryx   /Users/me/Code/diaryx
//! ```
//!
//! Split on the first whitespace run, because a workspace name can never contain
//! whitespace ([`prov::is_valid_workspace_id`]) and a path very well may. Parsed
//! by hand, without a config crate, matching the rest of the CLI's local-state
//! handling.
//!
//! ## What losing it costs
//!
//! Nothing that was working stops working. A foreign reference is *carried*
//! whether or not it resolves — no `check` finding depends on this file, and no
//! command fails because a peer is missing. All that is lost is the ability to
//! follow a link, which is why every failure here reads as "no peer" rather than
//! as an error.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The file's name inside whichever directory holds it.
const FILE: &str = "peers";

static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Resolve where this invocation reads and writes the peer map. Called once,
/// from `main`.
///
/// The order matches `--cache-dir`/`PROV_CACHE_DIR` and `-C`/`PROV_ROOT`, so the
/// CLI's three device-local settings behave alike:
///
/// 1. `--peers <FILE>`
/// 2. `PROV_PEERS`
/// 3. `XDG_CONFIG_HOME/prov/peers` — honored on every platform, because a user
///    who has set it has said where config data goes
/// 4. `~/Library/Application Support/prov/peers` on macOS, `~/.config/prov/peers`
///    elsewhere
/// 5. nothing, if none of those can be determined — there are simply no peers
pub(crate) fn init(flag: Option<PathBuf>) {
    let path = flag
        .or_else(|| std::env::var_os("PROV_PEERS").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join("prov").join(FILE))
        })
        .or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from)?;
            Some(if cfg!(target_os = "macos") {
                home.join("Library/Application Support/prov").join(FILE)
            } else {
                home.join(".config/prov").join(FILE)
            })
        });
    let _ = PATH.set(path);
}

/// The peer-map file in use, or `None` when this invocation has nowhere to keep
/// one. Printed by `prov peer list`, so a user can find and hand-edit it.
pub(crate) fn path() -> Option<&'static Path> {
    PATH.get_or_init(|| None).as_deref()
}

/// Every peer this device knows, name → workspace root.
///
/// An unreadable file, a missing file and an empty one are the same answer —
/// no peers — and none of them is a problem. A malformed line is skipped rather
/// than failing the load: one bad line should not cost the other peers.
pub(crate) fn load() -> BTreeMap<String, PathBuf> {
    let mut peers = BTreeMap::new();
    let Some(file) = path() else { return peers };
    let Ok(text) = std::fs::read_to_string(file) else {
        return peers;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // A name can never contain whitespace, a path often does — so the split
        // is at the first run of it, and everything after is the path.
        let Some((name, root)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let root = root.trim();
        if !prov::is_valid_workspace_id(name) || root.is_empty() {
            continue;
        }
        peers.insert(name.to_string(), PathBuf::from(root));
    }
    peers
}

/// Where the workspace named `name` lives on this device, if anywhere.
pub(crate) fn resolve(name: &str) -> Option<PathBuf> {
    load().remove(name)
}

/// Write `peers` back, replacing the file.
///
/// Unlike [`crate::cache`], a failure here is reported: the user asked for this
/// write in so many words (`prov peer add`), so silently not doing it would be
/// a lie. Goes through a temporary sibling and a rename for the usual reason —
/// an interrupted write leaves the previous map rather than a truncated one.
pub(crate) fn store(peers: &BTreeMap<String, PathBuf>) -> Result<(), crate::AnyError> {
    let Some(file) = path() else {
        return Err(
            "no peer-map location on this device — pass --peers <FILE> or set PROV_PEERS"
                .to_string()
                .into(),
        );
    };
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = String::from(
        "# prov peer map — workspace name, then where it lives on this device.\n\
         # Managed by `prov peer add` / `prov peer remove`; safe to hand-edit.\n",
    );
    for (name, root) in peers {
        out.push_str(&format!("{name} {}\n", root.display()));
    }
    let tmp = file.with_extension("tmp");
    std::fs::write(&tmp, out)?;
    if let Err(e) = std::fs::rename(&tmp, file) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}
