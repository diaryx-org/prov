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
//!
//! ## The map is a claim, and claims are checked
//!
//! A line in this file says "the workspace named `notes` is at that path." It
//! can be wrong — hand-edited, or right until the directory was replaced — and a
//! wrong one does not fail: it resolves to real documents in the wrong archive.
//! That is the reason this map is not in `prov.yaml`, and it does not stop being
//! the reason once the map is device-local.
//!
//! So the claim is checked *where it is used*, not once where it was recorded.
//! [`PeerMap`] is this crate's [`PeerResolver`], and every answer it gives comes
//! from [`PeerLookup::confirm`] — the peer is opened, its own `workspace_id` is
//! read, and a workspace that calls itself something else is reported as
//! [`Mismatched`](PeerLookup::Mismatched) instead of followed. `prov peer add`
//! still warns at record time, because catching it there is kinder; it is no
//! longer the only thing standing between a stale line and the wrong documents.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use prov::{Id, IdIndex, PeerLocation, PeerLookup, PeerResolver};

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

/// This device's peer file, as the port the library declares.
///
/// There is deliberately no unchecked `resolve(name) -> Option<PathBuf>` beside
/// this. Following a peer means going through [`PeerResolver::locate`], and a
/// convenience that skipped the confirmation would be the shortest path for
/// every future call site — which is exactly how the check stops happening.
/// Commands that only *report* the map read [`load`] directly.
///
/// Loaded once and held, so a command that meets a dozen foreign references
/// reads the file once rather than a dozen times — and, more to the point, so
/// every one of those references is answered against the same map.
pub(crate) struct PeerMap {
    peers: BTreeMap<String, PathBuf>,
}

impl PeerMap {
    /// Read this device's map.
    pub(crate) fn load() -> Self {
        Self { peers: load() }
    }

    /// Resolve a whole `id:<workspace>/<id>` reference to the file it names.
    ///
    /// Two openings of the peer, and both are load-bearing: the first confirms
    /// the workspace is the one the map claims, the second asks *its* registry
    /// where the id lives. Neither answer is available to the library — the
    /// second is another workspace's registry, which is exactly what
    /// `prov-graph` has no way to reach.
    ///
    /// `unverified` accepts an [`Unconfirmed`](prov::Unconfirmed) peer — an
    /// anonymous workspace, or a directory that could not be opened as one. It
    /// does not, and cannot, accept a mismatched one.
    pub(crate) fn resolve_document(
        &self,
        workspace: &str,
        id: &Id,
        unverified: bool,
    ) -> Result<PathBuf, DocumentError> {
        let lookup = self.locate(workspace);
        let location = if unverified {
            lookup.followable_unverified()
        } else {
            lookup.followable()
        };
        let Some(PeerLocation::Path(root)) = location else {
            return Err(DocumentError::Unfollowable(lookup));
        };
        let ctx = crate::find_root_quiet_at(root)
            .map_err(|e| DocumentError::Unopenable(root.clone(), e.to_string()))?;
        let peer_ws = crate::workspace(&ctx)
            .map_err(|e| DocumentError::Unopenable(root.clone(), e.to_string()))?;
        let path = peer_ws
            .index()
            .resolve(id)
            .ok_or_else(|| DocumentError::Unregistered(root.clone()))?;
        // Absolute, because the answer is only useful outside the peer
        // workspace — the caller is standing somewhere else by construction.
        Ok(root.join(path))
    }
}

/// Why a cross-workspace reference did not reach a file. Every case names the
/// location it got to, because the location *is* the complaint.
pub(crate) enum DocumentError {
    /// No peer, or one prov declines to follow. Carries the lookup so the
    /// caller can say which — an absent entry and a mismatched one need
    /// different advice.
    Unfollowable(PeerLookup),
    /// The peer is on record but could not be opened as a workspace.
    Unopenable(PathBuf, String),
    /// The peer opened, and its registry has never heard of the id.
    Unregistered(PathBuf),
}

impl PeerResolver for PeerMap {
    /// The peer file, checked against the archive it points at.
    ///
    /// A name that could never be a `workspace_id` is not looked up at all: no
    /// workspace can declare it, so an entry matching it was hand-written wrong
    /// and confirming it would be impossible by construction.
    fn locate(&self, workspace: &str) -> PeerLookup {
        if !prov::is_valid_workspace_id(workspace) {
            return PeerLookup::Unknown;
        }
        let Some(root) = self.peers.get(workspace) else {
            return PeerLookup::Unknown;
        };
        let location = PeerLocation::Path(root.clone());
        // A peer that is not a workspace *yet* is a reasonable thing to have
        // written down (`peer add` records one deliberately), so failing to
        // open it is a state, not an error.
        match crate::find_root_quiet_at(root) {
            Ok(ctx) => PeerLookup::confirm(workspace, location, &ctx.config.workspace_id),
            Err(_) => PeerLookup::unreadable(location),
        }
    }

    fn locate_document(&self, workspace: &str, id: &Id) -> Option<PeerLocation> {
        // Strict: the trait's contract is that an answer here carries no doubt,
        // and there is no wrapper on this return type to carry any in.
        self.resolve_document(workspace, id, false)
            .ok()
            .map(PeerLocation::Path)
    }
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
