//! The ignore list a workspace's graph implies.
//!
//! Every tool that copies, syncs or records a folder needs to be told what in
//! it is not content — and the workspace is the only thing that knows. Its
//! graph is a bounded reachable walk (§8) from the root document, so the
//! difference between *what is on disk* and *what the graph reaches* is
//! exactly the set a person did not mean to hand to such a tool: bookkeeping
//! prov keeps for itself, archives a manifest already pins by hash, an
//! editor's dotfiles, and files nothing links.
//!
//! So this module computes that difference and renders it as an **ignore
//! list**. It walks the folder top down, subtracts the reachable set, and
//! labels every rule with [`Reason`] — the label being the part worth reading,
//! since [`Reason::Unreached`] is the one a person may want to act on from the
//! other side: link the file, and the next list withdraws the rule.
//!
//! ## What this is not
//!
//! It writes nothing and knows no tool's file format. prov once maintained a
//! marker-fenced region of a [historica](https://crates.io/crates/historica)
//! store's `history/skipped.txt`, reconciling its own rules against the
//! person's hand-written ones and against the paths that store was already
//! tracking. That coupling is gone: prov states the difference its graph
//! implies, once, and whichever tool consumes it owns the merge — which rules
//! it already holds, which of its own it keeps, and what a rule means for a
//! file it has recorded before.
//!
//! ## The rendering
//!
//! Lines are **gitignore syntax**, the one ignore-file dialect most tools
//! already read, and each is anchored with a leading `/`: the paths are
//! workspace-relative, and an unanchored gitignore pattern with no `/` in it
//! matches at *every* depth — `/.envrc` says the root's dotfile, where
//! `.envrc` would also claim one three directories down that nothing looked
//! at. Glob metacharacters in a filename are escaped for the same reason,
//! so a note actually named `draft[1].md` is one rule about one file.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use prov_graph::error::Result;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

use super::Workspace;

/// One level of the recursion, boxed — the async walk refers to itself.
type Walked<'a> = Pin<Box<dyn Future<Output = Result<(Vec<Ignore>, bool)>> + 'a>>;

/// Why a path is on the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    /// prov's own bookkeeping: a byte-parking store's interior, or a page prov
    /// derives rather than the author writing. Excluded by decision.
    Bookkeeping,
    /// An archive a manifest document claims in bulk. Its rows are pinned by
    /// hash in a document the graph does reach, so the directory is one fact.
    Claimed,
    /// A hidden entry — an editor's or a tool's, not the workspace's content.
    /// A hidden directory is ruled without being walked, so a `.git` holding
    /// ten thousand objects costs one listing, not ten thousand.
    Hidden,
    /// Nothing the workspace links reaches it. The one reason worth acting on
    /// from the other side: link the file, and the next list withdraws the
    /// rule.
    Unreached,
}

/// One rule, and why it is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ignore {
    /// The path the rule names, relative to the workspace root.
    pub path: String,
    /// Whether the rule covers a directory and everything beneath it, rather
    /// than one file.
    pub whole_dir: bool,
    /// Why the tool being told should not take what it covers.
    pub reason: Reason,
}

impl fmt::Display for Ignore {
    /// The gitignore line: anchored to the workspace root, metacharacters
    /// escaped, a trailing `/` where the rule covers a directory whole.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}", escaped(&self.path))?;
        match self.whole_dir {
            true => f.write_str("/"),
            false => Ok(()),
        }
    }
}

/// What the graph says a tool should leave alone, in path order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IgnoreList {
    /// The rules, sorted by the path each names — stable across runs, so two
    /// lists diff cleanly and a reader finds a path by scanning once.
    pub rules: Vec<Ignore>,
}

impl IgnoreList {
    /// Whether the graph reaches everything on disk — nothing to ignore.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The whole list as the text of a gitignore-syntax file, one rule per
    /// line, each line terminated.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for rule in &self.rules {
            out.push_str(&rule.to_string());
            out.push('\n');
        }
        out
    }
}

impl<FS: Storage, Id, Ix: IndexStore> Workspace<FS, Id, Ix> {
    /// The ignore list this workspace's graph implies, walking from
    /// `root_doc`.
    ///
    /// The walk mirrors the one a recording or copying tool makes — every
    /// entry, top down — because the list's job is to say, of that exact
    /// walk, what should not be taken. A subtree the graph reaches nothing in
    /// collapses to a single directory rule, so an unrelated project sitting
    /// beside the workspace costs one line rather than one per file.
    pub async fn ignore_list(&self, root_doc: &Path) -> Result<IgnoreList> {
        let scan = Scan {
            reachable: slashed(self.reachable_files(root_doc).await?),
            bookkeeping: self
                .bookkeeping(root_doc)
                .await?
                .iter()
                .filter_map(|path| slash(path))
                .collect(),
        };

        let (mut rules, _) = walk(self, &scan, String::new()).await?;
        rules.sort_by(|a, b| order(a).cmp(&order(b)));
        Ok(IgnoreList { rules })
    }

    /// The path prefixes that are prov's own bookkeeping rather than content —
    /// where the bin parks the bytes a person has already consigned, and the
    /// page prov derives rather than the author writing.
    ///
    /// The about page is *reachable* — its pointer is what keeps it from lying
    /// loose — and excluded even so, which is why this is consulted before
    /// reachability rather than after.
    async fn bookkeeping(&self, root_doc: &Path) -> Result<Vec<PathBuf>> {
        let mut prefixes = Vec::new();
        if let Some(index) = self.recycle_bin_path(root_doc).await? {
            prefixes.push(super::store_dir(&index).join("items"));
        }
        if let Some(about) = self.about_path(root_doc).await? {
            prefixes.push(about);
        }
        Ok(prefixes)
    }
}

