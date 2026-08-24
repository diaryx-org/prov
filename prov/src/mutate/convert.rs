//! `convert` — restating a document in a different spelling.
//!
//! Four axes, one discipline: a link's [`LinkStyle`](prov_graph::link::LinkStyle),
//! the metadata block's frontmatter *language*, its *embedding shape*, and the
//! body prose's *grammar*. Each is per-file by default (DESIGN §8) — how a
//! document spells its own links, metadata and prose is its own to declare, so a
//! workspace may sit in a mixed style and still be `check`-clean — with
//! `recursive` sweeping the spanning subtree as one change set, so a failure two
//! thirds of the way down converts nothing.
//!
//! Three of the four move no document. The fourth cannot avoid it: a body's
//! grammar is declared by its *filename*, so converting it renames the file and
//! every inbound link follows — see
//! [`convert_content_format`](Workspace::convert_content_format).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use fig::Segment;

use crate::identity::IdentityPolicy;
use crate::workspace::Workspace;
use prov_graph::content::{ContentFormat, transcode};
use prov_graph::document::{Document, EmbedStyle, MetaCarrier};
use prov_graph::error::{Error, Result};
use prov_graph::link::{self, Link};
use prov_graph::meta::Value;
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

use super::maintain::splice_body;

/// One prose file's move under a content-format conversion: what to transcode,
/// where it lands, and (for a separated pair) the node whose `content` pointer
/// has to follow it.
struct TranscodePlan {
    /// The file holding the prose — the document itself, or its separated body.
    from: PathBuf,
    /// Where it lands: the same name carrying the target grammar's extension.
    to: PathBuf,
    /// The grammar `from`'s extension currently declares.
    from_format: ContentFormat,
    /// The node owning this prose, when it is a separated body file — `None` when
    /// the prose *is* the document.
    node: Option<PathBuf>,
    /// The `content` value naming `to` (its basename), for that node.
    content_ref: String,
}

/// A document a conversion cannot act on: an error when the caller named it
/// directly, a silent skip when it merely fell inside a recursive sweep.
fn skip_or_err<T>(named: bool, path: &Path, why: &str) -> Result<Option<T>> {
    if named {
        return Err(Error::Structure(format!(
            "{}: nothing to convert — {why}",
            path.display()
        )));
    }
    Ok(None)
}

/// Which axis a metadata reformat varies — the shared core of
/// [`Workspace::convert_meta_format`] and [`Workspace::convert_meta_embed`]. Both
/// re-emit a document's block in a new archetype resolved from the document's
/// *other* axis: `Format` keeps the embedding shape and swaps the frontmatter
/// language; `Embed` keeps the language and swaps the shape.
#[derive(Clone, Copy)]
enum ReformatAxis {
    /// Vary the frontmatter language (`metadata.format`), keep the embedding shape.
    Format(fig::Format),
    /// Vary the embedding shape (`metadata.embed`), keep the frontmatter language.
    Embed(EmbedStyle),
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Convert the **path-form** links the document at `file` declares into
    /// `style` — re-spelling each relative/absolute path target in the target
    /// [`LinkStyle`](prov_graph::link::LinkStyle) (root `/a`, relative `../a`, or bare
    /// canonical `a`) while its resolved destination, label, and wrapper stay
    /// exactly the same. Id-form, external, and nominal (alias) targets are left
    /// untouched — `style` governs only how a *path* is written. Both frontmatter
    /// relation links and body `[[…]]`/`[](…)` links are converted.
    ///
    /// **Per-file by default (DESIGN §8).** Converting `file` restyles only the
    /// links *it* declares; links elsewhere pointing *at* `file` are those
    /// documents' to convert (so a workspace can sit in a mixed style, which is
    /// valid and `check`-clean). With `recursive`, the same conversion is applied
    /// to every document in `file`'s spanning subtree. Returns the paths of the
    /// documents actually rewritten.
    pub async fn convert_link_style(
        &mut self,
        file: &Path,
        style: prov_graph::link::LinkStyle,
        recursive: bool,
    ) -> Result<Vec<PathBuf>> {
        // A recursive sweep reads the subtree twice: `spanning_subtree` loads
        // every document to find the children, and `restyle_document` loads each
        // one again to rewrite it. One scope halves that.
        let _scope = self.read_scope();
        let file = link::normalize(file);
        if !self.exists(&file).await? {
            return Err(Error::NotFound(file.to_path_buf()));
        }
        let targets = if recursive {
            self.spanning_subtree(&file).await?
        } else {
            vec![file]
        };
        // One set for the whole subtree, not one per document: a recursive
        // restyle is a single edit to the reader ("convert this subtree"), so a
        // failure two thirds of the way down should not leave the other third
        // converted. Every document is independent here — nothing reads what an
        // earlier one wrote — so the whole sweep stages cleanly.
        let mut cs = self.change();
        let mut changed = Vec::new();
        for path in &targets {
            if let Some(text) = self.restyle_document(path, style).await? {
                cs.write(path, text);
                changed.push(path.clone());
            }
        }
        self.commit(cs).await?;
        Ok(changed)
    }

    /// Convert the metadata block of the document at `file` to a different
    /// frontmatter language — re-emitting its embedded metadata in `format` while
    /// keeping the document's *embedding shape* (delimited frontmatter stays
    /// delimited, a ```` ```lang ```` code block stays a code block, an HTML island
    /// stays an island) and every value. Only the serialization changes; comments
    /// do not survive a cross-format rewrite (a YAML comment has no JSON home).
    ///
    /// **Per-file by default (DESIGN §8),** like [`convert_link_style`](Self::convert_link_style):
    /// a document's metadata format is its own to declare, so a workspace may hold
    /// a mix. With `recursive`, every document in `file`'s spanning subtree is
    /// converted. A document already in `format`, or one with no metadata block, is
    /// left untouched; a *whole-file* (separate/config) document is not an embedded
    /// block to re-fence — converting one would rename the file and re-point its
    /// links — so it is an error to name one directly and is skipped under a
    /// recursive sweep. Returns the paths of the documents actually rewritten.
    pub async fn convert_meta_format(
        &mut self,
        file: &Path,
        format: fig::Format,
        recursive: bool,
    ) -> Result<Vec<PathBuf>> {
        self.reformat_sweep(file, ReformatAxis::Format(format), recursive)
            .await
    }

