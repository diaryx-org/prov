//! prov's peer port — where the *other* workspaces are.
//!
//! [`Target::Foreign`](crate::graph::Target::Foreign) is where resolution
//! stops. It can tell you that a reference names the workspace `notes` and the
//! id `ajp7eq`; it cannot tell you where `notes` is, and deliberately does not
//! try, because that map is a property of the device doing the reading rather
//! than of the archive being read. The same reference resolves to a directory
//! on one machine, a URL on another, and nothing at all on a third.
//!
//! This module is the seam between those two halves. It does not hold a map and
//! never will. It declares the *shape* a host's map answers in — the third port
//! beside [`fs::ReadStorage`](crate::fs::ReadStorage) and
//! [`index::IdIndex`](crate::index::IdIndex), and the smallest of the three.
//!
//! ## Why a port at all, if prov holds no map
//!
//! Because two things about cross-workspace resolution *are* prov's, and before
//! this module both were re-decided per host:
//!
//! 1. **What an answer is.** `prov-cli` answers with a directory on this disk;
//!    diaryx answers with a published ARK permalink. Those are one type
//!    ([`PeerLocation`]), and a consumer that can render either — an export
//!    writing an `href`, a viewer offering to follow a link — should not need to
//!    know which host it is talking to.
//!
//! 2. **What makes an answer trustworthy.** This is the load-bearing half. A
//!    peer map is a claim about a name, and a wrong claim does not fail: it
//!    resolves to *real documents in the wrong archive*. That failure mode is
//!    the whole reason there is no peer table in `prov.yaml` — but it is also
//!    fixable, because a prov workspace declares its own name
//!    ([`workspace_id`](crate::graph::ReadSettings::workspace_id)). Comparing
//!    the name asked for against the name found is a check only prov can
//!    specify, and [`PeerLookup::confirm`] is where it happens, so no host
//!    decides for itself what "confirmed" means.
//!
//! ## Where this port stops
//!
//! At an address. Nothing here opens a workspace, and nothing here can: reading
//! the peer would need a second [`ReadStorage`](crate::fs::ReadStorage) and a
//! second [`IdIndex`](crate::index::IdIndex), which only the host has. So a
//! resolver hands back *where*, the host does the opening, and the layering the
//! rest of this crate keeps — a read core that cannot reach past its own root —
//! is undisturbed.
//!
//! That is also why no method on [`Graph`](crate::graph::Graph) takes a
//! resolver and why `Graph` grows no third type parameter. Following a foreign
//! reference is a *second step* after resolution, taken by a caller that wants
//! it, not a deeper mode of the first one. A traversal that never follows one
//! pays nothing, and the read core's generics stay two wide.

use std::path::PathBuf;

use crate::identity::Id;

/// An address a resolver answers with: somewhere on this device, or somewhere
/// on the network.
///
/// Both spellings are first-class. A peer that is a sibling directory and a
/// peer that is a published site are the same kind of fact — "the host says it
/// is over there" — and a consumer that handles only one of them would work for
/// `prov-cli` and not for diaryx, or the reverse.
///
/// The same type answers for a workspace ([`PeerResolver::locate`], where it is
/// the workspace root) and for one document inside it
/// ([`PeerResolver::locate_document`], where it is the file). They are the same
/// two spellings and nothing distinguishes them but which question was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerLocation {
    /// A path on the device doing the reading. Absolute by convention: a
    /// relative one would mean something different from each directory the host
    /// is later run in, which is the per-device failure this whole design is
    /// arranged around.
    Path(PathBuf),
    /// A URL. Never fetched here — prov does no network I/O — so this is
    /// carried and handed back exactly as the host spelled it.
    Url(String),
}

impl std::fmt::Display for PeerLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(path) => write!(f, "{}", path.display()),
            Self::Url(url) => write!(f, "{url}"),
        }
    }
}

