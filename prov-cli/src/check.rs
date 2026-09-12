//! `check` and `manifest` — what is wrong with the workspace, and the repairs
//! on offer.
//!
//! `check` walks the whole graph and reports; `--fix` walks the findings and,
//! for each, offers everything the library's `remedies` can do about it;
//! `--follow` runs the same check again in every workspace this one reaches
//! across a confirmed boundary, reported grouped because a finding's subject
//! is a path in one workspace's terms. `manifest` is the per-directory
//! counterpart: what a manifest lists against what is there.
//!
//! Narration goes to stderr and findings to stdout, one per line or as a JSON
//! array — the convention that lets `prov check` stand as a CI gate.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use prov::{FileIndex, Minter, NodeKind, StdFs, Workspace, block_on};

use crate::CmdResult;
use crate::about::about_context;
use crate::cli::{CheckArgs, FixModeArg};
use crate::json;
use crate::peer;
use crate::session::{
    Ctx, ensure_registry, find_root, find_root_quiet, find_root_quiet_at, persist, resolve_target,
    workspace, ws_rel,
};
use crate::term::prompt;
use crate::tree::{descent, workspace_label};

/// `check` — walk the workspace and report what is wrong with it.
///
/// `only` filters the *results*; it never narrows the walk. The findings that
/// matter most about a single document are the relational ones — nothing links
/// to it, its parent dropped it, an inbound label went stale — and every one of
/// those is discovered from somewhere else in the graph. Narrowing the walk to
/// reach them faster would be narrowing it past the evidence, and the command
/// would report a file clean because it could not see the three things wrong
/// with it. So `--only` costs a full check, and says so.
///
/// Narration to stderr; stdout carries the machine value — one finding per
/// line, or the whole set as a JSON array under `--json`.
pub(crate) fn cmd_check(args: CheckArgs) -> CmdResult {
    let CheckArgs {
        root,
        fix,
        only,
        json: as_json,
        follow,
        unverified,
    } = args;
    // A target may be an `id:` or `@`-route, resolved to a path before the
    // workspace is opened for checking.
    let root = root.map(|r| resolve_target(&r)).transpose()?;
    let only = only.map(|o| resolve_target(&o)).transpose()?;
    let (root, only) = (root.as_deref(), only.as_deref());
    // `check` reports config issues in full (Finding::ConfigIssue), so skip the
    // one-line find_root warning that would just duplicate them.
    let mut ctx = find_root_quiet()?;
    // Heal first, validate second: if a mutation was interrupted by a crash, a
    // write-ahead journal is on disk. Roll it forward before reading the
    // workspace, so `check` reports on a consistent tree — and so the recovery
    // that `Error::Torn` points here to perform actually happens.
    match block_on(prov::recover(&StdFs, &ctx.root_dir))? {
        prov::Recovered::Applied(n) => {
            eprintln!("recovered an interrupted change: rolled {n} op(s) forward from the journal");
        }
        prov::Recovered::Nothing => {}
    }
    let root = match root {
        Some(r) => ws_rel(&ctx, r)?,
        None => ctx.root_doc.clone(),
    };
    // A subject that is not there would filter every finding away and report
    // the file clean — the one failure mode a per-file check must not have, and
    // an unreadable one at that, since "no findings" is exactly what a correct
    // run of this command usually prints. A typo is caught here instead.
    let only = only.map(|o| ws_rel(&ctx, o)).transpose()?;
    if let Some(subject) = &only
        && !ctx.root_dir.join(subject).exists()
    {
        return Err(format!(
            "{}: no such document — `--only` filters findings by subject, so a \
path that is not in the workspace would report clean",
            subject.display()
        )
        .into());
    }
    let mut ws = workspace(&ctx)?;
    let mut findings = block_on(ws.check(&root))?;
    // The generated page is checked alongside the graph, so "run `check` before
    // handing this workspace to someone" guarantees one more thing: that the
    // page describing it is not lying. Only when checking the workspace root —
    // a scoped `check <subtree>` is asking about that subtree.
    if root == ctx.root_doc {
        let about_ctx = about_context(&ctx)?;
        if let Some(finding) = block_on(ws.check_about(&ctx.root_doc, &ctx.config, &about_ctx))? {
            findings.push(finding);
        }
    }
    if let Some(subject) = &only {
        findings.retain(|f| f.subject() == subject);
    }
    let findings = findings;
    if let Some(mode) = fix {
        return cmd_check_fix(&mut ctx, &mut ws, &root, &findings, mode, only.as_deref());
    }
    // Clap has already refused `--follow` beside `--fix` and `--only`, so what
    // crosses the boundary is exactly the command above: the origin's own check,
    // run again in each workspace this one reaches.
    if let Some(depth) = follow {
        return check_across(&ctx, &ws, &root, findings, as_json, depth, unverified);
    }
    if as_json {
        print!(
            "{}",
            json::J::Arr(findings.iter().map(json::finding).collect()).render()
        );
    } else {
        for finding in &findings {
            println!("{finding}");
        }
    }
    // The count line is narration for a person reading a terminal, and `--json`
    // is the mode where nobody is: the array says how many it holds, and says it
    // to the program that asked. Errors still go to stderr — this silences the
    // summary, not the diagnostics.
    if !as_json {
        let scope = match &only {
            Some(subject) => format!(" for {}", subject.display()),
            None => String::new(),
        };
        if findings.is_empty() {
            eprintln!("ok: no findings{scope}");
        } else {
            eprintln!("{} finding(s){scope}", findings.len());
        }
    }
    // Unchanged by `--json`: findings mean a non-zero exit, which is what lets
    // `prov check` stand as a CI gate. Worth knowing in a shell that treats a
    // non-zero exit as a failed pipeline (nushell does) — there, capture the
    // status rather than letting it abort the pipe:
    //
    //     (prov check --json | complete).stdout | from json
    if findings.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// `check --follow` — the same check the origin just ran, run again in every
/// workspace the origin reaches across a confirmed boundary, reported grouped.
///
/// Grouped and not merged, because a finding's subject is a path in one
/// workspace's terms and means nothing once it has crossed a root. So every
/// line carries the workspace it belongs to, and the JSON shape is an array of
/// workspaces rather than an array of findings.
///
/// What this is *not* is verification of foreign references. That stays refused
/// for the reason it always has been: a finding raised about a workspace this
/// device cannot see is a false positive on every device that lacks it. Each
/// workspace here is checked exactly as `prov check` checks it standing inside
/// it — no reference crosses, only the reader.
///
/// Read-only in every peer, including the recovery `check` performs at home: a
/// journal left by an interrupted write in *another* workspace is that
/// workspace's to roll forward, and rolling it forward from here would be
/// writing across a boundary.
fn check_across(
    ctx: &Ctx,
    ws: &Workspace<StdFs, Minter, FileIndex>,
    root: &Path,
    origin: Vec<prov::Finding>,
    as_json: bool,
    depth: usize,
    unverified: bool,
) -> CmdResult {
    let peers = peer::PeerMap::load();
    let federation = block_on(prov::descend(ws, root, &peers, &descent(depth, unverified)))?;

    // One report per workspace reached, the origin first — whose findings are
    // already in hand, since they are the ones this command has always printed.
    let mut reports: Vec<WorkspaceReport> = vec![WorkspaceReport {
        name: federation.workspaces[0].name.clone(),
        declares: federation.workspaces[0].declares.clone(),
        label: workspace_label(&federation.workspaces[0]).to_string(),
        root_dir: ctx.root_dir.clone(),
        findings: origin,
    }];
    for reached in federation.workspaces.iter().skip(1) {
        let label = workspace_label(reached).to_string();
        // Opened the way this CLI opens any workspace, rather than reusing the
        // read-only handle `descend` already has: `check` wants the same
        // workspace `prov check` would build standing inside the peer, config,
        // identity policy and all.
        let peer_ctx = find_root_quiet_at(&reached.root_dir)
            .map_err(|e| format!("`{label}` at {}: {e}", reached.root_dir.display()))?;
        let peer_ws = workspace(&peer_ctx)?;
        let mut findings = block_on(peer_ws.check(&peer_ctx.root_doc))?;
        let about_ctx = about_context(&peer_ctx)?;
        if let Some(finding) =
            block_on(peer_ws.check_about(&peer_ctx.root_doc, &peer_ctx.config, &about_ctx))?
        {
            findings.push(finding);
        }
        reports.push(WorkspaceReport {
            name: reached.name.clone(),
            declares: reached.declares.clone(),
            label,
            root_dir: peer_ctx.root_dir.clone(),
            findings,
        });
    }

    let total: usize = reports.iter().map(|r| r.findings.len()).sum();
    if as_json {
        print!(
            "{}",
            json::J::Arr(
                reports
                    .iter()
                    .map(|r| json::workspace_report(&r.name, &r.declares, &r.root_dir, &r.findings))
                    .collect()
            )
            .render()
        );
    } else {
        for report in &reports {
            // The header is narration and the findings are the machine value, so
            // they go to different streams — which also means a piped stdout is
            // still one self-describing finding per line.
            eprintln!(
                "── workspace {} ({}) ──",
                report.label,
                report.root_dir.display()
            );
            for finding in &report.findings {
                println!("{}: {finding}", report.label);
            }
        }
        // A boundary that was not crossed is why the report is shorter than the
        // federation — not a finding about either workspace, and not something
        // that changes the exit code. Once per distinct refusal: a dozen
        // references to one absent peer are one fact about this device.
        let mut refusals = Vec::new();
        collect_refusals(&federation.tree, &mut refusals);
        for refusal in &refusals {
            eprintln!("not followed: {refusal}");
        }
        let across = format!(" across {} workspace(s)", reports.len());
        if total == 0 {
            eprintln!("ok: no findings{across}");
        } else {
            eprintln!("{total} finding(s){across}");
        }
    }
    if total == 0 {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// One workspace's findings, and enough about the workspace to say whose they
/// are on a line that may be read on its own.
struct WorkspaceReport {
    /// The name the reference asked for (empty for an anonymous origin).
    name: String,
    /// What the workspace calls itself (empty when anonymous).
    declares: String,
    /// What to print: [`workspace_label`] over the two above.
    label: String,
    /// The directory every subject in `findings` is relative to.
    root_dir: PathBuf,
    findings: Vec<prov::Finding>,
}

/// Every boundary a descent refused, once per distinct reason, in the order the
/// walk met them.
fn collect_refusals(node: &prov::crossing::Node, out: &mut Vec<String>) {
    if let Some(prov::Boundary::Refused(refusal)) = &node.boundary {
        let line = match &node.kind {
            NodeKind::Foreign { workspace, .. } => format!("`{workspace}` — {refusal}"),
            _ => refusal.to_string(),
        };
        if !out.contains(&line) {
            out.push(line);
        }
    }
    for child in &node.children {
        collect_refusals(child, out);
    }
}

/// Show, refresh or deeply verify the manifest covering a directory.
///
/// `target` is whichever handle the caller has — the covered directory, the
/// node describing it, or the manifest document itself — because after a
/// rename the three no longer share a name, and requiring the right one would
/// be requiring the user to know which.
///
/// Narration to stderr; stdout carries the machine value, which is the manifest
/// document's path (bare/`--update`) or one line per failing file (`--verify`).
pub(crate) fn cmd_manifest(target: &Path, update: bool, verify: bool) -> CmdResult {
    let ctx = find_root()?;
    let mut ws = workspace(&ctx)?;
    let target_rel = ws_rel(&ctx, target)?;
    let node = resolve_manifest_node(&ws, &target_rel)?;

    if update {
        let changed = block_on(ws.update_manifest(&node))?;
        persist(&ctx, &mut ws)?;
        if changed.is_clean() {
            eprintln!("{}: already up to date", changed.manifest.display());
        } else {
            eprintln!(
                "{}: {} added, {} removed, {} changed",
                changed.manifest.display(),
                changed.added.len(),
                changed.removed.len(),
                changed.changed.len()
            );
        }
        println!("{}", changed.manifest.display());
        return Ok(ExitCode::SUCCESS);
    }

    if verify {
        let findings = block_on(ws.verify_manifest(&node))?;
        for finding in &findings {
            println!("{finding}");
        }
        return if findings.is_empty() {
            eprintln!("ok: every listed file matches its checksum");
            Ok(ExitCode::SUCCESS)
        } else {
            eprintln!("{} file(s) no longer match their checksum", findings.len());
            Ok(ExitCode::FAILURE)
        };
    }

    let status = block_on(ws.manifest_status(&node))?
        .ok_or_else(|| format!("{} declares no manifest", node.display()))?;
    eprintln!(
        "{}: {} file(s) under {}{}",
        status.manifest.display(),
        status.listed,
        status.root.display(),
        if status.hashed {
            ", each with a checksum"
        } else {
            ", no checksums (an inventory)"
        }
    );
    for path in &status.missing {
        eprintln!("  missing: {}", path.display());
    }
    for path in &status.extra {
        eprintln!("  unlisted: {}", path.display());
    }
    if !status.agrees() {
        eprintln!("the directory has drifted — `prov manifest --update` records it as it is now");
    }
    println!("{}", status.manifest.display());
    Ok(if status.agrees() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The node describing `target`, which may be the covered directory, the node
/// itself, or the manifest document. A rename separates their names, so all
/// three are accepted rather than making the user work out which one prov wants.
fn resolve_manifest_node(
    ws: &Workspace<StdFs, Minter, FileIndex>,
    target: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    // A directory: the reverse lookup by convention.
    if let Ok(meta) = block_on(ws.graph().stat(target))
        && meta.is_dir()
    {
        return block_on(ws.manifest_node_covering(target))?.ok_or_else(|| {
            format!(
                "{} is not covered by a manifest — `prov attach --manifest {}` covers it",
                target.display(),
                target.display()
            )
            .into()
        });
    }
    // The node itself.
    if block_on(ws.manifest_of(target))?.is_some() {
        return Ok(target.to_path_buf());
    }
    // The manifest document: find the node through the directory it covers, so
    // the answer is the same one every other route gives.
    if let Ok(manifest) = block_on(ws.graph().read_manifest(target))
        && let Ok(root) = manifest.checked_root(target)
        && let Some(node) = block_on(ws.manifest_node_covering(&root))?
    {
        return Ok(node);
    }
    Err(format!(
        "{} is not a manifest, a manifest node, or a covered directory",
        target.display()
    )
    .into())
}

/// Repair the findings: walk them, and for each one offer everything
/// [`remedies`](prov::Workspace::remedies) can do about it.
///
/// A finding rarely has *one* repair, which is why this is a numbered menu and
/// not a yes/no. A broken link may be pointed at a near-match or dropped; a
/// contested containment may be settled either way; an orphan may be adopted
/// under any container above it. Prov ranks them but does not choose.
///
/// `remedies` is consulted lazily, one finding at a time, so a repair applied
/// early correctly changes — or empties — the offers a later finding makes.
///
/// Every line here is narration and goes to **stderr**; stdout stays reserved
/// for the machine value, which for this command is the findings a repair
/// *introduced*.
fn cmd_check_fix(
    ctx: &mut Ctx,
    ws: &mut Workspace<StdFs, Minter, FileIndex>,
    root: &Path,
    findings: &[prov::Finding],
    mode: FixModeArg,
    only: Option<&Path>,
) -> CmdResult {
    let mut applied = 0usize;
    let mut needs_attention = 0usize;
    // Choices the user asked to repeat, by remedy kind. Only ever consulted when
    // the finding at hand offers exactly one remedy of that kind — otherwise
    // "all of this kind" would silently pick between candidates it never saw.
    let mut repeat: BTreeSet<prov::RemedyKind> = BTreeSet::new();
    for finding in findings {
        let remedies = block_on(ws.remedies(finding))?;
        if remedies.is_empty() {
            eprintln!("•  {finding}");
            needs_attention += 1;
            continue;
        }

        // `mechanical` applies what restates an authority and nothing else. A
        // finding whose repairs all involve a choice is left standing, and
        // counted, so the exit code still reports it.
        if mode == FixModeArg::Mechanical {
            match remedies
                .iter()
                .find(|r| r.warrant == prov::Warrant::Derived)
            {
                Some(remedy) => {
                    eprintln!("⚑  {finding}");
                    eprintln!("   → {}", remedy.effect);
                    block_on(ws.apply_fix(&remedy.fix))?;
                    applied += 1;
                }
                None => {
                    eprintln!("•  {finding}");
                    needs_attention += 1;
                }
            }
            continue;
        }

        // A choice the user already made, repeatable only because it is
        // unambiguous here: exactly one remedy of that kind, and never a
        // destructive one.
        let repeated = repeat.iter().find_map(|kind| {
            let mut of_kind = remedies.iter().filter(|r| r.kind == *kind);
            let only = of_kind.next()?;
            (of_kind.next().is_none() && only.warrant != prov::Warrant::Destructive).then_some(only)
        });
        if let Some(remedy) = repeated {
            eprintln!("⚑  {finding}");
            eprintln!("   → {}", remedy.effect);
            block_on(ws.apply_fix(&remedy.fix))?;
            applied += 1;
            continue;
        }

        // One remedy is a yes/no question and reads better asked as one — which
        // is also what every finding looked like before findings could offer more
        // than one repair. Several is a menu.
        eprintln!("⚑  {finding}");
        let single = remedies.len() == 1;
        if single {
            eprintln!("   → {}  [{}]", remedies[0].effect, remedies[0].warrant);
        } else {
            for (n, remedy) in remedies.iter().enumerate() {
                eprintln!("   {}) {} [{}]", n + 1, remedy.effect, remedy.warrant);
            }
        }
        let answer = prompt(&if single {
            "   apply? [y]es / [n]o / [a]ll of this kind / [q]uit: ".to_string()
        } else {
            format!(
                "   [1-{}] / [s]kip / [a]ll of this kind / [q]uit: ",
                remedies.len()
            )
        })?;
        // A bare number picks; `a` picks the first and repeats that kind. `y` is
        // accepted only where there is nothing to disambiguate — with a menu on
        // screen, "yes" does not name an answer. EOF reads as an empty line, so a
        // non-interactive `--fix` skips everything rather than guessing;
        // `--fix mechanical` is the scriptable door.
        let chosen = match answer.as_str() {
            "y" | "yes" if single => Some(&remedies[0]),
            "q" | "quit" => {
                eprintln!("stopped; {applied} fix(es) applied");
                break;
            }
            "" | "s" | "skip" | "n" | "no" => None,
            "a" | "all" => {
                let first = &remedies[0];
                if first.warrant == prov::Warrant::Destructive {
                    // Never batch a removal, however emphatically it was asked
                    // for: the whole reason a link is reported rather than
                    // rewritten is that it records intent.
                    eprintln!("   (won't repeat a destructive repair — choose it one at a time)");
                    None
                } else {
                    repeat.insert(first.kind);
                    Some(first)
                }
            }
            other => match other.parse::<usize>() {
                Ok(n) if (1..=remedies.len()).contains(&n) => Some(&remedies[n - 1]),
                _ => None,
            },
        };
        match chosen {
            Some(remedy) => {
                block_on(ws.apply_fix(&remedy.fix))?;
                applied += 1;
            }
            None => needs_attention += 1,
        }
    }
    // A fix may have registered an ID (an adopted `id`, or an id-link back-link):
    // make sure a registry exists and persist the identity changes to disk. Gate
    // on the index actually having changed, so a purely path-based fix (a
    // path-style inverse, adopting an orphan by path) does not bootstrap an empty
    // registry document as a side effect.
    if applied > 0 && ws.index().is_dirty() {
        ensure_registry(ctx)?;
        persist(ctx, ws)?;
    }
    if applied == 0 {
        // Nothing ran, so a second walk would return what the first one did.
        eprintln!("applied 0 fix(es); {needs_attention} finding(s) need attention");
        return Ok(ExitCode::SUCCESS);
    }

    // Re-check and diff against the run these fixes were chosen from. A fix is a
    // *mutation of the graph*, so "applied N" is a report of effort, not of
    // outcome — and the count of what still needs attention was computed before
    // any of them ran. Only a second walk can say what actually changed, and only
    // the three buckets can separate what these fixes repaired from what they
    // broke from what was already wrong.
    let mut after = block_on(ws.check(root))?;
    // Scope the second walk the way the first one was scoped, or the diff would
    // compare a filtered before against an unfiltered after and read every
    // untouched finding elsewhere in the workspace as newly introduced.
    if let Some(subject) = only {
        after.retain(|f| f.subject() == subject);
    }
    let diff = prov::CheckDiff::between(findings, &after);

    for finding in &diff.introduced {
        println!("{finding}");
    }
    eprintln!(
        "applied {applied} fix(es): {} finding(s) resolved, {} introduced, {} still outstanding",
        diff.fixed.len(),
        diff.introduced.len(),
        diff.pre_existing.len()
    );
    if diff.is_clean() {
        return Ok(ExitCode::SUCCESS);
    }
    // A repair that broke something is the one outcome a script must not miss.
    // Outstanding findings on their own are not this run's verdict, and keep the
    // exit code they have always had.
    eprintln!("a fix introduced the finding(s) above — run `prov check` and review");
    Ok(ExitCode::FAILURE)
}