/// The workspace facts one walk consults, gathered once.
struct Scan {
    reachable: BTreeSet<String>,
    bookkeeping: Vec<String>,
}

impl Scan {
    /// Whether a bookkeeping prefix covers `rel` (a file prefix names only
    /// itself; a directory prefix covers everything beneath).
    fn bookkeeping_covers(&self, rel: &str) -> bool {
        self.bookkeeping
            .iter()
            .any(|prefix| rel == prefix || under(rel, prefix))
    }

    /// Whether the graph reaches anything strictly beneath the directory
    /// `rel`.
    fn reaches_under(&self, rel: &str) -> bool {
        self.reachable
            .range(format!("{rel}/")..)
            .next()
            .is_some_and(|path| under(path, rel))
    }
}

/// Whether `path` lies strictly beneath the directory `prefix`.
fn under(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with('/'))
}

fn slash(path: &Path) -> Option<String> {
    path.to_str().map(str::to_owned)
}

fn slashed(paths: BTreeSet<PathBuf>) -> BTreeSet<String> {
    paths.iter().filter_map(|path| slash(path)).collect()
}

/// The list's ordering: by the path the rule names, then a file before the
/// directory of the same name, so regeneration is stable.
fn order(rule: &Ignore) -> (&str, u8) {
    (&rule.path, u8::from(rule.whole_dir))
}

/// A gitignore line's literal spelling of `path`: the characters gitignore
/// reads as pattern syntax, escaped, and a trailing space escaped too — a
/// reader strips one otherwise, and a file may honestly end in one.
fn escaped(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for character in path.chars() {
        if matches!(character, '\\' | '*' | '?' | '[' | ']') {
            out.push('\\');
        }
        out.push(character);
    }
    if out.ends_with(' ') {
        out.insert(out.len() - 1, '\\');
    }
    out
}

/// One directory level of the walk.
///
/// Returns the rules the subtree asks for and whether the graph reaches
/// anything in it — the fact the caller needs to collapse a wholly-unreached
/// subtree into a single rule.
fn walk<'a, FS: Storage, Id, Ix: IndexStore>(
    workspace: &'a Workspace<FS, Id, Ix>,
    scan: &'a Scan,
    rel_dir: String,
) -> Walked<'a> {
    Box::pin(async move {
        let mut rules = Vec::new();
        let mut any_reachable = false;

        // A directory that cannot be listed is not walked rather than fatal —
        // whatever is in it, the tool's own walk will refuse it by name, not
        // silently.
        let Ok(mut entries) = workspace.listing(Path::new(&rel_dir)).await else {
            return Ok((rules, any_reachable));
        };
        entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

        for entry in entries {
            // A name that is not UTF-8 cannot be spelled in a rule, and every
            // tool that meets it refuses it loudly by itself; nothing useful
            // to write.
            let Some(name) = entry
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            let rel = match rel_dir.is_empty() {
                true => name.clone(),
                false => format!("{rel_dir}/{name}"),
            };

            if entry.file_type().is_dir() {
                if scan.bookkeeping_covers(&rel) {
                    rules.push(dir(rel, Reason::Bookkeeping));
                    continue;
                }
                if workspace
                    .manifest_node_for(Path::new(&rel))
                    .await?
                    .is_some()
                {
                    rules.push(dir(rel, Reason::Claimed));
                    continue;
                }
                if name.starts_with('.') && !scan.reaches_under(&rel) {
                    rules.push(dir(rel, Reason::Hidden));
                    continue;
                }
                let (sub, sub_reachable) = walk(workspace, scan, rel.clone()).await?;
                any_reachable |= sub_reachable;
                // A subtree the graph reaches nothing in is one fact, not a
                // rule per file — provided everything in it is unreached, a
                // claimed or bookkeeping rule beneath keeping its own reason.
                let collapses = !sub_reachable
                    && !sub.is_empty()
                    && sub.iter().all(|rule| rule.reason == Reason::Unreached);
                match collapses {
                    true => rules.push(dir(rel, Reason::Unreached)),
                    false => rules.extend(sub),
                }
            } else if entry.file_type().is_file() {
                // Bookkeeping is checked before reachability, not after: a
                // derived page is *deliberately* reachable — the pointer is
                // what keeps it from lying loose — and excluded even so.
                let reason = if scan.bookkeeping_covers(&rel) {
                    Reason::Bookkeeping
                } else if scan.reachable.contains(&rel) {
                    any_reachable = true;
                    continue;
                } else if name.starts_with('.') {
                    Reason::Hidden
                } else {
                    Reason::Unreached
                };
                rules.push(Ignore {
                    path: rel,
                    whole_dir: false,
                    reason,
                });
            }
            // Neither a file nor a directory — a symlink, a socket. A rule
            // could silence it, but the consuming tool's own refusal names it
            // better than a silent skip would.
        }

        Ok((rules, any_reachable))
    })
}

/// A rule covering a directory whole.
fn dir(path: String, reason: Reason) -> Ignore {
    Ignore {
        path,
        whole_dir: true,
        reason,
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests;
