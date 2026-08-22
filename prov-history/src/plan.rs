//! Computing the skiplist: the walk, the subtraction, and the diff.
//!
//! The walk here mirrors the one historica's own recording makes — every
//! entry beside the store, top down — because the skiplist's job is to say, of
//! that exact walk, what recording should not take. What it consults on the
//! way is the workspace's knowledge, through [`SkipHost`]: the reachable set,
//! the bookkeeping prefixes, and which directories a manifest claims in bulk.
//!
//! The result is a target state, not an instruction stream: [`Skiplist::rules`]
//! is what the generated region of `skipped.txt` should say, whole, and
//! [`Skiplist::fresh`]/[`Skiplist::stale`] are the diff against what it says
//! now — computed so a plan can be shown before it is applied, and so an
//! application that changes nothing can say so.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use historica::store::{HEADER_FILE, STORE_DIR};
use historica::working::Rule;
use prov_graph::error::Result;

use crate::{SkipHost, Standing};

/// One level of the recursion, boxed — the async walk refers to itself.
type Walked<'a> = Pin<Box<dyn Future<Output = Result<(Vec<Skip>, bool, bool)>> + 'a>>;

/// Why a rule is in the skiplist.
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
    /// from the other side: link the file, and the next plan withdraws the
    /// rule.
    Unreached,
}

/// One rule the skiplist asks for, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skip {
    /// The rule, in historica's own vocabulary.
    pub rule: Rule,
    /// Why recording should not take what it covers.
    pub reason: Reason,
}

/// What the generated region should say, and how that differs from what
/// stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skiplist {
    /// The target region, whole, in path order. [`apply`](crate::apply)
    /// writes exactly this between the markers.
    pub rules: Vec<Skip>,
    /// The rules in [`rules`](Self::rules) the standing region does not hold
    /// yet.
    pub fresh: Vec<Skip>,
    /// The standing region's rules that no longer belong — a file the graph
    /// now reaches, or one no longer on disk. Withdrawn by the next apply.
    pub stale: Vec<Rule>,
    /// Rules the graph asks for that the store's tree forbids: each covers a
    /// path historica is tracking, and a skip rule covering a tracked path
    /// stops recording cold. Withheld instead, with the tracked path that
    /// forbids each — the person decides whether the file should be dropped
    /// from history or linked back into the graph.
    pub withheld: Vec<(Rule, String)>,
    /// A hand-written rule covering a file the graph reaches, with the file.
    /// The region is this crate's to rewrite; the rest of `skipped.txt` is
    /// not, so these are reported rather than repaired.
    pub shadowed: Vec<(Rule, String)>,
}

impl Skiplist {
    /// Whether the standing region already says what this plan computed —
    /// nothing to add, nothing to withdraw.
    pub fn settled(&self) -> bool {
        self.fresh.is_empty() && self.stale.is_empty()
    }
}

/// Compute the skiplist for the workspace `host` describes, against what the
/// store already says.
///
/// Reads nothing from the store itself — [`Standing`] carries what was read —
/// and writes nothing anywhere; [`apply`](crate::apply) is the writing half.
pub async fn skiplist<H: SkipHost>(
    host: &H,
    root_doc: &Path,
    standing: &Standing,
) -> Result<Skiplist> {
    let scan = Scan {
        reachable: slashed(host.reachable_files(root_doc).await?),
        bookkeeping: host
            .bookkeeping(root_doc)
            .await?
            .iter()
            .filter_map(|p| slash(p))
            .collect(),
        tracked: &standing.tracked,
        hand: &standing.hand,
    };

    let mut shadowed = Vec::new();
    let mut withheld = Vec::new();
    let (mut rules, _, _) = walk(host, &scan, String::new(), &mut shadowed, &mut withheld).await?;
    rules.sort_by(|a, b| order(&a.rule).cmp(&order(&b.rule)));

    let fresh = rules
        .iter()
        .filter(|skip| !standing.region.contains(&skip.rule))
        .cloned()
        .collect();
    let stale = standing
        .region
        .iter()
        .filter(|rule| !rules.iter().any(|skip| &skip.rule == *rule))
        .cloned()
        .collect();

    Ok(Skiplist {
        rules,
        fresh,
        stale,
        withheld,
        shadowed,
    })
}

