//! The device-local peer file — where the *other* workspaces are, as a
//! [`PeerResolver`] any host can load.
//!
//! [`Target::Foreign`](crate::Target::Foreign) is where the graph stops: it can
//! say a reference names the workspace `notes` and the id `ajp7eq`, and
//! deliberately not where `notes` is. Resolution is the host's, and the
//! reasoning in `docs/reference-styles.md` still holds — a location is a fact
//! about a disk, so it never goes in `prov.yaml`. What this module adds is the
//! second half shipped once: the file `prov peer add` writes, readable by any
//! program that wants to follow the same map the CLI does, rather than each of
//! them parsing it again.
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
//! Split on the first whitespace run, because a workspace name can never
//! contain whitespace ([`is_valid_workspace_id`]) and a path very well may. A
//! malformed line is skipped rather than failing the load — one bad line should
//! not cost the other peers — and a missing, unreadable or empty file are all
//! the same answer: no peers, which is a state and not an error.
//!
//! ## The map is a claim, and claims are checked
//!
//! A line here says "the workspace named `notes` is at that path". It can be
//! wrong — hand-edited, or right until the directory was replaced — and a wrong
//! one does not fail: it resolves to real documents in the wrong archive. So
//! every answer [`PeerFile`] gives as a resolver goes through
//! [`PeerLookup::confirm`]: the peer is opened, its own `workspace_id` read, and
//! a workspace that calls itself something else is reported as
//! [`Mismatched`](PeerLookup::Mismatched) rather than followed.
//!
//! ## What is not here
//!
//! Writing. `prov peer add` and `remove` stay in the CLI, which is the one
//! program that edits the file; a library that could rewrite a device's peer
//! map from inside any host is more than a resolver needs to be.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use prov_graph::fs::StdFs;
use prov_graph::{Id, IdIndex, PeerLocation, PeerLookup, PeerResolver, block_on};

use crate::config::is_valid_workspace_id;
use crate::crossing::open_discovered;
use crate::discovery::{Discovery, discover};

/// The file's name inside whichever directory holds it.
pub const PEER_FILE: &str = "peers";

/// The environment variable naming the peer file outright.
pub const PEER_FILE_ENV: &str = "PROV_PEERS";

/// Where this device keeps its peer file, when that can be determined.
///
/// The order is the CLI's, so every program following the map reads the same
/// one:
///
/// 1. [`PEER_FILE_ENV`] (`PROV_PEERS`)
/// 2. `XDG_CONFIG_HOME/prov/peers` — honoured on every platform, because a user
///    who has set it has said where config data goes
/// 3. `~/Library/Application Support/prov/peers` on macOS, `~/.config/prov/peers`
///    elsewhere
/// 4. `None`, if none of those can be determined — there are simply no peers
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os(PEER_FILE_ENV)
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join("prov").join(PEER_FILE))
        })
        .or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from)?;
            Some(if cfg!(target_os = "macos") {
                home.join("Library/Application Support/prov")
                    .join(PEER_FILE)
            } else {
                home.join(".config/prov").join(PEER_FILE)
            })
        })
}

/// This device's peer file, loaded once and held — so a walk that meets a
/// dozen foreign references answers every one of them against the same map.
///
/// There is deliberately no unchecked `resolve(name) -> Option<PathBuf>` on
/// it. Following a peer means going through [`PeerResolver::locate`], and a
/// convenience that skipped the confirmation would be the shortest path for
/// every future call site — which is exactly how the check stops happening.
/// [`peers`](Self::peers) is for a caller that only *reports* the map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerFile {
    peers: BTreeMap<String, PathBuf>,
}

impl PeerFile {
    /// Parse the file's text. Never fails; see the module docs for what is
    /// skipped.
    pub fn parse(text: &str) -> Self {
        let mut peers = BTreeMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // A name can never contain whitespace, a path often does — so the
            // split is at the first run of it, and everything after is the path.
            let Some((name, root)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            let root = root.trim();
            if !is_valid_workspace_id(name) || root.is_empty() {
                continue;
            }
            peers.insert(name.to_string(), PathBuf::from(root));
        }
        Self { peers }
    }

