//! `tree` — the containment tree, rendered with box-drawing connectors.
//!
//! Also home to the few spellings a federated walk shares across commands:
//! [`descent`] and [`trust`] turn `--follow N` and `--unverified` into the
//! library's terms, and [`workspace_label`] is what a reached workspace is
//! called in any output. `check --follow` and `explore` borrow all three so a
//! boundary reads the same wherever it is crossed.

use std::path::Path;
use std::process::ExitCode;

use prov::{Node, NodeKind, block_on};

use crate::CmdResult;
use crate::peer;
use crate::session::{Session, ws_rel};

pub(crate) fn cmd_tree(root: Option<&Path>, follow: Option<usize>, unverified: bool) -> CmdResult {
    let session = Session::open()?;
    let root = match root {
        Some(r) => ws_rel(&session.ctx, r)?,
        None => session.ctx.root_doc.clone(),
    };
    // Without `--follow` this is the single-workspace walk it always was, byte
    // for byte: `descend` under a resolver would give the same shape, but the
    // unfollowed path should not pay for a peer map it never consults.
    let Some(depth) = follow else {
        let node = block_on(session.ws.tree(&root))?;
        print_node(&node, "", true, true);
        return Ok(ExitCode::SUCCESS);
    };
    let peers = peer::PeerMap::load();
    let federation = block_on(prov::descend(
        &session.ws,
        &root,
        &peers,
        &descent(depth, unverified),
    ))?;
    print_crossed_node(&federation, &federation.tree, "", true, true);
    Ok(ExitCode::SUCCESS)
}

/// How far a `--follow` goes, and on whose say-so.
pub(crate) fn descent(depth: usize, unverified: bool) -> prov::Descent {
    prov::Descent {
        trust: trust(unverified),
        depth,
    }
}

/// `--unverified`, as the library spells it. An absent name is what the flag
/// buys past; a *different* name is refused under both, by the library.
pub(crate) fn trust(unverified: bool) -> prov::Trust {
    if unverified {
        prov::Trust::Unverified
    } else {
        prov::Trust::Confirmed
    }
}

/// Render one tree node: `path — title (marker)`, then its children with
/// box-drawing connectors.
fn print_node(node: &Node, prefix: &str, is_last: bool, is_root: bool) {
    print_tree_line(
        prefix,
        is_last,
        is_root,
        &node_name(&node.path, node.title.as_deref(), node.label.as_deref()),
        &node_marker(&node.kind),
    );
    let child_prefix = child_prefix(prefix, is_last, is_root);
    for (i, child) in node.children.iter().enumerate() {
        print_node(child, &child_prefix, i + 1 == node.children.len(), false);
    }
}

/// [`print_node`]'s sibling for a tree that crossed a boundary. The connectors,
/// the name and every marker a node can carry within one workspace are the same
/// ones — what a crossing adds is a marker at the boundary itself, and nowhere
/// else, so a federated tree with no crossings in it renders identically.
fn print_crossed_node(
    federation: &prov::Federation,
    node: &prov::crossing::Node,
    prefix: &str,
    is_last: bool,
    is_root: bool,
) {
    print_tree_line(
        prefix,
        is_last,
        is_root,
        &node_name(&node.path, node.title.as_deref(), node.label.as_deref()),
        &boundary_marker(federation, node),
    );
    let child_prefix = child_prefix(prefix, is_last, is_root);
    for (i, child) in node.children.iter().enumerate() {
        print_crossed_node(
            federation,
            child,
            &child_prefix,
            i + 1 == node.children.len(),
            false,
        );
    }
}

fn print_tree_line(prefix: &str, is_last: bool, is_root: bool, name: &str, marker: &str) {
    let connector = if is_root {
        String::new()
    } else {
        format!("{prefix}{}", if is_last { "└── " } else { "├── " })
    };
    println!("{connector}{name}{marker}");
}

fn child_prefix(prefix: &str, is_last: bool, is_root: bool) -> String {
    if is_root {
        String::new()
    } else {
        format!("{prefix}{}", if is_last { "    " } else { "│   " })
    }
}

/// `path — title`, falling back to the link's label and then to the path alone.
fn node_name(path: &Path, title: Option<&str>, label: Option<&str>) -> String {
    title
        .or(label)
        .map(|t| format!("{} — {t}", path.display()))
        .unwrap_or_else(|| path.display().to_string())
}

/// What a node's resolution says about it, in its own workspace's terms.
fn node_marker(kind: &NodeKind) -> String {
    match kind {
        NodeKind::Doc => String::new(),
        NodeKind::Missing => " (missing)".to_string(),
        NodeKind::Cycle => " (cycle!)".to_string(),
        NodeKind::Unreadable(e) => format!(" (unreadable: {e})"),
        NodeKind::UnresolvedId(id) => format!(" (unresolved id: {id})"),
        NodeKind::AmbiguousAlias(name) => format!(" (ambiguous alias: [[{name}]])"),
        NodeKind::Foreign { workspace, id } => {
            format!(" (workspace {workspace}, id {id} — not followed)")
        }
    }
}

/// The marker for a node of a federated tree.
///
/// A followed boundary says which workspace the subtree below it is in and
/// where that workspace is on this device — without which the paths underneath
/// are relative to nothing the reader can see. A refused one keeps the marker an
/// unfollowed tree prints and adds why, which is the whole difference between
/// `--follow` and not.
fn boundary_marker(federation: &prov::Federation, node: &prov::crossing::Node) -> String {
    match &node.boundary {
        None => node_marker(&node.kind),
        Some(prov::Boundary::Followed { into }) => {
            let reached = &federation.workspaces[*into];
            format!(
                "  ⇒ workspace {} ({})",
                workspace_label(reached),
                reached.root_dir.display()
            )
        }
        Some(prov::Boundary::Refused(refusal)) => match &node.kind {
            NodeKind::Foreign { workspace, id } => {
                format!(" (workspace {workspace}, id {id} — not followed: {refusal})")
            }
            // Only a foreign leaf is ever crossed at, so this is unreachable
            // through `descend` — naming it beats a panic on a shape a future
            // boundary might take.
            other => format!("{} (not followed: {refusal})", node_marker(other)),
        },
    }
}

/// What to call a workspace in output: the name the reference asked for, else
/// what it calls itself, else that it calls itself nothing. An anonymous
/// workspace is legal — prov mints a name only on request — so the reader is
/// told the directory too, everywhere this appears.
pub(crate) fn workspace_label(reached: &prov::Reached) -> &str {
    if !reached.name.is_empty() {
        &reached.name
    } else if !reached.declares.is_empty() {
        &reached.declares
    } else {
        "<anonymous>"
    }
}