/// The workspace facts one walk consults, gathered once.
struct Scan<'a> {
    reachable: BTreeSet<String>,
    bookkeeping: Vec<String>,
    tracked: &'a BTreeSet<String>,
    hand: &'a [Rule],
}

impl Scan<'_> {
    /// Whether a bookkeeping prefix covers `rel` (a file prefix names only
    /// itself; a directory prefix covers everything beneath).
    fn bookkeeping_covers(&self, rel: &str) -> bool {
        self.bookkeeping
            .iter()
            .any(|prefix| rel == prefix || under(rel, prefix))
    }

    /// The first hand rule covering `rel`, if any.
    fn hand_covering(&self, rel: &str) -> Option<&Rule> {
        self.hand.iter().find(|rule| rule.covers(rel))
    }

    /// Whether a hand rule already skips the directory `rel` whole.
    fn hand_skips_directory(&self, rel: &str) -> bool {
        self.hand.iter().any(|rule| match rule {
            Rule::Under(prefix) => rel == prefix || under(rel, prefix),
            Rule::Path(_) | Rule::Suffix(_) => false,
        })
    }

    /// Whether the graph reaches anything strictly beneath the directory
    /// `rel`.
    fn reaches_under(&self, rel: &str) -> bool {
        member_under(&self.reachable, rel)
    }

    /// A tracked path at or beneath `rel`, if the store's tree holds one.
    fn tracked_at_or_under(&self, rel: &str) -> Option<String> {
        if self.tracked.contains(rel) {
            return Some(rel.to_owned());
        }
        let mut range = self.tracked.range(format!("{rel}/")..);
        range
            .next()
            .filter(|path| under(path, rel))
            .map(|path| path.to_owned())
    }
}