    /// Convert the metadata block of the document at `file` to a different
    /// *embedding shape* — re-emitting its frontmatter as `style`
    /// (`delimited`/`code_block`/`html_script`/`html_code`) while keeping its
    /// frontmatter *language* and every value. The companion of
    /// [`convert_meta_format`](Self::convert_meta_format) on the other metadata axis:
    /// where that keeps the shape and swaps the language, this keeps the language
    /// and swaps the shape — so a `delimited` YAML block can become a ```` ```yaml ````
    /// code block that can then hold fig.
    ///
    /// Same discipline as its companion: per-file by default (`recursive` sweeps the
    /// spanning subtree), a no-op when the document is already in `style` or carries
    /// no block. Two shapes are out of scope and rejected: `separate` (moving
    /// metadata to a sibling file is a move, not a re-fence), and a language the
    /// target shape cannot carry (`delimited` + fig — fig has no delimiter syntax).
    /// Returns the paths of the documents actually rewritten.
    pub async fn convert_meta_embed(
        &mut self,
        file: &Path,
        style: EmbedStyle,
        recursive: bool,
    ) -> Result<Vec<PathBuf>> {
        self.reformat_sweep(file, ReformatAxis::Embed(style), recursive)
            .await
    }

    /// Convert the **body prose** of the document at `file` into the `format`
    /// grammar — re-serializing Markdown as Djot (or either as HTML) through
    /// `twig`, and renaming the file so its extension declares what it now holds.
    ///
    /// The axis that moves a document, and it has to: a metadata block declares
    /// its own language *inside* the file, so
    /// [`convert_meta_format`](Self::convert_meta_format) rewrites in place, while
    /// a body's grammar is declared by its **filename** — `ContentFormat::from_extension`
    /// is prov's only reading of it, and every other tool on the machine reads it
    /// the same way. So converting body prose is a transcode *and* a rename, and
    /// the rename drags the workspace's inbound links with it: every reference
    /// that resolved to `notes.md` is retargeted to `notes.dj`, exactly as
    /// [`rename`](Workspace::rename) would. `colophon:<id>` references are left
    /// alone — the registry's `id → path` update keeps those resolving.
    ///
    /// **Per-file by default (DESIGN §8)**, like its sibling axes: a mixed-grammar
    /// workspace is valid and `check`-clean. With `recursive`, the whole spanning
    /// subtree converts as one change set — and as *one* set, not one per
    /// document, which is what lets a mover that links to another mover come out
    /// right (see [`collect_inbound_rewrites_multi`](Workspace::collect_inbound_rewrites_multi)).
    ///
    /// A **separated** document converts its body file rather than its node: the
    /// prose is what has a grammar, so `notes.md` becomes `notes.dj` and the
    /// node's `content` pointer follows it. An attachment's opaque payload has no
    /// prose to transcode and a config document has no body; both are no-ops,
    /// hard errors when named directly and skipped in a sweep.
    ///
    /// `force` gates the lossy directions — anything with HTML at either end (see
    /// [`ContentFormat::is_lossy_to`]). Returns the *new* paths of the prose files
    /// actually converted.
    ///
    /// A document reached by a *pointer* rather than by the spanning relation —
    /// the `about` page is the one prov authors — converts like any other: it
    /// declares no `part_of`, but
    /// [`spanning_root`](Workspace::spanning_root) resolves that to the
    /// workspace's actual root, so the census covers the document holding the
    /// pointer and the pointer follows the move.
    pub async fn convert_content_format(
        &mut self,
        file: &Path,
        format: ContentFormat,
        recursive: bool,
        force: bool,
    ) -> Result<Vec<PathBuf>> {
        // The heaviest sweep here, and the one with the most passes over one
        // document: `spanning_subtree` to find the targets, `plan_transcode` per
        // target, the inbound census, and a load per source it rewrites. One
        // scope covers all four.
        let _scope = self.read_scope();
        let file = link::normalize(file);
        if !self.exists(&file).await? {
            return Err(Error::NotFound(file.to_path_buf()));
        }
        let targets = if recursive {
            self.spanning_subtree(&file).await?
        } else {
            vec![file.clone()]
        };

        // 1. Plan every prose file that moves, before touching anything. A
        //    conversion is refused whole (an unforced lossy direction, an
        //    occupied destination) rather than half-applied.
        let mut plans = Vec::new();
        for path in &targets {
            if let Some(plan) = self
                .plan_transcode(path, format, force, path == &file)
                .await?
            {
                plans.push(plan);
            }
        }
        if plans.is_empty() {
            return Ok(Vec::new());
        }
        let mut destinations = BTreeSet::new();
        for plan in &plans {
            // A rename overwrites, and an overwrite is the one thing staging
            // cannot undo — the clobbered bytes are gone before anything could
            // have copied them. So this is a guard, not something rollback
            // covers. (`plan_transcode` derives the destination, so the caller
            // never sees the collision coming either.)
            if self.exists(&plan.to).await? {
                return Err(Error::AlreadyExists(plan.to.clone()));
            }
            // Two movers can also collide with *each other*, which the on-disk
            // check cannot see because neither destination exists yet: a grammar
            // spells more than one extension, so `a.md` and `a.markdown` both
            // convert to `a.dj`. Staged, that is two renames onto one path — the
            // second silently destroying the first.
            if !destinations.insert(plan.to.clone()) {
                return Err(Error::Structure(format!(
                    "{} and another document in this conversion both become {} — \
                     rename one first",
                    plan.from.display(),
                    plan.to.display(),
                )));
            }
        }

        // 2. The moves, as one map: inbound links are maintained against the whole
        //    set at once, so a document that references two movers is rewritten
        //    once with both retargetings rather than twice, each losing the other.
        let moves: BTreeMap<PathBuf, PathBuf> = plans
            .iter()
            .filter(|p| p.node.is_none()) // a separated body is reached by `content`, not by links
            .map(|p| (p.from.clone(), p.to.clone()))
            .collect();
        for (from, to) in &moves {
            if let Some(id) = self.index().id_for_path(from)
                && let Some(conflict) = self.move_conflict(&id, to)
            {
                return Err(conflict.into());
            }
        }
        let (_spanning, inverse) = self.spanning_pair()?;
        let root = self.spanning_root(&file, &inverse).await?;
        let mut rewrites = self.collect_inbound_rewrites_multi(&root, &moves).await?;

        // 3. Transcode each mover, over the inbound-rewritten text where there is
        //    one — a mover that links to a fellow mover is both a source and a
        //    destination of this sweep, and the rewrite has to survive into the
        //    text that gets transcoded.
        let mut cs = self.change();
        let mut converted = Vec::new();
        for plan in &plans {
            let staged = rewrites.remove(&plan.from);
            let text = match (staged, plan.node.is_some()) {
                // A separated body file is prose all the way down: no metadata
                // block to preserve, so the whole text transcodes.
                (staged, true) => {
                    let raw = match staged {
                        Some(text) => text,
                        None => self.read_text(&plan.from).await?,
                    };
                    transcode(&raw, plan.from_format, format)?
                }
                // A combined document keeps its metadata block byte-for-byte and
                // swaps only the prose beneath it.
                (staged, false) => {
                    let text = match staged {
                        Some(text) => text,
                        None => self.load(&plan.from).await?.0,
                    };
                    let body = Document::parse(&plan.from, &text)?.body;
                    let new_body = transcode(&body, plan.from_format, format)?;
                    splice_body(&text, &body, &new_body)
                }
            };
            cs.rename(&plan.from, &plan.to);
            cs.write(&plan.to, text);
            // A separated node stays put; only its `content` pointer follows the
            // body's new name. That edit is made *over* the node's inbound
            // rewrite, and takes it out of `rewrites` in the process — the node is
            // an ordinary document that may well link at a fellow mover, and
            // leaving it in the set would let the trailing loop below write the
            // link-rewritten text back over this `content` repoint, pointing the
            // node at a body file the rename just emptied.
            if let Some(node) = &plan.node {
                let node_text = match rewrites.remove(node) {
                    Some(text) => text,
                    None => self.load(node).await?.0,
                };
                let node_doc = Document::parse(node, &node_text)?;
                if let Some(carrier) = node_doc.carrier {
                    let mut editor = MetaEditor::open(&node_text, carrier)?;
                    editor.replace_value(
                        &[Segment::Key("content")],
                        fig::Value::Str(plan.content_ref.clone()),
                    )?;
                    cs.write(node, editor.render()?);
                }
            }
            converted.push(plan.to.clone());
        }
        // Whatever is left in `rewrites` is a source that links at a mover without
        // being one itself.
        for (source, text) in rewrites {
            cs.write(source, text);
        }
        // The registry follows each moved node, so every `colophon:<id>` pointing
        // at one survives the conversion untouched — the point of an id link.
        for (from, to) in &moves {
            if let Some(id) = self.index().id_for_path(from) {
                self.index_mut().set_path(&id, to);
            }
        }
        self.commit(cs).await?;
        Ok(converted)
    }