/// Why a location on record could not be confirmed to be the workspace that was
/// asked for.
///
/// Kept apart because what the reader has to *do* about them differs, and
/// because two of the three are ordinary rather than wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unconfirmed {
    /// Nothing at the location could be opened as a workspace — the directory
    /// is gone, or is not a workspace yet. Recording a peer before creating it
    /// is reasonable, so this is a state to report, not an error to raise.
    Unreadable,
    /// The workspace is there and readable, but anonymous — it declares no
    /// `workspace_id`, so there is nothing to compare the asked-for name
    /// against. The fix belongs in the *peer*, not in the map.
    Anonymous,
    /// The resolver did not look. A [`PeerLocation::Url`] is the usual reason:
    /// confirming one means a network round trip, which a synchronous resolver
    /// will not take and prov would not take anywhere.
    NotChecked,
}

impl std::fmt::Display for Unconfirmed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable => f.write_str("no workspace could be read there"),
            Self::Anonymous => f.write_str("that workspace does not name itself"),
            Self::NotChecked => f.write_str("its name was not checked"),
        }
    }
}

/// What a host knows about where one workspace is — and how sure it is.
///
/// Note what is *not* here: an error case. A peer that cannot be found, cannot
/// be read, or turns out to be someone else is never a failure, because a
/// foreign reference is carried whether or not it resolves. Every variant is an
/// answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerLookup {
    /// A location, and the workspace there declares the name that was asked
    /// for. The only variant [`followable`](PeerLookup::followable) returns.
    Confirmed(PeerLocation),
    /// A location whose occupant could not be checked against the name. Usable
    /// on the reader's say-so ([`followable_unverified`]), not on prov's.
    ///
    /// [`followable_unverified`]: PeerLookup::followable_unverified
    Unconfirmed {
        /// Where the host says the workspace is.
        location: PeerLocation,
        /// Why the claim could not be checked.
        why: Unconfirmed,
    },
    /// A location occupied by a workspace that calls itself something else.
    ///
    /// This is the failure the design exists to prevent, caught: the map says
    /// `notes` is here, the workspace here says it is `journal`, and following
    /// that would land every `id:notes/…` reference on real documents in the
    /// wrong archive. Never followable, by either accessor, at any insistence —
    /// there is no reader preference that makes a known-wrong answer right.
    Mismatched {
        /// Where the host says the workspace is.
        location: PeerLocation,
        /// The name the workspace found there actually declares.
        declares: String,
    },
    /// No location on record. The ordinary state — most workspaces have never
    /// heard of most other workspaces.
    Unknown,
}

impl PeerLookup {
    /// Weigh a location against what the workspace there calls itself. **This
    /// is the verification**, and a resolver that has read the peer's config
    /// should reach [`Confirmed`](PeerLookup::Confirmed) only through here.
    ///
    /// `declares` is the peer's own
    /// [`workspace_id`](crate::graph::ReadSettings::workspace_id), empty when it
    /// is anonymous — the same convention that field already uses, so a host can
    /// pass it straight through without deciding what an empty name means.
    ///
    /// Placing the comparison in a constructor is the point. A host that made
    /// this judgment itself would be free to accept a near-miss, or to skip the
    /// check on a fast path and still say `Confirmed`; here the only way to
    /// claim confirmation is to have the evidence in hand at the call.
    pub fn confirm(asked: &str, location: PeerLocation, declares: &str) -> Self {
        if declares.is_empty() {
            Self::Unconfirmed {
                location,
                why: Unconfirmed::Anonymous,
            }
        } else if declares == asked {
            Self::Confirmed(location)
        } else {
            Self::Mismatched {
                location,
                declares: declares.to_string(),
            }
        }
    }

    /// A location whose workspace could not be opened at all.
    pub fn unreadable(location: PeerLocation) -> Self {
        Self::Unconfirmed {
            location,
            why: Unconfirmed::Unreadable,
        }
    }

    /// A location the resolver did not check — a URL, typically.
    pub fn unchecked(location: PeerLocation) -> Self {
        Self::Unconfirmed {
            location,
            why: Unconfirmed::NotChecked,
        }
    }