/// Whether `path` lies strictly beneath the directory `prefix`.
fn under(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Whether `set` holds anything strictly beneath the directory `prefix`.
fn member_under(set: &BTreeSet<String>, prefix: &str) -> bool {
    set.range(format!("{prefix}/")..)
        .next()
        .is_some_and(|path| under(path, prefix))
}

fn slash(path: &Path) -> Option<String> {
    path.to_str().map(str::to_owned)
}

fn slashed(paths: BTreeSet<PathBuf>) -> BTreeSet<String> {
    paths.iter().filter_map(|p| slash(p)).collect()
}

/// The region's ordering: by the path or suffix the rule names, then by kind,
/// so regeneration is stable and a reader finds a path by scanning once.
fn order(rule: &Rule) -> (&str, u8) {
    match rule {
        Rule::Path(path) => (path, 0),
        Rule::Under(path) => (path, 1),
        Rule::Suffix(suffix) => (suffix, 2),
    }
}

/// One directory level of the walk.
///
/// Returns the skips the subtree asks for, whether the graph reaches anything
/// in it, and whether it holds any file at all — the two facts the caller
/// needs to collapse a wholly-unreached subtree into a single rule.
fn walk<'a, H: SkipHost>(
    host: &'a H,
    scan: &'a Scan<'a>,
    rel_dir: String,
    shadowed: &'a mut Vec<(Rule, String)>,
    withheld: &'a mut Vec<(Rule, String)>,
) -> Walked<'a> {
    Box::pin(async move {
        let mut skips = Vec::new();
        let mut any_reachable = false;
        let mut any_file = false;

        // A directory that cannot be listed is not walked rather than fatal —
        // the same tolerance the reporting pass this replaces had. Whatever is
        // in it, recording's own walk will refuse it by name, not silently.
        let Ok(mut entries) = host.listing(Path::new(&rel_dir)).await else {
            return Ok((skips, any_reachable, any_file));
        };
        entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

        for entry in entries {
            // A name that is not UTF-8 cannot be spelled in a rule, and
            // recording refuses it loudly by itself; nothing useful to write.
            let Some(name) = entry
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            let rel = match rel_dir.is_empty() {
                true => name.clone(),
                false => format!("{rel_dir}/{name}"),
            };

            if entry.file_type().is_dir() {
                // The store itself: historica hard-skips it from its own
                // recording, so there is nothing for a rule to say.
                if rel_dir.is_empty() && name == STORE_DIR && is_store(host, &rel).await {
                    continue;
                }
                // A hand rule already skips it whole; saying it again in the
                // region would make the person's line look redundant.
                if scan.hand_skips_directory(&rel) {
                    continue;
                }
                if scan.bookkeeping_covers(&rel) {
                    push_dir(&mut skips, withheld, scan, rel, Reason::Bookkeeping);
                    continue;
                }
                if host.claimed(Path::new(&rel)).await? {
                    push_dir(&mut skips, withheld, scan, rel, Reason::Claimed);
                    continue;
                }
                if name.starts_with('.') && !scan.reaches_under(&rel) {
                    push_dir(&mut skips, withheld, scan, rel, Reason::Hidden);
                    continue;
                }
                let (sub, sub_reachable, sub_file) =
                    walk(host, scan, rel.clone(), shadowed, withheld).await?;
                any_reachable |= sub_reachable;
                any_file |= sub_file;
                // A subtree the graph reaches nothing in is one fact, not a
                // rule per file — provided everything in it is unreached (a
                // claimed or bookkeeping rule beneath keeps its own reason)
                // and nothing tracked hides there.
                let collapses = !sub_reachable
                    && !sub.is_empty()
                    && sub.iter().all(|skip| skip.reason == Reason::Unreached)
                    && scan.tracked_at_or_under(&rel).is_none();
                match collapses {
                    true => skips.push(Skip {
                        rule: Rule::Under(rel),
                        reason: Reason::Unreached,
                    }),
                    false => skips.extend(sub),
                }
            } else if entry.file_type().is_file() {
                any_file = true;
                // Bookkeeping is checked before reachability, not after: a
                // derived page is *deliberately* reachable — the pointer is
                // what keeps it from lying loose — and excluded even so, the
                // way the old capture set subtracted its exclusions from the
                // reachable walk.
                let reason = if scan.bookkeeping_covers(&rel) {
                    Reason::Bookkeeping
                } else if scan.reachable.contains(&rel) {
                    any_reachable = true;
                    if let Some(rule) = scan.hand_covering(&rel) {
                        shadowed.push((rule.clone(), rel));
                    }
                    continue;
                } else if name.starts_with('.') {
                    Reason::Hidden
                } else {
                    Reason::Unreached
                };
                // The person already said skip; the region has nothing to add.
                if scan.hand_covering(&rel).is_some() {
                    continue;
                }
                if scan.tracked.contains(&rel) {
                    withheld.push((Rule::Path(rel.clone()), rel));
                    continue;
                }
                skips.push(Skip {
                    rule: Rule::Path(rel),
                    reason,
                });
            }
            // Neither a file nor a directory — a symlink, a socket. A rule
            // could silence it, but recording's own refusal names it better
            // than a silent skip would.
        }

        Ok((skips, any_reachable, any_file))
    })
}

/// A directory rule, or the report of why it cannot be written.
fn push_dir(
    skips: &mut Vec<Skip>,
    withheld: &mut Vec<(Rule, String)>,
    scan: &Scan<'_>,
    rel: String,
    reason: Reason,
) {
    match scan.tracked_at_or_under(&rel) {
        Some(tracked) => withheld.push((Rule::Under(rel), tracked)),
        None => skips.push(Skip {
            rule: Rule::Under(rel),
            reason,
        }),
    }
}

/// Whether `rel` holds a historica store — the marker file, not the name,
/// since a folder merely called `history` is content like any other.
async fn is_store<H: SkipHost>(host: &H, rel: &str) -> bool {
    match host.listing(Path::new(rel)).await {
        Ok(entries) => entries.iter().any(|entry| {
            entry.file_type().is_file()
                && entry.file_name().and_then(|n| n.to_str()) == Some(HEADER_FILE)
        }),
        Err(_) => false,
    }
}