    /// Plan the transcode of the prose belonging to the document at `path`, or
    /// `None` when there is nothing to convert (already in `format`, no body,
    /// an opaque payload). `named` is true when the caller pointed at this
    /// document directly, so a document out of scope is a hard error rather than
    /// a silent skip — the same courtesy [`reformat_document`](Self::reformat_document)
    /// extends on the metadata axes.
    async fn plan_transcode(
        &self,
        path: &Path,
        format: ContentFormat,
        force: bool,
        named: bool,
    ) -> Result<Option<TranscodePlan>> {
        let (_, doc) = self.load(path).await?;
        // Which file actually holds the prose: this one, or the separated body it
        // points at. The node of a separated pair has no grammar of its own — its
        // extension names a *metadata* format.
        let (prose, node) = match super::maintain::content_target(&doc, path) {
            Some(body) => (body, Some(path.to_path_buf())),
            None => (path.to_path_buf(), None),
        };
        if prov_graph::document::is_opaque_payload(&prose) {
            return skip_or_err(named, path, "its content is an opaque payload, not prose");
        }
        let Some(from_format) = ContentFormat::from_extension(&prose) else {
            return skip_or_err(named, path, "it has no body prose to convert");
        };
        if from_format == format {
            return Ok(None);
        }
        if from_format.is_lossy_to(format) && !force {
            return Err(Error::Structure(format!(
                "{}: converting {} to {} is lossy — the authored markup does not survive the \
                 round trip; pass --force to do it anyway",
                prose.display(),
                from_format.as_config_str(),
                format.as_config_str(),
            )));
        }
        let to = prose.with_extension(format.extension());
        let content_ref = to
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        Ok(Some(TranscodePlan {
            from: prose,
            to,
            from_format,
            node,
            content_ref,
        }))
    }

    /// The shared engine behind [`convert_meta_format`](Self::convert_meta_format)
    /// and [`convert_meta_embed`](Self::convert_meta_embed): resolve the target
    /// document set (this file, or its spanning subtree under `recursive`) and
    /// reformat each along `axis`, staging the whole sweep as one change set.
    ///
    /// One set for the subtree, not one per document — a recursive convert is a
    /// single edit to the reader, so a failure partway down leaves nothing
    /// converted. Every document is independent (nothing reads what another wrote),
    /// so the sweep stages cleanly. `named` gates whether an out-of-scope document
    /// is a hard error (the user pointed at it) or a skip (it merely fell inside
    /// the subtree).
    async fn reformat_sweep(
        &mut self,
        file: &Path,
        axis: ReformatAxis,
        recursive: bool,
    ) -> Result<Vec<PathBuf>> {
        // Both metadata axes come through here, and both read the subtree twice
        // — once to find it, once to re-emit each document. See
        // `convert_link_style`.
        let _scope = self.read_scope();
        let file = link::normalize(file);
        if !self.exists(&file).await? {
            return Err(Error::NotFound(file.to_path_buf()));
        }
        let targets = if recursive {
            self.spanning_subtree(&file).await?
        } else {
            vec![file.clone()]
        };
        let mut cs = self.change();
        let mut changed = Vec::new();
        for path in &targets {
            let named = path == &file;
            if let Some(text) = self.reformat_document(path, axis, named).await? {
                cs.write(path, text);
                changed.push(path.clone());
            }
        }
        self.commit(cs).await?;
        Ok(changed)
    }

