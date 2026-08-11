//! History's behavioural tests — the store's own verbs, over a real filesystem.
//!
//! The pure-logic tests (manifest ordering, id minting, blob paths, index
//! rendering) live at the bottom of the module that owns them. These are the
//! ones that need a workspace on disk: a capture is only meaningful against a
//! reachable graph, a restore against files it can put back, a prune against
//! blobs something else still names.
//!
//! What they run against is [`host::TestHost`] — a host built to
//! [`HistoryReadHost`](crate::HistoryReadHost) and
//! [`HistoryWriteHost`](crate::HistoryWriteHost) and nothing else. That is the
//! line these tests are drawn on: everything here is phrased in the vocabulary
//! this crate owns — [`Captured`](crate::Captured),
//! [`RestorePlan`](crate::RestorePlan), [`HistoryIssue`](crate::HistoryIssue) —
//! and a test that could only be phrased in `prov`'s (a `Finding`, an autofix,
//! the recycle bin, the title index) is testing the *host*, and lives in
//! `prov/src/history/tests`.
//!
//! One file per verb, mirroring the modules beside them. The fixtures they
//! share — a seeded workspace, a capture, a torn event — are in [`support`],
//! which no sibling could own without every other one reaching across for it.

mod counting_fs;
mod host;

mod capture;
mod check;
mod forget;
mod prune;
mod read;
mod restore;
mod support;
