//! The answers `prov` gives to [`SkipHost`], and what else depends on them.
//!
//! The *computation* — the walk, the diff, the region — is tested in
//! `prov_history` against a fixture host, so nothing there can come to depend
//! on what the skiplist is defined not to know. What belongs here is what
//! only exists here: the reachable set, the bookkeeping prefixes, and the
//! manifest claim as this workspace actually answers them, plus the parking
//! that keeps a historica store out of every workspace walk. A test belongs
//! in this module only if removing `prov` from it would remove its subject.

mod composition;
mod support;
