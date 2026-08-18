//! What a capture would leave behind, and why.
//!
//! A capture set is drawn from the **reachable** graph (§8's bounded walk), so a
//! file nothing links to is not captured. That is the right rule — a workspace
//! is what it says it is, not what happens to sit in its directories — but it
//! makes the omission *silent*, which is the wrong way for a safety net to fail.
//! A folder of four hundred notes nobody linked looks exactly like a folder of
//! four hundred notes that are safe.
//!
//! This pass is the other half of that rule: it walks the tree the capture set
//! was *not* drawn from and names what a capture would not take. It lives in
//! `prov` rather than in `prov-history` because reachability, the recycle bin
//! and the derived page are the workspace's knowledge — the store deliberately
//! knows none of them, and asks its host
//! ([`reachable_files`](prov_history::HistoryReadHost::reachable_files),
//! [`history_exclusions`](prov_history::HistoryReadHost::history_exclusions))
//! rather than growing a capability to work them out.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use crate::workspace::Workspace;
use prov_graph::error::Result;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

/// Why a file on disk is not in the capture set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Omission {
    /// Nothing the workspace links reaches it.
    ///
    /// **The one worth acting on.** The file is sitting in the workspace and a
    /// capture will not take it, so history will not bring it back. The repair
    /// is to link it — from a parent's `contents`, or as an attachment — after
    /// which it is captured like everything else.
    Unreached,
    /// It is prov's own bookkeeping: inside a byte-parking store (the history
    /// store's `events/` and `blobs/`, the recycle bin's `items/`) or the
    /// generated `about` page.
    ///
    /// Excluded on purpose and reported only so the totals add up. Capturing
    /// the store inside itself would mean no capture could ever be empty; the
    /// bin holds bytes already consigned; the page is derived from
    /// configuration this same manifest captures.
    Bookkeeping,
}

/// One file a capture would not take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uncaptured {
    /// The file, workspace-relative.
    pub path: PathBuf,
    /// Why it is not in the capture set.
    pub reason: Omission,
}

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Every file on disk that the next capture would **not** record, sorted by
    /// reason and then by path.
    ///
    /// The walk is deliberately unbounded by reachability — that is the entire
    /// point, since the reachable walk is what produced the capture set this is
    /// the complement of. What it does skip is hidden entries (`.git`,
    /// `.DS_Store`, and every editor's dotfile, which are not the workspace's
    /// content and never were) and the interiors of prov's parked stores, which
    /// are refused by *not descending* so that a store holding ten thousand
    /// blobs is never listed in order to be discarded.
    ///
    /// Files a **manifest** covers are not reported: an archive claimed in bulk
    /// is accounted for, and its rows are hashed in a document the capture set
    /// does hold. Listing ten thousand photographs as omissions would bury every
    /// finding worth reading, which is the failure this pass exists to prevent.
    pub async fn uncaptured(&self, root_doc: &Path) -> Result<Vec<Uncaptured>> {
        let captured: BTreeSet<PathBuf> = self
            .history_capture_set(root_doc)
            .await?
            .into_iter()
            .collect();
        // Descended-into or not, these are prov's own. `parked` is the subset
        // whose *interiors* are not worth walking at all; `known` is everything
        // a capture leaves out by decision rather than by oversight, which is
        // the difference this pass exists to report.
        let parked = self.parked_dirs(root_doc).await?;
        let mut known = parked.clone();
        // The whole history store, not just its parked interior. The store
        // excludes *itself* from the capture set (§2) — otherwise no capture
        // could ever be empty, and an exact restore would delete every event
        // newer than the one being restored. Its index and its tombstone list
        // are reachable and uncaptured even so, and without this they would read
        // as the very thing this pass exists to find.
        let (store_index, _) = self.history_store().store_index(root_doc).await?;
        known.push(crate::history::store_dir(&store_index));
        if let Some(about) = self.about_path(root_doc).await? {
            known.push(about);
        }

        let mut found = Vec::new();
        self.scan_uncaptured(PathBuf::new(), &captured, &parked, &known, &mut found)
            .await?;
        found.sort_by(|a, b| a.reason.cmp(&b.reason).then_with(|| a.path.cmp(&b.path)));
        Ok(found)
    }

    /// The recursive half of [`uncaptured`](Self::uncaptured).
    fn scan_uncaptured<'a>(
        &'a self,
        rel_dir: PathBuf,
        captured: &'a BTreeSet<PathBuf>,
        parked: &'a [PathBuf],
        known: &'a [PathBuf],
        out: &'a mut Vec<Uncaptured>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            let Ok(entries) = self.listing(&rel_dir).await else {
                return Ok(());
            };
            for entry in entries {
                let Some(name) = entry
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
                else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                let rel = match rel_dir.as_os_str().is_empty() {
                    true => PathBuf::from(&name),
                    false => rel_dir.join(&name),
                };
                if entry.file_type().is_dir() {
                    // Named, not descended into: the whole store is one fact,
                    // and walking it would turn that fact into ten thousand.
                    if parked.iter().any(|dir| rel.starts_with(dir)) {
                        out.push(Uncaptured {
                            path: rel,
                            reason: Omission::Bookkeeping,
                        });
                        continue;
                    }
                    // An archive claimed by a manifest is accounted for in bulk,
                    // and its rows are pinned in a document the capture set does
                    // hold. Same directory-local probe `loose_attachments` uses.
                    if self.manifest_node_for(&rel).await?.is_some() {
                        continue;
                    }
                    self.scan_uncaptured(rel, captured, parked, known, out)
                        .await?;
                } else if entry.file_type().is_file() && !captured.contains(&rel) {
                    let reason = match known.iter().any(|dir| rel.starts_with(dir)) {
                        true => Omission::Bookkeeping,
                        false => Omission::Unreached,
                    };
                    out.push(Uncaptured { path: rel, reason });
                }
            }
            Ok(())
        })
    }
}