    /// Reformat the metadata block of the document at `path` along `axis`, returning
    /// its new text, or `None` when nothing should change (no metadata block, or the
    /// document already sits at the requested value, or an out-of-scope whole-file
    /// document under a recursive sweep). `named` is true when the caller pointed at
    /// this document directly, so an out-of-scope document is a hard error rather
    /// than a silent skip.
    ///
    /// The two axes converge on the same reconstruction: resolve a target
    /// [`EmbedType`](prov_graph::document::EmbedType) from the document's `(style, format)`
    /// pair with one coordinate replaced, then re-emit the block in it. `Format`
    /// replaces the format and keeps the current style; `Embed` replaces the style
    /// and keeps the current format.
    async fn reformat_document(
        &self,
        path: &Path,
        axis: ReformatAxis,
        named: bool,
    ) -> Result<Option<String>> {
        let (text, doc) = self.load(path).await?;
        let Some(mapping) = doc.meta.as_mapping() else {
            return Ok(None); // no metadata block to convert
        };
        let kind = match doc.carrier {
            Some(MetaCarrier::Fenced(kind)) => kind,
            // The whole file *is* the metadata: re-embedding it means creating or
            // deleting a fenced host and moving the body — a move, not a re-fence,
            // and out of scope. An error when named directly; skipped in a sweep.
            Some(MetaCarrier::WholeFile(_)) if named => {
                return Err(Error::Structure(format!(
                    "{}: whole-file (separate) metadata — its format is its file \
                     extension and its shape is its own file; converting it is a move, \
                     not supported by `convert`",
                    path.display()
                )));
            }
            _ => return Ok(None),
        };
        // Resolve the target `(style, format)` from the current pair with the axis's
        // coordinate replaced; a document already at the requested value is a no-op.
        let (style, format) = match axis {
            ReformatAxis::Format(format) => {
                if kind.inner_format() == format {
                    return Ok(None);
                }
                (prov_graph::document::embed_style_of(kind), format)
            }
            ReformatAxis::Embed(style) => {
                if prov_graph::document::embed_style_of(kind) == style {
                    return Ok(None);
                }
                // `separate` is a whole-file sidecar, not a fenced shape — the same
                // move `WholeFile` above is, in the other direction.
                if style == EmbedStyle::Separate {
                    return Err(Error::Structure(format!(
                        "{}: `separate` moves metadata into a sibling file and re-points \
                         its links — a move, not supported by `convert`",
                        path.display()
                    )));
                }
                (style, kind.inner_format())
            }
        };
        let target = match prov_graph::document::embed_carrier(style, format) {
            Some(MetaCarrier::Fenced(target)) => target,
            // The only `(style, format)` with no fenced archetype is
            // `delimited` + fig — the fig dialect has no `---`-style delimiter. A
            // real "impossible as asked" (reached converting *to* fig from a
            // delimited block, or *to* delimited from a fig one), so it errors
            // rather than skipping, aborting any recursive sweep.
            _ => {
                let fmt = crate::config::metadata_format_str(format);
                return Err(Error::Structure(format!(
                    "{}: a {} block cannot carry {fmt} — {fmt} has no delimiter syntax; \
                     use a code_block or HTML embedding",
                    path.display(),
                    style.as_config_str(),
                )));
            }
        };
        // Re-housed in the document's own text, not rebuilt around `doc.body`.
        // The body is the two host sides concatenated, so rebuilding from it put
        // an HTML island's `<head>` *below* the island it used to sit above.
        Ok(Some(prov_store::edit::retype_block(
            &text, kind, mapping, target,
        )?))
    }

    /// Restyle every path link the document at `path` declares — frontmatter
    /// relation entries then body links — returning its new text, or `None` when
    /// nothing changed (so a no-op restyle stages no write). The body is spliced
    /// against `doc.body` (verbatim, MetaEditor-preserved) after the frontmatter
    /// edit, the same two-step `rename` uses.
    async fn restyle_document(
        &self,
        path: &Path,
        style: prov_graph::link::LinkStyle,
    ) -> Result<Option<String>> {
        let (text, doc) = self.load(path).await?;
        let meta_rewritten =
            restyle_frontmatter_links(&text, &doc, self.relations().relations(), path, style)?;
        let final_text = restyle_body_links(&meta_rewritten, &doc.body, path, style);
        Ok((final_text != text).then_some(final_text))
    }
}

/// Restyle every path-form frontmatter link `doc` declares into `style`,
/// keeping its resolved destination, label, and wrapper — the frontmatter half
/// of [`convert_link_style`](Workspace::convert_link_style). The sibling of
/// [`rename`](super::rename)'s `rerelativize`, but where that recomputes a
/// *relative* target for a move,
/// this re-spells a stationary target in a chosen [`LinkStyle`]. Id-form,
/// external, and nominal (alias) targets are skipped — `style` describes only
/// how a path is written.
fn restyle_frontmatter_links(
    text: &str,
    doc: &Document,
    relations: &[prov_graph::relation::Relation],
    file: &Path,
    style: prov_graph::link::LinkStyle,
) -> Result<String> {
    let Some(carrier) = doc.carrier else {
        return Ok(text.to_string()); // no metadata: nothing to restyle
    };
    let mut editor = MetaEditor::open(text, carrier)?;
    let restyle = |raw: &str| -> Option<String> {
        let link = Link::parse(raw);
        if !link.is_path_target() || prov_graph::title::is_alias_shaped(&link.target) {
            return None;
        }
        let resolved = link::resolve(file, &link.target);
        Some(
            link.with_path(link::path_text(style, file, &resolved))
                .render(),
        )
    };
    for relation in relations {
        let Some(value) = doc.meta.get(&relation.name) else {
            continue;
        };
        match value {
            Value::String(raw) => {
                if let Some(updated) = restyle(raw) {
                    editor
                        .replace_value(&[Segment::Key(&relation.name)], fig::Value::Str(updated))?;
                }
            }
            Value::Sequence(items) => {
                for (i, item) in items.iter().enumerate() {
                    if let Some(raw) = item.as_str()
                        && let Some(updated) = restyle(raw)
                    {
                        editor.replace_value(
                            &[Segment::Key(&relation.name), Segment::Index(i)],
                            fig::Value::Str(updated),
                        )?;
                    }
                }
            }
            _ => {}
        }
    }
    editor.render()
}

