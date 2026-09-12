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
//! For the same reason no device-local path ever is: `prov.yaml` describes the
//! archive, and the archive is device-independent — it is read on the laptop,
//! the phone, and the server that syncs it. `notes = ../notes` is true on
//! exactly one machine. Worse than being wrong elsewhere, it
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
use std::process::ExitCode;
use std::sync::OnceLock;

use prov::{Id, IdIndex, PeerLocation, PeerLookup, PeerResolver, link};

use crate::cli::PeerAction;
use crate::session::{Session, find_root_quiet_at};
use crate::{AnyError, CmdResult};

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
    ///
    /// ## Why this is not [`prov::open_peer`]
    ///
    /// The library's crossing does the same three steps and one of them
    /// differently, on purpose: it requires the recorded location to *be* a
    /// workspace root, where this confirms through [`find_root_quiet_at`],
    /// which climbs. So a peer recorded at a directory *inside* a workspace
    /// resolves here and is [`Unopenable`](prov::Refusal::Unopenable) there.
    ///
    /// Neither side moves. `prov peer add` records the directory the user named
    /// and this resolves what it recorded; a descent, which follows a reference
    /// it was not asked about into documents nobody looked at, holds the entry to
    /// the claim it makes — that the name belongs to a *root* — because
    /// promoting it to the enclosing workspace is how a reference lands in real
    /// documents in the wrong archive.
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
        let peer = find_root_quiet_at(root)
            .and_then(Session::over)
            .map_err(|e| DocumentError::Unopenable(root.clone(), e.to_string()))?;
        let path = peer
            .ws
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
        match find_root_quiet_at(root) {
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
/// A failure here is reported rather than swallowed: the user asked for this
/// write in so many words (`prov peer add`), so silently not doing it would be
/// a lie. Goes through a temporary sibling and a rename for the usual reason —
/// an interrupted write leaves the previous map rather than a truncated one.
pub(crate) fn store(peers: &BTreeMap<String, PathBuf>) -> Result<(), AnyError> {
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

/// `prov peer` — inspect and edit this device's map of other workspaces.
///
/// Deliberately does **not** need a workspace root: the map is a property of the
/// machine, and a user setting one up has often not `cd`'d anywhere in
/// particular. `peer resolve` is the one action that opens a workspace, and the
/// one it opens is the *peer*, never the current directory.
pub(crate) fn cmd_peer(action: PeerAction) -> CmdResult {
    match action {
        PeerAction::List => {
            let Some(file) = path() else {
                println!("(no peer map)");
                eprintln!(
                    "no peer-map location for this invocation — no config directory could be \
                     determined.\n\
                     \n  Set one with --peers <FILE> or PROV_PEERS. Cross-workspace references \
                     work either way;\n  without a map they are carried but cannot be followed."
                );
                return Ok(ExitCode::SUCCESS);
            };
            let peers = load();
            // The entries to stdout and the commentary to stderr, so `prov peer
            // list` pipes cleanly — the convention the other commands follow.
            for (name, root) in &peers {
                println!("{name}\t{}", root.display());
            }
            if peers.is_empty() {
                eprintln!(
                    "no peers recorded ({})\n\
                     \n  Add one with `prov peer add <name> <dir>`, where <name> is what that\n  \
                     workspace calls itself (`prov config workspace_id` there).",
                    file.display()
                );
            } else {
                eprintln!("{} peer(s) — {}", peers.len(), file.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        PeerAction::Add { name, dir } => {
            if !prov::is_valid_workspace_id(&name) {
                return Err(format!(
                    "`{name}` is not a valid workspace name — it cannot be empty or contain \
                     `/`, `:` or whitespace"
                )
                .into());
            }
            // Absolute, so the map means the same thing from every directory the
            // CLI is later run in. A peer map full of relative paths would
            // resolve differently per invocation, which is exactly the failure
            // mode that keeps it out of `prov.yaml` in the first place.
            let dir = dir
                .canonicalize()
                .map_err(|e| format!("{}: {e}", dir.display()))?;
            // Discovering the peer's root is what turns "a directory" into "a
            // workspace", and it is the first chance to notice that the name
            // being recorded is not the name that workspace answers to. It is no
            // longer the *last* chance — `peer resolve` asks again at the moment
            // it matters, because a line true when it was written can be stale by
            // the time it is followed — so this is advice, given early, and the
            // entry is recorded either way.
            let location = prov::PeerLocation::Path(dir.clone());
            match find_root_quiet_at(&dir) {
                Ok(peer_ctx) => {
                    // The same constructor the resolver uses, so `add` and
                    // `resolve` cannot come to different conclusions about the
                    // same directory.
                    match prov::PeerLookup::confirm(&name, location, &peer_ctx.config.workspace_id)
                    {
                        prov::PeerLookup::Confirmed(_) => {}
                        prov::PeerLookup::Unconfirmed { .. } => eprintln!(
                            "warning: the workspace at {} does not name itself — set \
                             `workspace_id` there\n  (`prov -C {} config workspace_id {name}`), \
                             or references written `id:{name}/<id>` will not be recognized as \
                             local when read inside it",
                            dir.display(),
                            dir.display()
                        ),
                        prov::PeerLookup::Mismatched { declares, .. } => eprintln!(
                            "warning: the workspace at {} calls itself `{declares}`, not \
                             `{name}` — references to it will be written `id:{declares}/<id>`, \
                             and `prov peer resolve id:{name}/<id>` will refuse this entry \
                             rather than follow it",
                            dir.display()
                        ),
                        prov::PeerLookup::Unknown => {
                            unreachable!("confirm never answers Unknown — it is given a location")
                        }
                    }
                }
                Err(e) => {
                    // Recorded anyway: a peer that is not a workspace *yet* is a
                    // reasonable thing to write down, and refusing would make the
                    // order of setup steps load-bearing.
                    eprintln!("warning: {}: {e}", dir.display());
                }
            }
            let mut peers = load();
            let previous = peers.insert(name.clone(), dir.clone());
            store(&peers)?;
            match previous {
                Some(old) if old != dir => {
                    eprintln!("{name} → {} (was {})", dir.display(), old.display())
                }
                _ => eprintln!("{name} → {}", dir.display()),
            }
            Ok(ExitCode::SUCCESS)
        }
        PeerAction::Remove { name } => {
            let mut peers = load();
            if peers.remove(&name).is_none() {
                eprintln!("no peer named `{name}`");
                return Ok(ExitCode::FAILURE);
            }
            store(&peers)?;
            eprintln!("removed `{name}` — references to it are still carried, just not followable");
            Ok(ExitCode::SUCCESS)
        }
        PeerAction::Resolve {
            reference,
            unverified,
        } => cmd_peer_resolve(&reference, unverified),
    }
}

/// One line saying where a peer is, or why it is not somewhere prov will go.
///
/// Every case names the location it found, including the ones it refuses: a
/// reader told only "cannot follow" has no way to see that the entry points at
/// the workspace next door.
pub(crate) fn describe_peer(lookup: &prov::PeerLookup, workspace: &str) -> String {
    match lookup {
        prov::PeerLookup::Confirmed(location) => {
            format!("`{location}`, per this device's peer map")
        }
        prov::PeerLookup::Unconfirmed { location, why } => {
            format!("`{location}`, but {why} (`--unverified` to follow it anyway)")
        }
        prov::PeerLookup::Mismatched { location, declares } => format!(
            "the peer map says `{location}`, but that workspace calls itself \
             `{declares}` — not followed (`prov peer add {workspace} <dir>` to correct it)"
        ),
        prov::PeerLookup::Unknown => format!(
            "no peer named `{workspace}` on this device (`prov peer add {workspace} <dir>`)"
        ),
    }
}

/// `prov peer resolve` — turn `id:<workspace>/<id>` into a file on this device.
///
/// The whole cross-workspace design in one command: the library parsed the
/// reference and stopped at "workspace `notes`, id `ajp7eq`"; everything past
/// that point is this device's peer map plus the *peer's own* registry. Nothing
/// here consults the current workspace at all.
///
/// The map is checked here rather than trusted here. A line recorded when it was
/// true and stale by now points at a directory that is some *other* workspace,
/// and following it would print a path to real documents in the wrong archive —
/// a wrong answer that looks exactly like a right one. So the peer is asked what
/// it calls itself, and a disagreement stops the command.
fn cmd_peer_resolve(reference: &str, unverified: bool) -> CmdResult {
    // Tolerate a bare `notes/ajp7eq` as well as the written `id:notes/ajp7eq`,
    // since the former is what a person reads off a screen.
    let written = if prov::link::strip_id_scheme(reference).is_some() {
        reference.to_string()
    } else {
        format!("{}{reference}", prov::link::ID_SCHEME)
    };
    let Some((peer_name, id)) = link::Link::parse(&written).foreign_target() else {
        return Err(format!(
            "`{reference}` is not a cross-workspace reference — expected `<workspace>/<id>`"
        )
        .into());
    };
    match PeerMap::load().resolve_document(&peer_name, &id, unverified) {
        Ok(path) => {
            println!("{}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        Err(DocumentError::Unfollowable(lookup)) => Err(describe_peer(&lookup, &peer_name).into()),
        Err(DocumentError::Unopenable(root, why)) => {
            Err(format!("{}: {why}", root.display()).into())
        }
        Err(DocumentError::Unregistered(root)) => Err(format!(
            "`{id}` is not registered in the workspace at {}",
            root.display()
        )
        .into()),
    }
}
