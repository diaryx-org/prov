//! `stamp` and `confirm` — the provenance a document carries about itself.
//!
//! Both write a claim into a document's own metadata on the strength of
//! evidence gathered at the command line: `stamp` restates the content
//! checksum and, where the bytes drifted or the user named the file, the
//! `updated` instant; `confirm` appends the actor's word that the document
//! was read and found right. The clock is [`crate::clock`], the actor is
//! [`crate::actor`], and which documents may carry a timestamp at all is the
//! session's [`updated_stamp`] — shared with `edit`, `set` and `unset` so the
//! verbs cannot disagree.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use prov::{ContentState, block_on};

use crate::CmdResult;
use crate::actor;
use crate::clock::now_rfc3339;
use crate::session::{Ctx, Session, machinery, updated_stamp, ws_rel};

/// `confirm` — append one entry to a document's `confirmed` list, or show the
/// list with `--show`.
///
/// The actor is a device-local fact (`actor::resolve`), the instant is this
/// process's clock, and the library does the rest — including refusing a
/// document whose checksum has drifted, since a confirmation cannot vouch its
/// way past a fixity mismatch. Machinery stores and the generated `about` page
/// are refused here, before the library is asked: a store is re-laid-out by
/// prov, and the page is rewritten whole, so a confirmation on either would be
/// a claim about a file prov itself rewrites.
pub(crate) fn cmd_confirm(file: &Path, by: Option<String>, show: bool) -> CmdResult {
    let mut session = Session::open()?;
    let rel = ws_rel(&session.ctx, file)?;

    if show {
        let standing = block_on(session.ws.confirmations(&rel))?;
        for entry in &standing.live {
            println!("{}\t{}\tstands", entry.by, entry.at);
        }
        for entry in &standing.stale {
            println!("{}\t{}\tstale", entry.by, entry.at);
        }
        eprintln!("{}: {}", rel.display(), standing.tier());
        return Ok(ExitCode::SUCCESS);
    }

    let machinery = machinery(&session.ctx, &session.ws)?;
    if machinery.iter().any(|m| m == &rel) {
        return Err(format!(
            "{}: a machinery store is re-laid-out by prov, so it carries no confirmation",
            rel.display()
        )
        .into());
    }
    if block_on(session.ws.about_path(&session.ctx.root_doc))?.as_deref() == Some(rel.as_path()) {
        return Err(format!(
            "{}: the generated page is rewritten whole by `prov about`, so it carries no confirmation",
            rel.display()
        )
        .into());
    }

    let actor = actor::resolve(by)?;
    let now = now_rfc3339();
    let entry = block_on(session.ws.confirm(&rel, &actor, &now))?;
    session.commit()?;
    let standing = block_on(session.ws.confirmations(&rel))?;
    eprintln!(
        "confirmed {} — {} at {} ({})",
        rel.display(),
        entry.by,
        entry.at,
        standing.tier()
    );
    println!("{}", rel.display());
    Ok(ExitCode::SUCCESS)
}