/// Restyle the path-form body links in `body` into `style`, splicing the result
/// back into `text` — the body half of
/// [`convert_link_style`](Workspace::convert_link_style), covering `[[…]]` and
/// markdown/djot `[t](a)` links alike. Id-form, external, and alias targets are
/// left alone. Returns `text` unchanged when nothing restyled.
fn restyle_body_links(
    text: &str,
    body: &str,
    file: &Path,
    style: prov_graph::link::LinkStyle,
) -> String {
    if body.is_empty() {
        return text.to_string();
    }
    let mut new_body = String::with_capacity(body.len());
    let mut cursor = 0;
    let mut rewrote = false;
    for bl in link::scan_body_links(file, body) {
        if !bl.is_path_target() || prov_graph::title::is_alias_shaped(&bl.link.target) {
            continue;
        }
        let resolved = link::resolve(file, &bl.link.target);
        let retargeted = bl
            .link
            .with_path(link::path_text(style, file, &resolved))
            .render();
        new_body.push_str(&body[cursor..bl.span.start]);
        new_body.push_str(&retargeted);
        cursor = bl.span.end;
        rewrote = true;
    }
    if !rewrote {
        return text.to_string();
    }
    new_body.push_str(&body[cursor..]);
    splice_body(text, body, &new_body)
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use prov_graph::link::LinkStyle;

    #[test]
    fn convert_restyles_one_files_links_leaving_the_rest_alone() {
        // Per-file (DESIGN §8): converting mid.md restyles the links *it*
        // declares — its `part_of` up and a body link — into plain_relative,
        // preserving each destination and label. The root's inbound link to
        // mid.md is untouched (that's the root's to convert), so the workspace
        // sits in a valid mixed style.
        let dir = tempdir("convert-linkstyle");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- '[Mid](/sub/mid.md)'\n---\n",
        );
        write(
            &dir,
            "sub/mid.md",
            "---\ntitle: Mid\npart_of: /index.md\n---\nSee [the leaf](/sub/leaf.md).\n",
        );
        write(
            &dir,
            "sub/leaf.md",
            "---\ntitle: Leaf\npart_of: /sub/mid.md\n---\n",
        );

        let n = block_on(ws(&dir).convert_link_style(
            Path::new("sub/mid.md"),
            LinkStyle::PlainRelative,
            false,
        ))
        .unwrap();
        assert_eq!(n.len(), 1, "only the one file converted");

        let mid = read(&dir, "sub/mid.md");
        // Up-link and body link now relative (destinations preserved, label kept).
        assert!(mid.contains("part_of: ../index.md"), "{mid}");
        assert!(mid.contains("[the leaf](leaf.md)"), "{mid}");
        // The root's inbound link stays in its original root style — not this
        // file's to convert.
        assert!(
            read(&dir, "index.md").contains("[Mid](/sub/mid.md)"),
            "inbound untouched"
        );
        // And the mixed-style workspace still validates.
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn convert_recursive_covers_the_spanning_subtree_and_spares_id_and_external() {
        // `-r` converts the file and its descendants. An `id:` link and an
        // external URL are left exactly as written — link_format spells only
        // *path* targets.
        let dir = tempdir("convert-recursive");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n---\n",
        );
        write(
            &dir,
            "a.md",
            "---\ntitle: A\npart_of: index.md\ncontents:\n- sub/b.md\n---\n\
             See [ext](https://example.com) and [[id:ajp7eqb|pinned]].\n",
        );
        write(&dir, "sub/b.md", "---\ntitle: B\npart_of: ../a.md\n---\n");

        let n = block_on(ws(&dir).convert_link_style(
            Path::new("index.md"),
            LinkStyle::MarkdownRoot,
            true,
        ))
        .unwrap();
        assert_eq!(n.len(), 3, "root + a + b all converted");

        let a = read(&dir, "a.md");
        // Path links became root-absolute.
        assert!(a.contains("part_of: /index.md"), "{a}");
        assert!(a.contains("- /sub/b.md"), "{a}");
        // External and id targets untouched.
        assert!(a.contains("[ext](https://example.com)"), "{a}");
        assert!(a.contains("[[id:ajp7eqb|pinned]]"), "{a}");
        assert!(
            read(&dir, "sub/b.md").contains("part_of: /a.md"),
            "descendant converted"
        );
        // (No `check` here: `ajp7eqb` is a deliberately fake id, which `check`
        // would flag as malformed regardless of the conversion. The first
        // convert test validates a clean workspace after converting.)
    }

    #[cfg(feature = "json")]
    #[test]
    fn convert_meta_format_reserializes_the_block_keeping_values_and_body() {
        // A delimited YAML block becomes a delimited JSON block (`;;;`): every
        // value and the prose body survive, only the frontmatter language changes,
        // and the workspace still validates.
        let dir = tempdir("convert-meta-json");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- '[Leaf](/leaf.md)'\n---\n# Root\n\nprose\n",
        );
        write(
            &dir,
            "leaf.md",
            "---\ntitle: Leaf\npart_of: /index.md\n---\n",
        );

        let n =
            block_on(ws(&dir).convert_meta_format(Path::new("index.md"), fig::Format::Json, false))
                .unwrap();
        assert_eq!(n.len(), 1, "only the named file converted");

        let out = read(&dir, "index.md");
        assert!(out.starts_with(";;;\n"), "delimited JSON now: {out}");
        assert!(out.contains("\"title\": \"Root\""), "{out}");
        assert!(
            out.contains("[Leaf](/leaf.md)"),
            "link value preserved: {out}"
        );
        assert!(out.ends_with("# Root\n\nprose\n"), "body untouched: {out}");
        // Per-file: the leaf stays YAML, and the mixed-format workspace is clean.
        assert!(read(&dir, "leaf.md").starts_with("---\n"), "leaf untouched");
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    // fig has no `---`-style delimiter, so a *delimited* block cannot become fig,
    // but a *code-block* one can (```` ```fig ````): the embedding shape is kept.
    #[cfg(feature = "fig-lang")]
    #[test]
    fn convert_meta_format_keeps_the_embedding_shape_and_rejects_impossible_pairs() {
        let dir = tempdir("convert-meta-fig");
        // A code-block YAML document converts cleanly to a ```` ```fig ```` block.
        write(&dir, "code.md", "```yaml\ntitle: Root\n```\nbody\n");
        let n =
            block_on(ws(&dir).convert_meta_format(Path::new("code.md"), fig::Format::Fig, false))
                .unwrap();
        assert_eq!(n.len(), 1);
        let code = read(&dir, "code.md");
        assert!(code.starts_with("```fig\n"), "code block kept: {code}");
        assert!(code.contains("title = Root"), "fig dialect: {code}");
        assert!(code.ends_with("body\n"), "body untouched: {code}");

        // A delimited (`---`) block cannot carry fig — a hard error, not a silent skip.
        write(&dir, "delim.md", "---\ntitle: Root\n---\nbody\n");
        let err =
            block_on(ws(&dir).convert_meta_format(Path::new("delim.md"), fig::Format::Fig, false))
                .unwrap_err();
        assert!(
            err.to_string().contains("cannot carry fig"),
            "clear diagnostic: {err}"
        );
    }

    // Sequence layout is the trap here: fig's per-key Embed splice renders a
    // single-element sequence as a broken inline `* item`, so the reconstruction
    // must go through the canonical serializer. Needs fig on top of the yaml gate.
    #[cfg(feature = "fig-lang")]
    #[test]
    fn convert_meta_format_renders_sequences_the_canonical_way() {
        let dir = tempdir("convert-meta-seq");
        // A *single-element* `contents` is the case that exposed the splice bug.
        write(
            &dir,
            "index.md",
            "```yaml\ntitle: Root\ncontents:\n- '[Leaf](/leaf.md)'\n```\n# Root\n",
        );
        write(
            &dir,
            "leaf.md",
            "```yaml\ntitle: Leaf\npart_of: /index.md\n```\n",
        );

        block_on(ws(&dir).convert_meta_format(Path::new("index.md"), fig::Format::Fig, false))
            .unwrap();
        let out = read(&dir, "index.md");
        // The link survives as a real sequence element, not fused into a scalar.
        assert!(
            out.contains("[Leaf](/leaf.md)") && !out.contains("= * ["),
            "sequence stays well-formed: {out}"
        );
        // The proof it is well-formed: the workspace re-parses and validates.
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[cfg(feature = "fig-lang")]
    #[test]
    fn convert_meta_embed_reshapes_the_block_and_unblocks_fig() {
        // The motivating flow: a `delimited` block cannot hold fig, but re-embedding
        // it as a `code_block` (language kept) can then be converted to fig.
        let dir = tempdir("convert-meta-embed");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- '[Leaf](/leaf.md)'\n---\n# Root\n",
        );
        write(
            &dir,
            "leaf.md",
            "---\ntitle: Leaf\npart_of: /index.md\n---\n",
        );

        // delimited → code_block keeps the YAML language, only the shape changes.
        let n = block_on(ws(&dir).convert_meta_embed(
            Path::new("index.md"),
            EmbedStyle::CodeBlock,
            false,
        ))
        .unwrap();
        assert_eq!(n.len(), 1);
        let code = read(&dir, "index.md");
        assert!(code.starts_with("```yaml\n"), "now a code block: {code}");
        assert!(code.ends_with("# Root\n"), "body untouched: {code}");
        // …which the format axis can now carry to fig.
        block_on(ws(&dir).convert_meta_format(Path::new("index.md"), fig::Format::Fig, false))
            .unwrap();
        assert!(read(&dir, "index.md").starts_with("```fig\n"));
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);

        // `separate` is a whole-file move, out of scope — a clear refusal.
        let err = block_on(ws(&dir).convert_meta_embed(
            Path::new("leaf.md"),
            EmbedStyle::Separate,
            false,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("separate"), "{err}");
    }

    #[cfg(feature = "json")]
    #[test]
    fn convert_meta_format_recursive_skips_no_ops_and_out_of_scope_documents() {
        // A recursive convert sweeps the spanning subtree. A document already in
        // the target format is a no-op; a whole-file (config) document is not a
        // fenced block and is skipped when merely swept — while naming one directly
        // is an error.
        let dir = tempdir("convert-meta-recursive");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n---\n",
        );
        // `a.md` is already JSON, so the sweep leaves it untouched (a no-op).
        write(
            &dir,
            "a.md",
            ";;;\n{\"title\": \"A\", \"part_of\": \"index.md\"}\n;;;\n",
        );

        let n =
            block_on(ws(&dir).convert_meta_format(Path::new("index.md"), fig::Format::Json, true))
                .unwrap();
        assert_eq!(
            n.len(),
            1,
            "only the root actually changed (a.md was already JSON)"
        );
        assert!(read(&dir, "index.md").starts_with(";;;\n"));

        // Naming a whole-file config document directly is refused.
        write(&dir, "conf.yaml", "title: Config\n");
        let err = block_on(ws(&dir).convert_meta_format(
            Path::new("conf.yaml"),
            fig::Format::Json,
            false,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("whole-file"), "{err}");
    }

    // ---- content_format: the axis that moves the document ----

    #[test]
    fn convert_content_format_transcodes_the_body_and_moves_the_file() {
        // The prose is re-serialized as Djot, the file takes the `.dj` extension
        // its new grammar declares, and the metadata block above it is untouched.
        // The parent's inbound link follows the rename — which is what separates
        // this axis from the other three, none of which move anything.
        let dir = tempdir("convert-content-basic");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- '[A](a.md)'\n- b.md\n---\n",
        );
        write(
            &dir,
            "a.md",
            "---\ntitle: A\npart_of: index.md\n---\n\
             Setext\n======\n\n*emph* and [[b]] and `code`.\n",
        );
        // `b` stays Markdown — a wikilink to it must survive the transcode *and*
        // still resolve, which is the pairing that makes a mixed-grammar
        // workspace usable rather than merely permitted.
        write(&dir, "b.md", "---\ntitle: B\npart_of: index.md\n---\n");

        let n = block_on(ws(&dir).convert_content_format(
            Path::new("a.md"),
            ContentFormat::Djot,
            false,
            false,
        ))
        .unwrap();
        assert_eq!(n, vec![PathBuf::from("a.dj")], "reports the *new* path");
        assert!(!dir.join("a.md").exists(), "the old file is gone");

        let a = read(&dir, "a.dj");
        // Frontmatter preserved byte-for-byte; prose re-spelled in Djot.
        assert!(
            a.starts_with("---\ntitle: A\npart_of: index.md\n---\n"),
            "{a}"
        );
        assert!(a.contains("# Setext"), "setext heading became ATX: {a}");
        assert!(a.contains("_emph_"), "emphasis re-spelled for djot: {a}");
        // A wikilink is prov's own notation, not twig's — it must come through
        // verbatim or every body link in the workspace would break on convert.
        assert!(a.contains("[[b]]"), "wikilink survived: {a}");
        assert!(a.contains("`code`"), "code span survived: {a}");

        // The inbound link followed the move, label kept. It is re-spelled in the
        // workspace's configured path style (root, here) rather than its original
        // relative spelling — the same thing `rename` does to a link it retargets.
        assert!(
            read(&dir, "index.md").contains("[A](/a.dj)"),
            "inbound retargeted"
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn convert_content_format_recursive_keeps_links_between_two_movers() {
        // The case one-move-at-a-time gets wrong: `a` and `b` both move, and `a`
        // links to `b`. Each rewrite has to see the other, or whichever is staged
        // last silently drops the first one's edit and `a` is left pointing at a
        // path the sweep just emptied.
        let dir = tempdir("convert-content-recursive");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n- b.md\n---\n",
        );
        write(
            &dir,
            "a.md",
            "---\ntitle: A\npart_of: index.md\nlinks:\n- b.md\n---\nSee [B](b.md).\n",
        );
        write(
            &dir,
            "b.md",
            "---\ntitle: B\npart_of: index.md\n---\nBack to [A](a.md).\n",
        );

        let n = block_on(ws(&dir).convert_content_format(
            Path::new("index.md"),
            ContentFormat::Djot,
            true,
            false,
        ))
        .unwrap();
        assert_eq!(n.len(), 3, "root and both children moved");

        // Retargeted links are re-spelled in the workspace's path style (root).
        let a = read(&dir, "a.dj");
        assert!(
            a.contains("- /b.dj"),
            "frontmatter link to a fellow mover: {a}"
        );
        assert!(a.contains("[B](/b.dj)"), "body link to a fellow mover: {a}");
        assert!(read(&dir, "b.dj").contains("[A](/a.dj)"), "and back again");
        let root = read(&dir, "index.dj");
        assert!(
            root.contains("- /a.dj") && root.contains("- /b.dj"),
            "{root}"
        );
        assert_eq!(block_on(ws(&dir).check("index.dj")).unwrap(), vec![]);
    }

    #[test]
    fn convert_content_format_gates_html_and_no_ops_on_the_current_grammar() {
        // HTML is the lossy endpoint: refused without force, allowed with it. A
        // document already in the target grammar converts nothing at all.
        let dir = tempdir("convert-content-html");
        write(&dir, "index.md", "---\ntitle: Root\n---\n# Hi\n");

        let err = block_on(ws(&dir).convert_content_format(
            Path::new("index.md"),
            ContentFormat::Html,
            false,
            false,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("lossy"), "{err}");
        assert!(dir.join("index.md").exists(), "refused, so nothing moved");

        // Already Markdown: nothing to do, and no rename either.
        let n = block_on(ws(&dir).convert_content_format(
            Path::new("index.md"),
            ContentFormat::Markdown,
            false,
            false,
        ))
        .unwrap();
        assert!(n.is_empty(), "no-op");
        assert!(dir.join("index.md").exists());

        // With force, the same conversion goes through.
        let n = block_on(ws(&dir).convert_content_format(
            Path::new("index.md"),
            ContentFormat::Html,
            false,
            true,
        ))
        .unwrap();
        assert_eq!(n, vec![PathBuf::from("index.html")]);
        assert!(read(&dir, "index.html").contains("<h1>Hi</h1>"));
    }

    #[test]
    fn convert_content_format_moves_a_separated_body_and_repoints_its_node() {
        // A separated pair's grammar belongs to the *body*, not the node: the
        // node's own extension names a metadata format. So the body file moves and
        // the `content` pointer follows it, while the node stays where it is.
        let dir = tempdir("convert-content-separated");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- notes.yaml\n---\n",
        );
        write(
            &dir,
            "notes.yaml",
            "title: Notes\npart_of: index.md\ncontent: notes.md\n",
        );
        write(&dir, "notes.md", "*emph* prose.\n");

        let n = block_on(ws(&dir).convert_content_format(
            Path::new("notes.yaml"),
            ContentFormat::Djot,
            false,
            false,
        ))
        .unwrap();
        assert_eq!(n, vec![PathBuf::from("notes.dj")], "the body moved");

        assert!(dir.join("notes.yaml").exists(), "the node stayed put");
        assert!(!dir.join("notes.md").exists());
        assert!(read(&dir, "notes.dj").contains("_emph_"), "body transcoded");
        assert!(
            read(&dir, "notes.yaml").contains("content: notes.dj"),
            "the node's pointer followed its body"
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn convert_content_format_refuses_a_document_with_no_prose_and_skips_it_in_a_sweep() {
        // A config document has no body and an attachment's payload is opaque
        // bytes. Naming either is an error; meeting one mid-sweep is a skip.
        let dir = tempdir("convert-content-noprose");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- conf.yaml\n---\n# Hi\n",
        );
        write(&dir, "conf.yaml", "title: Config\npart_of: index.md\n");

        let err = block_on(ws(&dir).convert_content_format(
            Path::new("conf.yaml"),
            ContentFormat::Djot,
            false,
            false,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("nothing to convert"), "{err}");

        // Swept rather than named: the root converts, the config is passed over.
        let n = block_on(ws(&dir).convert_content_format(
            Path::new("index.md"),
            ContentFormat::Djot,
            true,
            false,
        ))
        .unwrap();
        assert_eq!(n, vec![PathBuf::from("index.dj")]);
        assert!(
            read(&dir, "conf.yaml").starts_with("title: Config"),
            "untouched"
        );
        assert!(
            read(&dir, "index.dj").contains("conf.yaml"),
            "the config link is unaffected by the root's own move"
        );
    }

    #[test]
    fn convert_content_format_refuses_an_occupied_destination_and_converts_nothing() {
        // The one failure staging cannot undo is an overwrite — the clobbered
        // bytes are gone before rollback could have copied them. So a collision is
        // a guard *before* the sweep stages anything, and the whole conversion is
        // refused rather than half-applied.
        let dir = tempdir("convert-content-collision");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n---\n",
        );
        write(
            &dir,
            "a.md",
            "---\ntitle: A\npart_of: index.md\n---\nprose\n",
        );
        write(&dir, "a.dj", "an unrelated file already sitting there\n");

        let before = snapshot(&dir);
        let err = block_on(ws(&dir).convert_content_format(
            Path::new("index.md"),
            ContentFormat::Djot,
            true,
            false,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("a.dj"), "{err}");
        // Not even the root — which had no collision of its own — converted.
        assert_eq!(snapshot(&dir), before, "the workspace is exactly as it was");
    }

    #[test]
    fn convert_content_format_sweeps_a_separated_node_that_also_links_at_a_mover() {
        // Two edits land on the same node in one sweep and neither may lose: its
        // `content` follows its body to `.dj`, *and* its `part_of` follows the root
        // to `.dj`. They are computed by different halves of the engine — the
        // inbound census and the pointer repoint — so a node reached by both has to
        // come out carrying both.
        let dir = tempdir("convert-content-sep-sweep");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- notes.yaml\n---\n# Root\n",
        );
        write(
            &dir,
            "notes.yaml",
            "title: Notes\npart_of: index.md\ncontent: notes.md\n",
        );
        write(&dir, "notes.md", "*emph* prose.\n");

        let n = block_on(ws(&dir).convert_content_format(
            Path::new("index.md"),
            ContentFormat::Djot,
            true,
            false,
        ))
        .unwrap();
        assert_eq!(n.len(), 2, "the root and the separated body both moved");

        let node = read(&dir, "notes.yaml");
        assert!(
            node.contains("content: notes.dj"),
            "the content pointer followed the body: {node}"
        );
        assert!(
            node.contains("part_of: /index.dj"),
            "and the up-link followed the root: {node}"
        );
        assert!(read(&dir, "notes.dj").contains("_emph_"));
        assert_eq!(block_on(ws(&dir).check("index.dj")).unwrap(), vec![]);
    }

    #[test]
    fn convert_content_format_refuses_two_movers_landing_on_one_name() {
        // A grammar spells more than one extension, so `a.md` and `a.markdown` both
        // want to become `a.dj`. Neither destination exists yet, so only a check
        // across the *plan* catches it — staged, it would be two renames onto one
        // path, the second destroying the first.
        let dir = tempdir("convert-content-two-onto-one");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n- a.markdown\n---\n",
        );
        write(&dir, "a.md", "---\ntitle: A\npart_of: index.md\n---\none\n");
        write(
            &dir,
            "a.markdown",
            "---\ntitle: A2\npart_of: index.md\n---\ntwo\n",
        );

        let before = snapshot(&dir);
        let err = block_on(ws(&dir).convert_content_format(
            Path::new("index.md"),
            ContentFormat::Djot,
            true,
            false,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("both become"), "{err}");
        assert_eq!(snapshot(&dir), before, "nothing converted");
    }

    #[test]
    fn convert_content_format_retargets_links_to_a_parentless_document() {
        // The about page's shape — reached by the root's `about` pointer, in no
        // spanning tree. Converting it moves it, so the pointer has to follow;
        // `spanning_root` yielding the workspace root rather than the file itself
        // is what puts the root in the census that finds it.
        let dir = tempdir("convert-content-parentless");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nabout: about.md\n---\nSee [the page](about.md).\n",
        );
        write(&dir, "about.md", "---\ntitle: About\n---\n*emph* prose.\n");

        let n = block_on(ws(&dir).convert_content_format(
            Path::new("about.md"),
            ContentFormat::Djot,
            false,
            false,
        ))
        .unwrap();
        assert_eq!(n, vec![PathBuf::from("about.dj")]);

        let root = read(&dir, "index.md");
        assert!(
            root.contains("about: /about.dj"),
            "pointer followed: {root}"
        );
        assert!(
            root.contains("[the page](/about.dj)"),
            "body link followed: {root}"
        );
        assert!(read(&dir, "about.dj").contains("_emph_"));
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn convert_content_format_carries_the_registry_so_id_links_keep_resolving() {
        // The move is a rename, so the registry has to follow it — a `colophon:<id>`
        // reference is deliberately *not* rewritten (that is the point of linking
        // by id), and only the id → path update keeps it resolving.
        let dir = tempdir("convert-content-registry");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n---\n",
        );
        write(
            &dir,
            "a.md",
            "---\ntitle: A\npart_of: index.md\n---\nprose\n",
        );

        use prov_graph::index::IdIndex;
        let mut w = hosted_registry_ws(&dir, StdFs);
        let id = block_on(w.register(Path::new("a.md"), crate::identity::Trigger::Link)).unwrap();
        block_on(w.convert_content_format(Path::new("a.md"), ContentFormat::Djot, false, false))
            .unwrap();
        assert_eq!(
            w.index().resolve(&id).as_deref(),
            Some(Path::new("a.dj")),
            "the id followed its document"
        );
    }
}
