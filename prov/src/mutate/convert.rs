//! `convert` — the re-spellings that move no document.
//!
//! Three axes, one discipline: a link's [`LinkStyle`](crate::link::LinkStyle),
//! the metadata block's frontmatter *language*, and its *embedding shape*. Each
//! is per-file by default (DESIGN §8) — how a document spells its own links and
//! metadata is its own to declare, so a workspace may sit in a mixed style and
//! still be `check`-clean — with `recursive` sweeping the spanning subtree as
//! one change set, so a failure two thirds of the way down converts nothing.

use std::path::{Path, PathBuf};

use fig::Segment;

use crate::document::{Document, EmbedStyle, MetaCarrier};
use crate::edit::MetaEditor;
use crate::error::{Error, Result};
use crate::fs::Storage;
use crate::identity::IdentityPolicy;
use crate::index::IndexStore;
use crate::link::{self, Link};
use crate::meta::Value;
use crate::workspace::Workspace;

use super::maintain::splice_body;

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
    /// [`LinkStyle`](crate::link::LinkStyle) (root `/a`, relative `../a`, or bare
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
        style: crate::link::LinkStyle,
        recursive: bool,
    ) -> Result<Vec<PathBuf>> {
        let file = link::normalize(file);
        if !self.fs().try_exists(&self.root().join(&file)).await? {
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
        let file = link::normalize(file);
        if !self.fs().try_exists(&self.root().join(&file)).await? {
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
    /// [`EmbedType`](crate::document::EmbedType) from the document's `(style, format)`
    /// pair with one coordinate replaced, then re-emit the block in it. `Format`
    /// replaces the format and keeps the current style; `Embed` replaces the style
    /// and keeps the current format.
    async fn reformat_document(
        &self,
        path: &Path,
        axis: ReformatAxis,
        named: bool,
    ) -> Result<Option<String>> {
        let (_, doc) = self.load(path).await?;
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
                (crate::document::embed_style_of(kind), format)
            }
            ReformatAxis::Embed(style) => {
                if crate::document::embed_style_of(kind) == style {
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
        let target = match crate::document::embed_carrier(style, format) {
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
        Ok(Some(crate::edit::reformat_block(
            &doc.body, mapping, target,
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
        style: crate::link::LinkStyle,
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
    relations: &[crate::relation::Relation],
    file: &Path,
    style: crate::link::LinkStyle,
) -> Result<String> {
    let Some(carrier) = doc.carrier else {
        return Ok(text.to_string()); // no metadata: nothing to restyle
    };
    let mut editor = MetaEditor::open(text, carrier)?;
    let restyle = |raw: &str| -> Option<String> {
        let link = Link::parse(raw);
        if !link.is_path_target() || crate::title::is_alias_shaped(&link.target) {
            return None;
        }
        let resolved = link::resolve(file, &link.target);
        Some(
            link.with_target(link::path_text(style, file, &resolved))
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
    style: crate::link::LinkStyle,
) -> String {
    if body.is_empty() {
        return text.to_string();
    }
    let mut new_body = String::with_capacity(body.len());
    let mut cursor = 0;
    let mut rewrote = false;
    for bl in link::scan_body_links(file, body) {
        if !bl.is_path_target() || crate::title::is_alias_shaped(&bl.link.target) {
            continue;
        }
        let resolved = link::resolve(file, &bl.link.target);
        let retargeted = bl
            .link
            .with_target(link::path_text(style, file, &resolved))
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
    use crate::link::LinkStyle;

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
}