/// `stamp` — the bookkeeping of an edit prov did not host.
///
/// [`cmd_edit`](crate::doc::cmd_edit) already does this for an edit it launched the editor for, and
/// it can be unconditional about the timestamp because it snapshotted the bytes
/// before handing over. Nothing here saw the edit happen, so the checksum is
/// the only evidence available, and [`ContentState`] is how that evidence is
/// read:
///
/// - **`Drifted`** — the bytes changed. Both stamps land, in the single
///   crash-safe write `record_content_update` makes of them.
/// - **`Intact`** — the bytes did not change. Nothing is written, which is what
///   makes re-running this free and puts it safely in a sync hook.
/// - **`Unrecorded`** — no checksum on record (fixity off, a document fixity
///   does not cover, or one that predates it). There is no evidence either way,
///   so a *named* target is
///   stamped on the strength of the user having named it, and `--all` skips it:
///   naming one file asserts an edit, sweeping a workspace does not.
/// - **`Unverifiable`** — a digest from an algorithm this build cannot compute.
///   Skipped, never overwritten, exactly as `check` leaves it alone.
///
/// Narration to stderr; stdout carries the machine value, one stamped path per
/// line — so `prov stamp --all | xargs …` gets what actually changed.
pub(crate) fn cmd_stamp(
    target: Option<&Path>,
    all: bool,
    no_timestamp: bool,
    dry_run: bool,
) -> CmdResult {
    let mut session = Session::open()?;

    // What to consider, and whether a name was put to each one.
    //
    // `--all` takes the document population `check` validates, not the *file*
    // set: a shadowed payload (`attach --opaque`) is bytes prov holds without
    // interpreting, and any `content_hash` inside one belongs to the exhibit
    // rather than to this workspace. An ordinary attachment payload is still in
    // here and still will not parse as a document — it is covered through its
    // sidecar, and skipped below where it is found.
    let targets: Vec<PathBuf> = match (target, all) {
        (Some(path), _) => vec![ws_rel(&session.ctx, path)?],
        (None, true) => block_on(session.ws.reachable_documents_from(&session.ctx.root_doc))?
            .into_iter()
            .collect(),
        (None, false) => {
            return Err("nothing to stamp: name a document, or pass --all".into());
        }
    };
    let named = target.is_some();

    let now = now_rfc3339();
    // The workspace may not record an `updated` field at all, in which case
    // there is no timestamp half to this command and only the checksum moves.
    // Which documents may carry one is decided per path below, so that naming
    // the workspace node stamps its checksum (it has none) and never its
    // config.
    let machinery = machinery(&session.ctx, &session.ws)?;

    let mut stamped = 0usize;
    let mut seeded = 0usize;
    let mut skipped = 0usize;
    for path in targets {
        let state = match block_on(session.ws.content_state(&path)) {
            Ok(state) => state,
            // Under `--all` this is a reached file that is not a document (an
            // attachment's payload, a manifest's covered bytes) — not an error,
            // just not this command's business. A named target that cannot be
            // read is.
            Err(_) if !named => continue,
            Err(e) => return Err(format!("{}: {e}", path.display()).into()),
        };
        // Which stamps this document has earned. The two halves are decided
        // separately because they rest on different evidence: a checksum
        // *restates* the bytes, so it is owed wherever it is missing or wrong,
        // while a timestamp *asserts* that an edit happened, which only drift
        // or the user naming the file can establish.
        //
        // That split is what makes `--all` worth running: it brings a whole
        // workspace's fixity up to date — seeding the documents that never had
        // a checksum, correcting the ones that drifted — and claims an edit
        // time for exactly the drifted ones, never for a document it merely
        // read.
        let (write, claims_edit) = match state {
            ContentState::Drifted => (true, true),
            ContentState::Unrecorded => (true, named),
            ContentState::Intact | ContentState::Unverifiable => (false, false),
        };
        let timestamp = (claims_edit && !no_timestamp)
            .then(|| updated_stamp(&session.ctx, &machinery, &path, &now))
            .flatten();
        if !write {
            if named {
                eprintln!(
                    "{}: {} — nothing to stamp",
                    path.display(),
                    match state {
                        ContentState::Intact => "checksum still matches the bytes",
                        ContentState::Unverifiable =>
                            "checksum uses an algorithm this build cannot compute",
                        _ => "unchanged",
                    }
                );
            }
            skipped += 1;
            continue;
        }
        if dry_run {
            // Both states that reach here are being stamped *for* the
            // checksum, so that is what a dry run names. Whether one actually
            // lands on an unrecorded document depends on the workspace's fixity
            // tier covering its kind, which only the write answers — and which
            // "would" already leaves open.
            eprintln!(
                "{}: would stamp {}",
                path.display(),
                stamp_summary(&session.ctx, timestamp, true)
            );
            println!("{}", path.display());
            if state == ContentState::Unrecorded {
                seeded += 1;
            } else {
                stamped += 1;
            }
            continue;
        }
        if block_on(session.ws.record_content_update(&path, timestamp))? {
            // Whether a checksum actually landed is not knowable up front for an
            // `Unrecorded` document: `record_content_update` writes one only if
            // the workspace covers this document at all — fixity on, and the
            // document pointing at a file of its own — which is a decision the
            // library makes and does not report. Re-reading
            // the state is how the narration stays a claim about what happened
            // rather than about what was attempted — one extra read, and only
            // for a document that was actually written.
            let hashed = match state {
                ContentState::Drifted => true,
                _ => block_on(session.ws.content_state(&path))? == ContentState::Intact,
            };
            eprintln!(
                "{}: stamped {}",
                path.display(),
                stamp_summary(&session.ctx, timestamp, hashed)
            );
            println!("{}", path.display());
            if state == ContentState::Unrecorded {
                seeded += 1;
            } else {
                stamped += 1;
            }
        } else {
            // `record_content_update` self-gates on the same two questions, so
            // it can decline what `content_state` waved through — a document
            // fixity does not cover (one that keeps its prose inline, so there
            // is no separate file to vouch for), with no `updated` field
            // configured either. Nothing to write, and nothing wrong.
            if named {
                eprintln!(
                    "{}: this workspace records neither a checksum nor a timestamp for it",
                    path.display()
                );
            }
            skipped += 1;
        }
    }

    if all {
        let verb = if dry_run { "would stamp" } else { "stamped" };
        eprintln!(
            "{verb} {stamped} drifted document(s), seeded {seeded} that had no checksum; \
{skipped} unchanged"
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Which stamps a write landed, for one narration line.
fn stamp_summary(ctx: &Ctx, timestamp: Option<(&str, &str)>, hashed: bool) -> String {
    match (hashed, timestamp) {
        (true, Some(_)) => format!("`{}` + checksum", ctx.config.updated),
        (true, None) => "checksum".into(),
        (false, Some(_)) => format!("`{}`", ctx.config.updated),
        // `record_content_update` reported a write, so something moved; the
        // only remaining possibility is a timestamp field this narration was
        // not given. Unreachable in practice, and not worth a panic.
        (false, None) => "nothing".into(),
    }
}