    /// The location to follow, or `None`. `Some` only when the workspace there
    /// answered to the name asked for.
    ///
    /// This is the strict accessor and the default one. A caller that reaches
    /// for it cannot resolve into the wrong archive, because the archive
    /// confirmed it is the right one.
    pub fn followable(&self) -> Option<&PeerLocation> {
        match self {
            Self::Confirmed(location) => Some(location),
            _ => None,
        }
    }

    /// The location to follow when the reader has accepted an unconfirmed one —
    /// an anonymous peer, or a URL nothing local can check.
    ///
    /// Still `None` for [`Mismatched`](PeerLookup::Mismatched). The escape is
    /// for *absent* evidence, never for evidence pointing the other way.
    pub fn followable_unverified(&self) -> Option<&PeerLocation> {
        match self {
            Self::Confirmed(location) | Self::Unconfirmed { location, .. } => Some(location),
            Self::Mismatched { .. } | Self::Unknown => None,
        }
    }

    /// Every location on record, followable or not — for saying *why* a
    /// reference did not resolve. A diagnostic must be able to name the
    /// mismatched directory; that is the whole content of the complaint.
    pub fn location(&self) -> Option<&PeerLocation> {
        match self {
            Self::Confirmed(location)
            | Self::Unconfirmed { location, .. }
            | Self::Mismatched { location, .. } => Some(location),
            Self::Unknown => None,
        }
    }

    /// Whether the host has no location on record at all.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}

/// A host's map from a workspace name to where that workspace is.
///
/// This is the trait a host implements to make foreign references followable.
/// It is not generic over anything and takes `&self` throughout, so it is
/// dyn-compatible: a consumer can hold `&dyn PeerResolver` and be handed
/// `prov-cli`'s device-local peer file, diaryx's ARK resolution, or
/// [`NoPeers`], without being written twice.
///
/// **The implementor's obligation** is the one prov cannot enforce from here:
/// build every answer through [`PeerLookup::confirm`] when the peer's own name
/// is readable, and through [`unreadable`](PeerLookup::unreadable) /
/// [`unchecked`](PeerLookup::unchecked) when it is not. Returning
/// `Confirmed` for a location whose occupant was never read is the one way to
/// defeat this design, and it takes deliberate effort.
pub trait PeerResolver {
    /// Where the workspace named `workspace` is.
    ///
    /// A name that is not [well-formed](crate::link::is_valid_workspace_id)
    /// has no workspace to find and should answer
    /// [`Unknown`](PeerLookup::Unknown) rather than be looked up: it cannot be
    /// any workspace's `workspace_id`, so a map entry matching it was
    /// hand-written wrong.
    fn locate(&self, workspace: &str) -> PeerLookup;

    /// Where one *document* in that workspace is — the whole reference answered
    /// at once, rather than the workspace it lives in.
    ///
    /// Defaults to `None`, because most resolvers cannot answer it. Turning
    /// `id:notes/ajp7eq` into a file means opening `notes` and reading *its*
    /// registry, which the host can do and this crate cannot; turning it into a
    /// permalink means knowing that host's URL scheme. A caller that gets
    /// `None` falls back to [`locate`](PeerResolver::locate) and does the
    /// opening itself.
    ///
    /// The same obligation applies twice over: answer only for a workspace
    /// whose identity you have confirmed. There is no [`PeerLookup`] wrapper on
    /// this one to carry the doubt in, so an unconfirmed answer here is
    /// indistinguishable from a confirmed one.
    fn locate_document(&self, workspace: &str, id: &Id) -> Option<PeerLocation> {
        let _ = (workspace, id);
        None
    }
}

impl<T: PeerResolver + ?Sized> PeerResolver for &T {
    fn locate(&self, workspace: &str) -> PeerLookup {
        (**self).locate(workspace)
    }