    /// Read the file at `path`. A missing or unreadable file is an empty map.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(_) => Self::default(),
        }
    }

    /// Read this device's file, from [`default_path`] — or an empty map when
    /// the device has nowhere to keep one.
    pub fn from_device() -> Self {
        default_path()
            .map(|path| Self::load(&path))
            .unwrap_or_default()
    }

    /// The map as written: name → workspace root, for reporting.
    pub fn peers(&self) -> &BTreeMap<String, PathBuf> {
        &self.peers
    }

    /// Whether the file names nobody.
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

impl PeerResolver for PeerFile {
    /// The file, checked against the archive it points at.
    ///
    /// A name that could never be a `workspace_id` is not looked up at all: no
    /// workspace can declare it, so an entry matching it was hand-written wrong
    /// and confirming it would be impossible by construction.
    ///
    /// The peer is confirmed by *discovering* at the recorded path, which
    /// climbs: a directory recorded inside a workspace confirms as that
    /// workspace here. [`open_peer`](crate::open_peer) then holds the entry to
    /// the stricter claim that it names a root, and refuses one that does not —
    /// the two agree on every well-formed entry and differ only on that one,
    /// on purpose; see `docs/reference-styles.md`.
    fn locate(&self, workspace: &str) -> PeerLookup {
        if !is_valid_workspace_id(workspace) {
            return PeerLookup::Unknown;
        }
        let Some(root) = self.peers.get(workspace) else {
            return PeerLookup::Unknown;
        };
        let location = PeerLocation::Path(root.clone());
        // A peer that is not a workspace *yet* is a reasonable thing to have
        // written down, so failing to open it is a state, not an error.
        match block_on(discover(&StdFs, root)) {
            Ok(Discovery::Found(found)) => {
                PeerLookup::confirm(workspace, location, &found.config.workspace_id)
            }
            _ => PeerLookup::unreadable(location),
        }
    }

    /// The file a whole `id:<workspace>/<id>` reference names, absolute —
    /// because the answer is only useful outside the peer, where the caller is
    /// standing by construction. Strictly confirmed: the trait offers no
    /// wrapper to carry doubt in, so an unconfirmed peer answers `None`.
    fn locate_document(&self, workspace: &str, id: &Id) -> Option<PeerLocation> {
        let PeerLocation::Path(root) = self.locate(workspace).followable()?.clone() else {
            return None;
        };
        let Ok(Discovery::Found(found)) = block_on(discover(&StdFs, &root)) else {
            return None;
        };
        let peer = block_on(open_discovered(&StdFs, &found)).ok()?;
        let path = peer.index().resolve(id)?;
        Some(PeerLocation::Path(found.root_dir.join(path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_paths_comments_and_skips_what_it_cannot_use() {
        let file = PeerFile::parse(
            "# prov peer map\n\
             notes    /Users/me/vaults/notes\n\
             \n\
             diaryx /Users/me/Code/my diaryx\n\
             bad:name /x\n\
             lonely\n\
             empty   \n",
        );
        let peers = file.peers();
        assert_eq!(peers.len(), 2);
        assert_eq!(peers["notes"], PathBuf::from("/Users/me/vaults/notes"));
        // Everything after the first whitespace run is the path, spaces included.
        assert_eq!(peers["diaryx"], PathBuf::from("/Users/me/Code/my diaryx"));
    }

    #[test]
    fn a_malformed_name_is_unknown_without_a_lookup() {
        let file = PeerFile::parse("notes /nowhere\n");
        assert_eq!(file.locate("not a name"), PeerLookup::Unknown);
        assert_eq!(file.locate("absent"), PeerLookup::Unknown);
    }

    #[test]
    fn a_recorded_path_that_is_not_a_workspace_is_unreadable_not_unknown() {
        let dir = std::env::temp_dir().join(format!("prov-peers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = PeerFile::parse(&format!("notes {}\n", dir.display()));
        match file.locate("notes") {
            PeerLookup::Unconfirmed { location, .. } => {
                assert_eq!(location, PeerLocation::Path(dir.clone()));
            }
            other => panic!("expected an unconfirmed lookup, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_is_an_empty_map() {
        let file = PeerFile::load(Path::new("/definitely/not/here/peers"));
        assert!(file.is_empty());
    }
}
