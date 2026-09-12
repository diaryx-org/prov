//! `explore` — walk the graph interactively, one document per screen.
//!
//! A thin loop over the library's resolution: at each document the screen
//! offers the raw text, `$EDITOR`, every forward link by relation, every
//! backlink not already implied by one of them, and Back. A cross-workspace
//! reference is a step like any other when this device's peer map knows where
//! the workspace is — the screen moves into the peer and subsequent links
//! resolve in *its* terms — so the explorer holds one [`ExploreWs`] per
//! workspace it has entered and Back finds the previous one as it was left.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use prov::{
    Document, FileIndex, Id, IdIndex, Minter, PeerResolver, StdFs, Target, Value, Workspace,
    block_on, link,
};

use crate::peer::{self, describe_peer};
use crate::session::{Ctx, find_root, find_root_quiet_at, load, workspace, ws_rel};
use crate::term::{edit_file, page_text};
use crate::tree::trust;
use crate::{AnyError, CmdResult};

/// One choice on an explore screen — what selecting the menu item does.
enum ExploreAction {
    /// Page the current document's raw text.
    View,
    /// Open the current document in `$EDITOR`.
    Edit,
    /// Navigate to another document of the workspace being explored (a resolved
    /// forward link or a backlink).
    Goto(PathBuf),
    /// Cross into another workspace: open the peer this device's map records,
    /// ask *its* registry where the id lives, and carry on exploring there. The
    /// one navigation that changes which workspace the screen is in.
    Cross {
        /// The workspace the reference names.
        workspace: String,
        /// The id it names there — resolved by the peer's own registry, which is
        /// the one thing the reading workspace can never answer.
        id: Id,
    },
    /// A link that resolves to nothing followable (external, unresolved id,
    /// ambiguous alias) — selecting it just prints why.
    Note(String),
    /// Return to the previously-visited document, in whichever workspace it was.
    Back,
    Quit,
}

/// One workspace the explorer has open, and everything a screen in it needs.
///
/// A struct because there can be several: crossing a boundary does not replace
/// the workspace being explored, it adds one, and Back has to find the previous
/// one exactly as it left it. Each is opened once, keyed on its root directory,
/// and kept for the session — the title index and the backlink map are the
/// expensive halves, and re-crossing a boundary should not pay for them twice.
struct ExploreWs {
    ctx: Ctx,
    ws: Workspace<StdFs, Minter, FileIndex>,
    /// The document title lookup and the backlink map, both scoped to what this
    /// workspace reaches from its own root document.
    titles: prov::TitleIndex,
    backlinks: std::collections::BTreeMap<PathBuf, Vec<prov::Backlink>>,
    /// What to call this workspace on a screen inside it.
    label: String,
}

impl ExploreWs {
    /// Open a workspace for exploring: the ordinary CLI workspace, plus the two
    /// reachability-scoped indexes a screen reads. `name` is what the reference
    /// that led here called it — for the origin, what it calls itself.
    fn open(ctx: Ctx, name: &str) -> Result<Self, AnyError> {
        let ws = workspace(&ctx)?;
        let root = ctx.root_doc.clone();
        // Both bounded/lazy, so cheap even at the root of a large repo — and
        // computed once per workspace rather than once per screen.
        let titles = block_on(ws.title_index_scoped(&root))?;
        let backlinks = block_on(ws.backlinks(&root))?;
        let label = if name.is_empty() {
            "<anonymous>".to_string()
        } else {
            name.to_string()
        };
        Ok(Self {
            ctx,
            ws,
            titles,
            backlinks,
            label,
        })
    }
}