    fn locate_document(&self, workspace: &str, id: &Id) -> Option<PeerLocation> {
        (**self).locate_document(workspace, id)
    }
}

/// No peers — every workspace is somewhere this host cannot see.
///
/// The honest default rather than a degenerate one: it is what a workspace with
/// no configured map already behaves like, and what every consumer that has not
/// been given a resolver should use. Mirrors [`NoIndex`](crate::index::NoIndex).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPeers;

impl PeerResolver for NoPeers {
    fn locate(&self, _workspace: &str) -> PeerLookup {
        PeerLookup::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(path: &str) -> PeerLocation {
        PeerLocation::Path(PathBuf::from(path))
    }

    #[test]
    fn a_workspace_answering_to_the_name_asked_for_is_confirmed() {
        assert_eq!(
            PeerLookup::confirm("notes", dir("/vaults/notes"), "notes"),
            PeerLookup::Confirmed(dir("/vaults/notes"))
        );
    }

    #[test]
    fn a_workspace_calling_itself_something_else_is_never_followable() {
        // The failure this design exists to prevent: the map says `notes`, the
        // archive says `journal`, and following it would resolve every
        // `id:notes/…` reference to real documents in the wrong workspace.
        let lookup = PeerLookup::confirm("notes", dir("/vaults/journal"), "journal");
        assert_eq!(
            lookup,
            PeerLookup::Mismatched {
                location: dir("/vaults/journal"),
                declares: "journal".into(),
            }
        );
        assert_eq!(lookup.followable(), None);
        // And not on insistence either — the reader's escape hatch is for
        // missing evidence, not for evidence pointing the other way.
        assert_eq!(lookup.followable_unverified(), None);
        // But it is still nameable, because the diagnostic *is* the directory.
        assert_eq!(lookup.location(), Some(&dir("/vaults/journal")));
    }

    #[test]
    fn an_anonymous_peer_is_unconfirmed_rather_than_mismatched() {
        // Nothing to compare against is not the same as comparing and
        // disagreeing: the peer may well be `notes`, it just has not said so.
        // So the strict accessor declines and the permissive one allows.
        let lookup = PeerLookup::confirm("notes", dir("/vaults/notes"), "");
        assert_eq!(
            lookup,
            PeerLookup::Unconfirmed {
                location: dir("/vaults/notes"),
                why: Unconfirmed::Anonymous,
            }
        );
        assert_eq!(lookup.followable(), None);
        assert_eq!(lookup.followable_unverified(), Some(&dir("/vaults/notes")));
    }

    #[test]
    fn an_unchecked_url_is_followable_only_unverified() {
        let lookup = PeerLookup::unchecked(PeerLocation::Url("https://diaryx.org".into()));
        assert_eq!(lookup.followable(), None);
        assert!(lookup.followable_unverified().is_some());
        assert!(!lookup.is_unknown());
    }

    #[test]
    fn an_unknown_peer_yields_no_location_at_all() {
        let lookup = PeerLookup::Unknown;
        assert!(lookup.is_unknown());
        assert_eq!(lookup.location(), None);
        assert_eq!(lookup.followable_unverified(), None);
    }

    #[test]
    fn no_peers_knows_nothing_and_offers_no_documents() {
        let id = Id("ajp7eq".into());
        assert_eq!(NoPeers.locate("notes"), PeerLookup::Unknown);
        assert_eq!(NoPeers.locate_document("notes", &id), None);
    }

    /// A resolver held behind `&dyn` works, which is the whole reason the trait
    /// takes no generics: one consumer serves `prov-cli`'s peer file and
    /// diaryx's ARK resolution without being written twice.
    #[test]
    fn a_resolver_is_usable_through_a_trait_object() {
        struct One;
        impl PeerResolver for One {
            fn locate(&self, workspace: &str) -> PeerLookup {
                PeerLookup::confirm(workspace, dir("/vaults/notes"), "notes")
            }
        }
        let erased: &dyn PeerResolver = &One;
        assert!(erased.locate("notes").followable().is_some());
        assert_eq!(erased.locate("other").followable(), None);
        // And the blanket impl for references composes with it, so a caller
        // holding `&&dyn` or a plain `&One` is not a different consumer.
        fn ask(peers: impl PeerResolver) -> bool {
            peers.locate("notes").followable().is_some()
        }
        assert!(ask(erased));
        assert!(ask(&One));
    }
}
