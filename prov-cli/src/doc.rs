//! The single-document verbs: `show`, `links`, `meta`, `get`, `body`, `render`,
//! `set`, `unset`, `edit`.
//!
//! These operate on one file by path and, for the read-only ones, need no
//! workspace at all — `prov meta notes.md` works on a stray file, in a
//! tarball, anywhere. The two writers (`set`, `unset`) look for a workspace
//! around the file and do its bookkeeping when they find one, and stay a bare
//! text rewrite when they do not; `edit` is the one verb here that requires
//! a root, because a hosted edit is what its timestamp claim rests on.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use prov::{Format, RelationSet, Value, block_on, edit, meta};

use crate::cli::MetaFormat;
use crate::clock::now_rfc3339;
use crate::session::{Session, load, machinery, updated_stamp, workspace_around, ws_rel};
use crate::term::edit_file;
use crate::{AnyError, CmdResult};

/// The relation vocabulary. For now the diaryx preset; configurable vocabularies
/// (and a `--relations` flag) come later.
fn relation_set() -> RelationSet {
    RelationSet::diaryx()
}

pub(crate) fn cmd_show(file: &Path) -> CmdResult {
    let (_, doc) = load(file)?;
    let set = relation_set();

    println!("{}", file.display());

    if let Some(title) = doc.meta.get("title").and_then(Value::as_str) {
        println!("  title: {title}");
    }

    if !doc.has_meta() {
        println!("  (no embedded metadata)");
        return Ok(ExitCode::SUCCESS);
    }

    let children = set.children(&fig::Value::from(&doc.meta));
    if let Some(spanning) = set.spanning_relation() {
        println!("  {spanning} ({} children):", children.len());
        for child in &children {
            println!("    - {child}");
        }
    }

    // Overlay relations (everything that isn't the spanning tree), grouped and
    // printed in the vocabulary's declared order.
    let spanning = set.spanning_relation();
    let edges = set.edges(&fig::Value::from(&doc.meta));
    for relation in set.relations() {
        if Some(relation.name.as_str()) == spanning {
            continue;
        }
        let targets: Vec<&str> = edges
            .iter()
            .filter(|e| e.relation == relation.name)
            .map(|e| e.target.as_str())
            .collect();
        if targets.is_empty() {
            continue;
        }
        println!("  {}:", relation.name);
        for target in targets {
            println!("    - {target}");
        }
    }

    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_links(file: &Path, relation: Option<&str>) -> CmdResult {
    let (_, doc) = load(file)?;
    for edge in relation_set().edges(&fig::Value::from(&doc.meta)) {
        if relation.is_none_or(|want| want == edge.relation) {
            println!("{}\t{}", edge.relation, edge.target);
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_meta(file: &Path, format: Option<MetaFormat>) -> CmdResult {
    let (_, doc) = load(file)?;
    let Some(mapping) = doc.meta.as_mapping() else {
        return Err("document has no embedded metadata".into());
    };
    // Default to the format the document already uses.
    let format = format
        .map(Format::from)
        .unwrap_or_else(|| doc.carrier.map(|c| c.format()).unwrap_or(Format::Yaml));
    print!("{}", meta::serialize_mapping(mapping, format)?);
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_get(file: &Path, key: &str) -> CmdResult {
    let (_, doc) = load(file)?;
    let mut value = &doc.meta;
    for part in key.split('.') {
        value = match part.parse::<usize>() {
            Ok(index) => value.as_sequence().and_then(|s| s.get(index)),
            Err(_) => value.get(part),
        }
        .ok_or_else(|| format!("no `{key}` in {}", file.display()))?;
    }
    match value {
        Value::Null => println!("null"),
        Value::Bool(b) => println!("{b}"),
        Value::Int(i) => println!("{i}"),
        Value::Float(f) => println!("{f}"),
        Value::String(s) => println!("{s}"),
        compound => {
            let format = doc.carrier.map(|c| c.format()).unwrap_or(Format::Yaml);
            print!("{}", meta::serialize_value(compound, format)?);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// A document's prose and the file declaring its grammar — [`prov::Body`] for a
/// caller that reads by real path rather than through a workspace.
///
/// `body`/`render` name a file directly and never open a workspace, so the
/// `content` target is resolved against the file's own directory here rather
/// than through `Graph::body`. A *separated* document keeps its prose in that
/// sibling; asking its `.yaml` node for `doc.body` gets the empty string, which
/// reads as "this document has no prose" and is not what happened.
fn body_of(file: &Path) -> Result<(String, PathBuf), Box<dyn std::error::Error>> {
    let (_, doc) = load(file)?;
    let Some(content) = doc.content_path(file) else {
        return Ok((doc.body, file.to_path_buf()));
    };
    if doc.is_attachment() {
        return Err(format!(
            "{}: attachment sidecar for {} — an opaque payload, not a prose body",
            file.display(),
            content.display(),
        )
        .into());
    }
    Ok((std::fs::read_to_string(&content)?, content))
}

pub(crate) fn cmd_body(file: &Path) -> CmdResult {
    let (text, _) = body_of(file)?;
    print!("{text}");
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_render(file: &Path) -> CmdResult {
    let (text, from) = body_of(file)?;
    let format = prov::ContentFormat::from_extension(&from).ok_or_else(|| {
        format!(
            "{}: not a recognized body format (expected .md/.markdown or .dj/.djot)",
            from.display()
        )
    })?;
    let html = prov::render_html(&text, format)?;
    print!("{html}");
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_set(file: &Path, key: &str, value: &str) -> CmdResult {
    let (text, doc) = load(file)?;
    let updated = edit::set_in_text(&text, doc.carrier, key, edit::infer_scalar(value))?;
    write_field_edit(file, key, &updated)?;
    println!("{}", file.display());
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_unset(file: &Path, key: &str) -> CmdResult {
    let (text, doc) = load(file)?;
    let updated = edit::unset_in_text(&text, doc.carrier, key)?;
    write_field_edit(file, key, &updated)?;
    println!("{}", file.display());
    Ok(ExitCode::SUCCESS)
}

/// Land a single-field edit — the new `text` of `file` after `key` was set or
/// removed — with the bookkeeping the edit implies when the file is a
/// document in a workspace, and as a bare rewrite when it is not.
///
/// Inside a workspace this is the same seam `edit` uses: one journaled write
/// that stamps the `updated` field with the current instant and restates the
/// content checksum where the document records one — because an edit that
/// closes a task with `set status done` is an edit, and a `check` that later
/// asks when the document last changed should get the answer. Outside one
/// (a file in a tarball, a stray document, a workspace that cannot be opened)
/// the command stays what it was — a text rewrite and nothing else — so a
/// script reading and writing loose files keeps working.
///
/// The one field the stamp defers to is its own: `set <file> updated <at>`
/// is the user naming the instant, and restamping it with now would make the
/// argument unreachable. A dotted `key` is compared by its first segment, so
/// setting *inside* the field defers too.
fn write_field_edit(file: &Path, key: &str, text: &str) -> Result<(), AnyError> {
    let dir = std::env::current_dir()?.join(file);
    let ctx = dir.parent().and_then(workspace_around);
    let Some(ctx) = ctx else {
        std::fs::write(file, text)?;
        return Ok(());
    };
    let Ok(rel) = ws_rel(&ctx, file) else {
        std::fs::write(file, text)?;
        return Ok(());
    };
    let mut session = Session::over(ctx)?;
    let now = now_rfc3339();
    let machinery = machinery(&session.ctx, &session.ws)?;
    let own_field = key.split('.').next() == Some(session.ctx.config.updated.as_str());
    let stamp = (!own_field)
        .then(|| updated_stamp(&session.ctx, &machinery, &rel, &now))
        .flatten();
    block_on(session.ws.save_document(&rel, text, stamp))?;
    let stamped = stamp.map(|(field, _)| field.to_string());
    session.commit()?;
    if let Some(field) = stamped {
        eprintln!("{} — stamped `{field}`", rel.display());
    }
    Ok(())
}

pub(crate) fn cmd_edit(file: &Path) -> CmdResult {
    // Snapshot before the editor so we can tell whether the user actually changed
    // anything — an open-and-quit must not bump the timestamp or restamp.
    let before = std::fs::read(file).ok();
    edit_file(file)?;
    let changed = std::fs::read(file).ok() != before;

    let mut session = Session::open()?;
    let rel = ws_rel(&session.ctx, file)?;
    if !changed {
        eprintln!("edited {} (no changes)", rel.display());
        println!("{}", rel.display());
        return Ok(ExitCode::SUCCESS);
    }

    // The bookkeeping a real edit implies, in one crash-safe write: restamp the
    // body checksum (under `full`), and stamp the `updated` field (when
    // configured) with the current time — RFC 3339 UTC, the machine-standard
    // value the library reads back (DESIGN §2). Both self-gate, so this is a
    // no-op when neither is enabled.
    let now = now_rfc3339();
    let machinery = machinery(&session.ctx, &session.ws)?;
    let updated = updated_stamp(&session.ctx, &machinery, &rel, &now);
    let wrote = block_on(session.ws.record_content_update(&rel, updated))?;
    let stamped = updated.map(|(field, _)| field.to_string());
    session.commit()?;

    match (wrote, stamped) {
        (true, Some(field)) => eprintln!("edited {} — stamped `{field}` + checksum", rel.display()),
        (true, None) => eprintln!("edited {} — content checksum updated", rel.display()),
        (false, Some(field)) => eprintln!("edited {} — stamped `{field}`", rel.display()),
        _ => eprintln!("edited {}", rel.display()),
    }
    println!("{}", rel.display());
    Ok(ExitCode::SUCCESS)
}