/// Interactively walk the workspace graph: at each document, view or edit it, or
/// follow any forward link (in any relation) or backlink to move on. A thin loop
/// over the library's resolution — the same path/id/alias resolution `tree` and
/// `check` use, with the reachability-scoped title index and the backlink map
/// each computed once up front.
///
/// A cross-workspace reference is a step too, where this device's map says where
/// the workspace is and the peer confirms its own name: the screen moves into
/// the peer, subsequent links resolve in *its* terms, and Back crosses home.
/// Read-only in the peer, as every crossing is — `Edit` opens `$EDITOR` on the
/// file, which is the user editing their own other workspace, not prov writing
/// across a boundary.
pub(crate) fn cmd_explore(file: Option<&Path>, unverified: bool) -> CmdResult {
    let ctx = find_root()?;
    let mut current = match file {
        Some(f) => ws_rel(&ctx, f)?,
        None => ctx.root_doc.clone(),
    };
    // Keyed on the root directory, which is what a crossing lands on and the one
    // handle both sides of a boundary agree about.
    let origin = ctx.root_dir.clone();
    let name = ctx.config.workspace_id.clone();
    let mut open: std::collections::BTreeMap<PathBuf, ExploreWs> =
        std::collections::BTreeMap::new();
    open.insert(origin.clone(), ExploreWs::open(ctx, &name)?);
    let mut here = origin.clone();

    // Where the walk has been, workspace and all: a path alone would send Back
    // to the same-named file in the wrong archive.
    let mut history: Vec<(PathBuf, PathBuf)> = Vec::new();
    loop {
        let full = open[&here].ctx.root_dir.join(&current);
        let (text, doc) = match load(&full) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("prov: cannot open {}: {e}", current.display());
                match history.pop() {
                    Some((prev_ws, prev)) => {
                        here = prev_ws;
                        current = prev;
                        continue;
                    }
                    None => return Ok(ExitCode::FAILURE),
                }
            }
        };
        let (header, actions) = explore_screen(
            &open[&here],
            here != origin,
            &current,
            &doc,
            !history.is_empty(),
        );

        let mut menu = cliclack::select(header);
        for (i, (label, hint, _)) in actions.iter().enumerate() {
            menu = menu.item(i, label, hint);
        }
        // Any error (including a Ctrl-C / Esc cancel) leaves the explorer.
        let Ok(choice) = menu.interact() else { break };

        match &actions[choice].2 {
            ExploreAction::View => page_text(&text)?,
            ExploreAction::Edit => edit_file(&full)?,
            ExploreAction::Goto(p) => {
                history.push((here.clone(), current.clone()));
                current = p.clone();
            }
            ExploreAction::Cross { workspace, id } => {
                let (workspace, id) = (workspace.clone(), id.clone());
                // The map is read again per crossing for the same reason a
                // screen reads it per screen: one edited between screens should
                // take effect on the next one.
                let peers = peer::PeerMap::load();
                let crossing = match block_on(prov::open_peer(
                    &StdFs,
                    &peers,
                    &workspace,
                    trust(unverified),
                )) {
                    Ok(crossing) => crossing,
                    Err(e) => {
                        eprintln!("prov: cannot read `{workspace}`: {e}");
                        continue;
                    }
                };
                let peer = match crossing {
                    prov::Crossing::Refused(refusal) => {
                        eprintln!("prov: `{workspace}` — {refusal}");
                        continue;
                    }
                    prov::Crossing::Opened(peer) => peer,
                };
                // `open_peer` is the confirmation — the peer map's claim checked
                // against what the workspace there calls itself — and the root
                // directory it vouched for is all that is carried over.
                // Exploring wants the workspace this CLI builds anywhere else
                // (its config, its identity policy, its registry), so the peer is
                // opened again through the ordinary route.
                let key = peer.discovered.root_dir.clone();
                if !open.contains_key(&key) {
                    let opened = find_root_quiet_at(&key)
                        .and_then(|peer_ctx| ExploreWs::open(peer_ctx, &workspace));
                    match opened {
                        Ok(state) => {
                            open.insert(key.clone(), state);
                        }
                        Err(e) => {
                            eprintln!("prov: cannot explore `{workspace}`: {e}");
                            continue;
                        }
                    }
                }
                // The peer's own registry answers where the id lives. A miss is
                // not a broken link: registration is a publish-time contract, and
                // the document may simply not be published yet.
                match open[&key].ws.index().resolve(&id) {
                    Some(path) => {
                        history.push((here.clone(), current.clone()));
                        here = key;
                        current = path;
                    }
                    None => eprintln!("prov: `{workspace}` has no document registered as `{id}`"),
                }
            }
            ExploreAction::Note(message) => eprintln!("prov: {message}"),
            ExploreAction::Back => {
                if let Some((prev_ws, prev)) = history.pop() {
                    here = prev_ws;
                    current = prev;
                }
            }
            ExploreAction::Quit => break,
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// One explore screen: its header, and every choice on it — view/edit, each
/// forward link by relation, each backlink that is not already one of them, then
/// navigation.
fn explore_screen(
    state: &ExploreWs,
    away: bool,
    current: &Path,
    doc: &Document,
    has_history: bool,
) -> (String, Vec<(String, String, ExploreAction)>) {
    let mut actions: Vec<(String, String, ExploreAction)> = Vec::new();
    actions.push((
        "View this document".into(),
        "page the raw file".into(),
        ExploreAction::View,
    ));
    actions.push(("Edit in $EDITOR".into(), String::new(), ExploreAction::Edit));

    // Documents already reachable from this screen by a forward link. A
    // backlink whose source is in this set is the inverse of a link we
    // already show — the child's `part_of` mirroring our `contents`, most
    // often — and navigates to the same place, so it is suppressed below to
    // keep a folder-note's menu from listing every child twice.
    let mut forward_targets: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    // Once per screen, not once per link: every foreign reference on this
    // document is answered against the same map, and a map edited between
    // screens is picked up on the next one.
    let peers = peer::PeerMap::load();
    for relation in state.ws.relations().relations() {
        let Some(value) = doc.meta.get(&relation.name) else {
            continue;
        };
        for raw in value.link_strings() {
            let parsed = link::Link::parse(&raw);
            let (label, hint, action) =
                match state
                    .ws
                    .resolve_link_with(current, &parsed, Some(&state.titles))
                {
                    Target::Path(p) => {
                        let t = doc_title(&state.ctx, &p);
                        forward_targets.insert(p.clone());
                        (
                            format!("{}: {t}  ({})", relation.name, p.display()),
                            String::new(),
                            ExploreAction::Goto(p),
                        )
                    }
                    Target::External => (
                        format!("{}: {} (external)", relation.name, parsed.target),
                        String::new(),
                        ExploreAction::Note("external link — not followed".into()),
                    ),
                    Target::SameDocument => (
                        format!("{}: {} (this document)", relation.name, parsed.target),
                        String::new(),
                        ExploreAction::Note(
                            "a place inside this document — prov does not read one".into(),
                        ),
                    ),
                    Target::UnresolvedId(id) => (
                        format!("{}: {id} (unresolved id)", relation.name),
                        String::new(),
                        ExploreAction::Note("this id has no live registry entry".into()),
                    ),
                    Target::AmbiguousAlias(name) => (
                        format!("{}: {name} (ambiguous alias)", relation.name),
                        String::new(),
                        ExploreAction::Note("several documents share this title".into()),
                    ),
                    // A step like any other, when the map knows where the
                    // workspace is and the peer confirms its name — and the hint
                    // says which of those is missing when it is not.
                    Target::Foreign { workspace, id } => (
                        format!("{}: {id} → workspace {workspace}", relation.name),
                        describe_peer(&peers.locate(&workspace), &workspace),
                        ExploreAction::Cross { workspace, id },
                    ),
                };
            actions.push((label, hint, action));
        }
    }

    if let Some(inbound) = state.backlinks.get(current) {
        for backlink in inbound {
            // Skip the inverse of a forward link already on this screen — the
            // same document, reached the same way (a child's `part_of` echoing
            // our `contents`). Genuinely-new backlinks (a `related` from a
            // document we don't link to) are unaffected.
            if forward_targets.contains(&backlink.source) {
                continue;
            }
            let by = if backlink.by_id { "id" } else { "path" };
            actions.push((
                format!("← {} [{}]", backlink.source.display(), backlink.site),
                format!("linked from, by {by}"),
                ExploreAction::Goto(backlink.source.clone()),
            ));
        }
    }

    if has_history {
        actions.push((
            "Back".into(),
            "the previous document".into(),
            ExploreAction::Back,
        ));
    }
    actions.push(("Quit".into(), String::new(), ExploreAction::Quit));

    let title = doc
        .meta
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // Which workspace this is, but only once the walk has left home: naming it
    // on every screen of an ordinary session would be noise about a boundary
    // nobody crossed.
    let where_ = if away {
        format!("{} ▸ ", state.label)
    } else {
        String::new()
    };
    let header = if title.is_empty() {
        format!("{where_}{}", current.display())
    } else {
        format!("{where_}{} — {title}", current.display())
    };
    (header, actions)
}

/// The title a linked document declares (its `title` frontmatter), else a title
/// derived from its filename — the label an explore menu shows for a link.
fn doc_title(ctx: &Ctx, rel: &Path) -> String {
    load(&ctx.root_dir.join(rel))
        .ok()
        .and_then(|(_, d)| {
            d.meta
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| link::path_to_title(rel))
}
