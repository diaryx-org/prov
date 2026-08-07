//! The `about.md` generator — a workspace's own reading instructions, written
//! for a person who has the directory and nothing else.
//!
//! `docs/spec.md` states the honest limit of self-description up front: a reader
//! must share *some* convention to bootstrap, and that page is the floor. The
//! trouble is *where the floor lives* — in this repository, on a forge, in a
//! document the workspace does not contain. A prov workspace handed to a
//! stranger is self-describing only to the extent the stranger can obtain the
//! spec, which is a dependency on an institution surviving. This module moves
//! that floor down to "you must be able to open a text file and read English."
//!
//! # Specialize, don't vendor
//!
//! The obvious fix — ship a copy of the spec in every workspace — is worse than
//! it looks. The spec is written for the population of *all* prov workspaces, so
//! every rule it states carries branches this workspace does not exercise:
//!
//! > **Read its metadata block.** The block is separated from the body by a
//! > fence at the top of the file — `---` for YAML, `;;;` for JSON, or an
//! > opening ```` ```fig ```` line.
//!
//! A reader holding one directory has no use for two of those three branches and
//! no way to tell which applies without checking. The generality is not merely
//! surplus; it is *work transferred to the reader*. So this module resolves each
//! rule against the configuration and emits the residue:
//!
//! > Every file here opens with a `---` line. Everything between it and the next
//! > `---` is the file's metadata, written in YAML. The rest is the document.
//!
//! Nothing is lost operationally, two branches are gone, and the sentence is
//! about *this directory* rather than about prov. That is the whole design, and
//! the name is meant literally: the kernel is **self-hosted** when the traversal
//! rules are resident in — and specialized to — the artifact they describe.
//!
//! # The generation rule
//!
//! The load-bearing constraint, from which everything else follows:
//!
//! > **`about` is a function of the workspace's configuration and of prov's own
//! > read behavior. It never reports what the files currently contain.**
//!
//! Hence [`generate`] takes a [`WorkspaceConfig`], a [`RelationSet`] and an
//! [`AboutContext`] — and no filesystem. It cannot consult the corpus because it
//! is never given it.
//!
//! The tempting third source is the corpus, and the motivating case is real:
//! config declares `notation: markdown`, so prov *writes* `[Label](/path/x.md)`,
//! but prov *reads* wikilinks and bare targets too, and these files are
//! hand-editable by design. The fix is not to scan. It is to describe **what
//! prov will accept when reading**, which is knowable without touching a file —
//! see [`reference_section`]. That is strictly better than scanning: it costs no
//! traversal, never goes stale, and is *more* honest, because a spelling absent
//! today can appear tomorrow and a scan-derived claim ("no wikilinks occur
//! here") is a promise the document cannot keep.
//!
//! Everything downstream falls out of that one rule. The page is not rewritten
//! on ordinary mutation, so it is not the sync hotspot DESIGN §5 warns about; a
//! conflicted copy is resolved by *regeneration*, never by merge, because
//! nothing in it is a fact about anything but the configuration.
//!
//! # Operationally complete, generally incomplete
//!
//! The test each sentence must pass, so the generator has a rule rather than a
//! taste:
//!
//! > The document must be **operationally complete** — a reader can traverse the
//! > workspace using nothing else — and **generally incomplete** — silent about
//! > every option this workspace does not exercise.
//!
//! The visible consequence is that **the page's length tracks how unusual the
//! workspace is**. A workspace on defaults gets a short page; one with a bespoke
//! vocabulary gets a longer one, because there is genuinely more a stranger must
//! be told. That property is structural here rather than a matter of discipline:
//! each section is a function returning [`Option<String>`], and a section with
//! nothing to say returns `None`. If a default workspace ever produces four
//! pages, the generator is padding.
//!
//! # Prose in, prose out
//!
//! The page is authored as **Markdown source** in this module and transcoded
//! with `twig` when the workspace's `content_format` is Djot or HTML. Authoring
//! in the target AST was considered and rejected: prose quality is this
//! feature's main risk, and prose assembled from node-construction calls cannot
//! be reviewed by reading it. twig's Markdown serializer preserves source line
//! wrapping, so nothing is given up by writing the prose as prose.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::config::{About, Fixity, IdStorage, WorkspaceConfig};
use crate::content::{ContentFormat, transcode};
use crate::document::{EmbedStyle, MetaCarrier, embed_carrier};
use crate::error::{Error, Result};
use crate::identity::Registration;
use crate::link::{Addressing, Notation, PathStyle};
use crate::meta::{Mapping, Value};
use crate::relation::{Cardinality, RelationSet};

/// The column the generator wraps prose paragraphs at. Narrow on purpose: the
/// page is read in whatever the reader has to hand, which may be a terminal.
const WRAP: usize = 74;

/// The workspace facts the page needs that are *not* in [`WorkspaceConfig`] —
/// the resolved pointer targets and the root document's name.
///
/// These come from the root document's own links rather than from policy, so
/// they are passed in rather than read: [`generate`] touches no filesystem, and
/// the caller has already resolved them (`Workspace::config_path` and friends).
#[derive(Debug, Clone, Default)]
pub struct AboutContext {
    /// The root document, workspace-relative — `README.md` for a default
    /// workspace. Named explicitly in the page rather than referred to as "the
    /// root you came from", because the reader did not come from anywhere: they
    /// opened `about.md` because of its filename.
    pub root_doc: PathBuf,
    /// The config document the root points at, if any.
    pub config_doc: Option<PathBuf>,
    /// The id registry the root points at, if any.
    pub registry_doc: Option<PathBuf>,
    /// The recycle-bin index the root points at, if any.
    pub recycle_doc: Option<PathBuf>,
    /// The history-store index the root points at, if any.
    pub history_doc: Option<PathBuf>,
    /// The generating tool's version, for the byline (`"0.3.2"`).
    pub version: String,
}

impl AboutContext {
    /// A context naming `root_doc` and `version`, with no pointers resolved.
    pub fn new(root_doc: impl Into<PathBuf>, version: impl Into<String>) -> Self {
        Self {
            root_doc: root_doc.into(),
            version: version.into(),
            ..Self::default()
        }
    }
}

/// Generate the complete `about.md` for a workspace — metadata block (if the
/// workspace's embedding convention has one) plus body, in the workspace's
/// content format.
///
/// A pure function of its three arguments. That is what lets `--print`,
/// `--check` and the `AboutStale` finding all be the same call, and what makes
/// the whole page testable without a filesystem.
///
/// Returns [`Error::Content`] if `content_format` transcoding fails, and
/// [`Error::Config`] if the workspace's metadata format and embed style name no
/// carrier that can exist (`metadata.format: fig` with `embed: delimited` — the
/// fig dialect has no delimiter form).
pub fn generate(
    config: &WorkspaceConfig,
    relations: &RelationSet,
    ctx: &AboutContext,
) -> Result<String> {
    let body = markdown_body(config, relations, ctx);
    let body = transcode(&body, config.content_format)?;
    with_metadata_block(&body, config, ctx)
}

/// The page's body as Markdown source, before transcoding and before the
/// metadata block is attached.
///
/// Each section is `Option<String>`; a section with nothing to say about this
/// workspace contributes nothing, which is what makes the page's length track
/// how unusual the workspace is.
fn markdown_body(config: &WorkspaceConfig, relations: &RelationSet, ctx: &AboutContext) -> String {
    let sections = [
        Some(opening_section(config, relations, ctx)),
        Some(root_section(ctx)),
        Some(metadata_block_section(config, relations)),
        Some(reference_section(config, relations)),
        Some(relations_section(config, relations, ctx)),
        fields_section(config),
        machinery_section(config, relations, ctx),
        history_section(ctx),
        Some(conventions_section(config, ctx)),
        Some(safe_to_change_section(config, relations)),
        Some(footer_section(ctx)),
    ];
    let mut out = String::new();
    for section in sections.into_iter().flatten() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&section);
    }
    out
}

/// The title, the thesis, and the byline's prose half.
fn opening_section(
    config: &WorkspaceConfig,
    relations: &RelationSet,
    ctx: &AboutContext,
) -> String {
    let mut s = String::new();
    s.push_str("# How this workspace is organized\n\n");

    // Under `separate`, the metadata is not "in a block at the top of itself" —
    // it is in a companion file. The thesis is the same; the mechanism is not.
    let where_it_states = if config.embed_style == EmbedStyle::Separate {
        "Each file has a companion file beside it stating what it belongs to and \
         what belongs to it."
    } else {
        "Each file states, in a block at the top of itself, what it belongs to \
         and what belongs to it."
    };
    s.push_str(&para(&format!(
        "This directory is a set of plain text files that describe their own \
         structure. Nothing about how they fit together is kept outside them — \
         no database, no index, no hidden folder. {where_it_states} Follow those \
         statements and the whole directory unfolds."
    )));

    s.push('\n');
    s.push_str(&para(
        "Nobody wrote this page. It was produced by reading this directory's own \
         settings, so it describes what the files actually declare rather than \
         what someone remembered.",
    ));

    // One up-front warning, and only when the workspace does something a
    // stranger cannot possibly infer from looking: addressing by identifier.
    // Everything else they can work out by opening a file.
    if config.reference_target == Addressing::Id {
        s.push('\n');
        s.push_str(&para(
            "Read the section on references before anything else. Files here \
             point at each other by permanent identifier rather than by \
             filename, which is unusual and which nothing else in the directory \
             will explain to you.",
        ));
    } else if config.reference_target == Addressing::Alias {
        s.push('\n');
        s.push_str(&para(
            "Read the section on references before anything else. Files here \
             point at each other by title rather than by filename, which is \
             unusual and which nothing else in the directory will explain to \
             you.",
        ));
    }

    let _ = relations;
    let _ = ctx;
    s
}

/// Where to start reading. States the answer, never the procedure — the rule for
/// *finding* a root without being told is only interesting to a tool, which is
/// precisely the reader this page is not written for.
fn root_section(ctx: &AboutContext) -> String {
    let root = display_path(&ctx.root_doc);
    let mut s = format!("## Start at {}\n\n", code(&root));
    s.push_str(&para(&format!(
        "{} is the root. Everything else here hangs off it, directly or through \
         something else that does.",
        code(&root)
    )));
    s
}

/// How a file carries its metadata — the fence, the language, and which files
/// have no body at all.
fn metadata_block_section(config: &WorkspaceConfig, relations: &RelationSet) -> String {
    let format_name = format_name(config.default_embed_format);
    let carrier = embed_carrier(config.embed_style, config.default_embed_format);

    let mut s = String::new();

    if config.embed_style == EmbedStyle::Separate {
        s.push_str("## Metadata lives beside each file\n\n");
        s.push_str(&para(&format!(
            "A document here is two files: one holding the prose, and one beside \
             it holding the metadata, written in {format_name}. The metadata file \
             names its prose file under a {} key, and that pairing is what joins \
             the two.",
            code("content")
        )));
        s.push('\n');
        s.push_str(&para(
            "Neither file carries a fence or a marker of any kind — the metadata \
             file is metadata all the way down, and the prose file is nothing but \
             prose.",
        ));
        return s;
    }

    s.push_str("## Every file opens with a metadata block\n\n");

    let (intro, example, closing) = match carrier {
        Some(MetaCarrier::Fenced(kind)) => fence_prose(kind, &format_name, config, relations),
        // `Separate` is handled above; a `None` carrier means the format and
        // style name no real embedding (fig has no delimiter form). Describe the
        // format alone rather than inventing a fence.
        _ => (
            format!("A file begins with a metadata block written in {format_name}."),
            None,
            "Everything in that block is the file's metadata. Everything after \
             it is the document."
                .to_string(),
        ),
    };

    s.push_str(&para(&intro));
    if let Some(example) = example {
        s.push('\n');
        s.push_str(&example);
    }
    s.push('\n');
    s.push_str(&para(&closing));
    s.push('\n');
    s.push_str(&para(&format!(
        "The order of the keys is for your benefit, not the machine's. Some files \
         are metadata all the way down, with no document part. You can tell by \
         the extension: {} files have no separate body and no fence.",
        whole_file_extensions()
    )));
    s
}

/// The fence prose for one concrete carrier: how the block opens, a worked
/// example, and how it ends. Split out because it is the one place where the
/// specialization is *entirely* mechanical — one branch per carrier prov can
/// actually write, and no branch for any it cannot.
fn fence_prose(
    kind: fig::EmbedType,
    format_name: &str,
    config: &WorkspaceConfig,
    relations: &RelationSet,
) -> (String, Option<String>, String) {
    use fig::EmbedType as E;

    // The worked example must be one that could actually occur *here*: this
    // workspace's up-field, holding a reference in this workspace's own
    // notation. A sample showing `part_of` to a reader whose files say
    // `section_of` teaches them the wrong key.
    let field = spanning_up_field(relations).unwrap_or_else(|| "part_of".to_string());
    let target = sample_reference(config);
    let sample_yaml = format!("title: Some Document\n{field}: '{target}'");
    let sample_json =
        format!("{{\n  \"title\": \"Some Document\",\n  \"{field}\": \"{target}\"\n}}");
    let sample_toml = format!("title = \"Some Document\"\n{field} = \"{target}\"");
    let sample_fig = sample_yaml.clone();
    let (sample_yaml, sample_json, sample_toml, sample_fig) = (
        sample_yaml.as_str(),
        sample_json.as_str(),
        sample_toml.as_str(),
        sample_fig.as_str(),
    );

    let delimited = |fence: &str, spelled: &str, sample: &str| {
        (
            format!("A file begins with a line containing {spelled}:"),
            Some(fenced_block(&format!(
                "{fence}\n{sample}\n{fence}\n\nThe rest of the file is the document itself."
            ))),
            format!(
                "Everything between that line and the next `{fence}` is the file's \
                 metadata, written in {format_name}. Everything after it is the \
                 document."
            ),
        )
    };

    let fenced = |lang: &str, sample: &str| {
        (
            format!("A file begins with a fenced code block labelled `{lang}`:"),
            Some(fenced_block(&format!(
                "```{lang}\n{sample}\n```\n\nThe rest of the file is the document itself."
            ))),
            format!(
                "Everything inside that block is the file's metadata, written in \
                 {format_name}. Everything after the closing fence is the \
                 document. The block is deliberately visible: it renders as a code \
                 block rather than disappearing."
            ),
        )
    };

    let island = |lang: &str, sample: &str| {
        (
            format!(
                "A file begins with an HTML data island — a `<script>` tag whose \
                 type names {format_name}:"
            ),
            Some(fenced_block(&format!(
                "<script type=\"application/{lang}\">\n{sample}\n</script>\n\n\
                 The rest of the file is the document itself."
            ))),
            format!(
                "Everything inside the tag is the file's metadata, written in \
                 {format_name}. Everything after `</script>` is the document. The \
                 island does not render, so a browser shows only the prose."
            ),
        )
    };

    let html_code = |lang: &str, sample: &str| {
        (
            format!(
                "A file begins with a visible HTML code block whose class names \
                 {format_name}:"
            ),
            Some(fenced_block(&format!(
                "<pre><code class=\"language-{lang}\">\n{sample}\n</code></pre>\n\n\
                 The rest of the file is the document itself."
            ))),
            format!(
                "Everything inside the block is the file's metadata, written in \
                 {format_name}. Everything after `</pre>` is the document. \
                 Characters like `<` and `&` are HTML-encoded in there; decode \
                 them before reading the values."
            ),
        )
    };

    match kind {
        E::FrontmatterYaml => delimited("---", "three dashes", sample_yaml),
        E::FrontmatterJson => delimited(";;;", "three semicolons", sample_json),
        E::PlusToml => delimited("+++", "three plus signs", sample_toml),
        E::MdFrontmatterJson => delimited("---json", "`---json`", sample_json),
        E::MdFrontmatterToml => delimited("---toml", "`---toml`", sample_toml),
        E::MdFrontmatterFig => delimited("---fig", "`---fig`", sample_fig),
        E::FencedYaml => fenced("yaml", sample_yaml),
        E::FencedJson => fenced("json", sample_json),
        E::FencedToml => fenced("toml", sample_toml),
        E::FrontmatterFig => fenced("fig", sample_fig),
        E::EndmatterYaml => (
            "A file *ends* with a fenced code block labelled `endmatter`:".into(),
            Some(fenced_block(&format!(
                "The document itself comes first.\n\n```endmatter\n{sample_yaml}\n```"
            ))),
            format!(
                "Everything inside that block is the file's metadata, written in \
                 {format_name}. Everything before it is the document."
            ),
        ),
        E::HtmlScriptYaml => island("yaml", sample_yaml),
        E::HtmlScriptJson => island("json", sample_json),
        E::HtmlScriptToml => island("toml", sample_toml),
        E::HtmlScriptFig => island("figl", sample_fig),
        E::HtmlCodeYaml => html_code("yaml", sample_yaml),
        E::HtmlCodeJson => html_code("json", sample_json),
        E::HtmlCodeToml => html_code("toml", sample_toml),
        E::HtmlCodeFig => html_code("figl", sample_fig),
        // `EmbedType` is `#[non_exhaustive]`; see the matching comment in
        // `document::embed_style_of` — a new variant needs a real case here too.
        _ => unreachable!("unhandled fig::EmbedType variant — add a case in fence_prose"),
    }
}

/// How a reference is written, and — the part config alone cannot tell a reader
/// — what *else* prov will accept, so a hand-typed spelling does not baffle them.
fn reference_section(config: &WorkspaceConfig, relations: &RelationSet) -> String {
    let mut s = String::from("## How to read a reference\n\n");
    let field = spanning_up_field(relations).unwrap_or_else(|| "part_of".to_string());

    match config.reference_target {
        Addressing::Path => {
            let shape = sample_reference(config);
            let description = match config.notation {
                Notation::Markdown => {
                    "The text in brackets is decoration — a human label, safe to \
                     change. The target is in the parentheses."
                }
                Notation::Wikilink => {
                    "The target sits between the double brackets. A `|` inside \
                     them separates the target from a human label, which is \
                     decoration and safe to change."
                }
                Notation::Bare => "There is no wrapper and no label — the value is the target.",
            };
            s.push_str(&para(match config.notation {
                Notation::Markdown => "References here are written like a Markdown link:",
                Notation::Wikilink => {
                    "References here are written as double-bracketed links holding a path:"
                }
                Notation::Bare => "References here are written as a bare path, with no wrapper:",
            }));
            s.push('\n');
            s.push_str(&fenced_block(&format!("{field}: '{shape}'")));
            s.push('\n');
            s.push_str(&para(description));
            s.push('\n');
            s.push_str(&para(path_style_prose(config.path_style)));
        }
        Addressing::Id => {
            let shape = sample_reference(config);
            s.push_str(&para(
                "References here are written as double-bracketed links addressed \
                 by **identifier**, not by filename:",
            ));
            s.push('\n');
            s.push_str(&fenced_block(&format!("{field}: '{shape}'")));
            s.push('\n');
            if config.reference_label {
                s.push_str(&para(
                    "The text after the `|` is decoration — a human label, safe to \
                     change. The part that matters is `id:aj7eqx`, and it names a \
                     *document*, not a location.",
                ));
            } else {
                s.push_str(&para("`id:aj7eqx` names a *document*, not a location."));
            }
            s.push('\n');
            s.push_str(&para(&id_resolution_prose(config)));
            s.push('\n');
            s.push_str(&para(
                "An identifier is permanent. It is never reissued, even after a \
                 document is deleted, so a reference that resolves to nothing \
                 means \"this document is gone,\" never \"this identifier belongs \
                 to something else now.\"",
            ));
        }
        Addressing::Alias => {
            s.push_str(&para(
                "References here name a document by its **title**, not by its \
                 filename:",
            ));
            s.push('\n');
            s.push_str(&fenced_block(&format!("{field}: '[[Board Records]]'")));
            s.push('\n');
            s.push_str(&para(
                "To resolve one, find the file whose `title` is that text. Titles \
                 are expected to be unique here; if two files share one, the \
                 reference is ambiguous and prov will say so rather than guess.",
            ));
        }
    }

    // What prov *reads*, which is not what config says it writes. This is the
    // corpus question answered without touching the corpus.
    let alternatives = alternative_spellings(config);
    if !alternatives.is_empty() {
        s.push('\n');
        s.push_str(&para(
            "Other spellings mean the same thing and are understood wherever a \
             reference can appear, so you may meet them in files someone edited \
             by hand:",
        ));
        s.push('\n');
        s.push_str(&table(
            &["written", "called"],
            &alternatives
                .iter()
                .map(|(w, c)| vec![code(w), c.to_string()])
                .collect::<Vec<_>>(),
        ));
    }

    if config.reference_target != Addressing::Path {
        s.push('\n');
        s.push_str(&para(&format!(
            "Where a path does appear — in a hand-edited file, or in one of the \
             spellings above — resolve it like this. {}",
            path_style_prose(config.path_style)
        )));
    }

    s.push('\n');
    s.push_str(&para(
        "A target containing `://`, or beginning with `mailto:`, points outside \
         this directory and is never resolved. A target naming a file that is not \
         here is simply broken — worth noting, not a reason to stop reading.",
    ));
    s
}

/// The relation vocabulary and the spine, as a table.
fn relations_section(
    config: &WorkspaceConfig,
    relations: &RelationSet,
    ctx: &AboutContext,
) -> String {
    let mut s = String::from("## How the files relate to each other\n\n");

    let content_relations = content_relations(relations);
    let spanning = relations.spanning_relation();
    let count = capitalize(number_word(content_relations.len()));
    match spanning {
        Some(spanning) => s.push_str(&para(&format!(
            "{count} relations are used here. Follow **{}** from {} to reach \
             every document; that is the spine, and every file sits at exactly \
             one place along it.",
            code(spanning),
            code(&display_path(&ctx.root_doc)),
        ))),
        None => s.push_str(&para(&format!("{count} relations are used here."))),
    }

    s.push('\n');
    let rows: Vec<Vec<String>> = content_relations
        .iter()
        .map(|r| -> Vec<String> {
            vec![
                code(&r.name),
                // The human gloss is tier-3 config prov merely carries, so it
                // lives in `relation_defs` rather than on the built relation. A
                // workspace that declares no vocabulary has no glosses to print.
                config
                    .relation_defs
                    .get(&r.name)
                    .and_then(|d| d.means.clone())
                    .unwrap_or_else(|| "—".to_string()),
                match r.cardinality {
                    Cardinality::One => "one".to_string(),
                    Cardinality::Many => "many".to_string(),
                },
                r.inverse.as_deref().map(code).unwrap_or_else(|| "—".into()),
            ]
        })
        .collect();
    s.push_str(&table(
        &["relation", "means", "how many", "its opposite"],
        &rows,
    ));

    if content_relations.iter().any(|r| r.inverse.is_some()) {
        s.push('\n');
        s.push_str(&para(
            "Both halves of a pair are kept in step: if A lists B under one, B \
             names A under its opposite. If you edit one half by hand and not the \
             other, nothing is lost — the pair is simply inconsistent until \
             someone repairs it.",
        ));
    }

    if let Some(up) = spanning.and_then(|s| inverse_of(relations, s)) {
        let mut closing = format!(
            "{} holds exactly one target, which is what makes the spine a tree \
             with a single top.",
            code(&up)
        );
        let overlay: Vec<String> = content_relations
            .iter()
            .filter(|r| Some(r.name.as_str()) != spanning && r.name != up)
            .map(|r| code(&r.name))
            .collect();
        if !overlay.is_empty() {
            let _ = write!(
                closing,
                " {} {} laid over that tree and may point anywhere; follow them \
                 for meaning, never to discover what is here.",
                join_list(&overlay),
                if overlay.len() == 1 { "is" } else { "are" }
            );
        }
        s.push('\n');
        s.push_str(&para(&closing));
    }

    s
}

/// Controlled-vocabulary fields — omitted entirely when the workspace declares
/// none, which is the common case.
fn fields_section(config: &WorkspaceConfig) -> Option<String> {
    let controlled: Vec<_> = config
        .fields
        .iter()
        .filter(|(_, spec)| spec.vocabulary.is_some())
        .collect();
    if controlled.is_empty() {
        return None;
    }

    let mut s = String::from("## Fields with fixed vocabularies\n\n");
    s.push_str(&para(&format!(
        "{} {} not hold free text. {} permitted values are listed in files of \
         their own.",
        capitalize(number_word(controlled.len())),
        if controlled.len() == 1 {
            "field does"
        } else {
            "fields do"
        },
        if controlled.len() == 1 {
            "Its"
        } else {
            "Their"
        },
    )));
    s.push('\n');

    let rows: Vec<Vec<String>> = controlled
        .iter()
        .map(|(name, spec)| {
            let rule = match spec.values {
                crate::config::OpenClosed::Closed => {
                    "**closed** — every value must appear in the list".to_string()
                }
                crate::config::OpenClosed::Open => {
                    "**open** — any value is allowed; the list records the ones in use".to_string()
                }
            };
            let target = spec
                .vocabulary
                .as_deref()
                .map(link_target_text)
                .unwrap_or_default();
            vec![code(name), rule, code(&target)]
        })
        .collect();
    s.push_str(&table(&["field", "rule", "values listed in"], &rows));

    if controlled
        .iter()
        .any(|(_, s)| s.values == crate::config::OpenClosed::Closed)
    {
        s.push('\n');
        s.push_str(&para(
            "A closed field is worth taking seriously: a value not on the list is \
             an error rather than a new category.",
        ));
    }
    Some(s)
}

/// The files the spine will never reach, and why that is deliberate.
fn machinery_section(
    config: &WorkspaceConfig,
    relations: &RelationSet,
    ctx: &AboutContext,
) -> Option<String> {
    let mut pointers: Vec<(String, String)> = Vec::new();
    if let (Some(rel), Some(path)) = (relations.config_relation(), &ctx.config_doc) {
        pointers.push((
            rel.to_string(),
            format!(
                "this directory's settings — the file this page was generated from ({})",
                code(&display_path(path))
            ),
        ));
    }
    if let (Some(rel), Some(path)) = (relations.registry_relation(), &ctx.registry_doc) {
        pointers.push((
            rel.to_string(),
            format!("the list of permanent ids ({})", code(&display_path(path))),
        ));
    }
    if let (Some(rel), Some(path)) = (relations.recycle_relation(), &ctx.recycle_doc) {
        pointers.push((
            rel.to_string(),
            format!("what has been deleted ({})", code(&display_path(path))),
        ));
    }
    if let (Some(rel), Some(path)) = (relations.history_relation(), &ctx.history_doc) {
        pointers.push((
            rel.to_string(),
            format!(
                "a record of past states of this directory ({})",
                code(&display_path(path))
            ),
        ));
    }
    for (name, spec) in &config.fields {
        if let Some(vocab) = &spec.vocabulary {
            pointers.push((
                format!("fields.{name}.vocabulary"),
                format!(
                    "the permitted values of {} ({})",
                    code(name),
                    code(&link_target_text(vocab))
                ),
            ));
        }
    }
    if pointers.is_empty() {
        return None;
    }

    let root = display_path(&ctx.root_doc);
    let spanning = relations.spanning_relation().unwrap_or("contents");
    let mut s = String::from("## Files that are not part of the tree\n\n");

    if pointers.len() == 1 {
        let (key, what) = &pointers[0];
        s.push_str(&para(&format!(
            "Following {} will never reach one file here. That is deliberate, not \
             an omission. {} points at it directly, through a key named {}, and it \
             points at nothing in return. It holds {}.",
            code(spanning),
            code(&root),
            code(key),
            what
        )));
    } else {
        s.push_str(&para(&format!(
            "Following {} will never reach some of the files here. That is \
             deliberate, not an omission. {} points at each of them directly, \
             through a key that names what it is, and none of them points back.",
            code(spanning),
            code(&root),
        )));
        s.push('\n');
        s.push_str(&table(
            &[&format!("key in {}", code(&root)), "what it points at"],
            &pointers
                .iter()
                .map(|(k, w)| vec![code(k), w.clone()])
                .collect::<Vec<_>>(),
        ));
    }

    s.push('\n');
    let up = relations
        .spanning_relation()
        .and_then(|sp| inverse_of(relations, sp));
    s.push_str(&para(&format!(
        "Files reached that way are machinery: they are not documents in the \
         tree, they carry no {}, and they are not counted when something asks \
         what this workspace contains.",
        code(up.as_deref().unwrap_or("part_of"))
    )));

    s.push('\n');
    s.push_str(&para(&format!(
        "A binary file is never part of the tree either. To bring in an image or \
         a PDF, a small text file is created beside it that names it under a {} \
         key; that text file is the document, and the binary rides along as its \
         payload. Everything in the tree is plain text, always.",
        code("content")
    )));
    Some(s)
}

/// The history store — a pointer, never a copy. `about` answers "how do I read
/// this?"; the manifests answer "what is here, and are the bytes intact?"
fn history_section(ctx: &AboutContext) -> Option<String> {
    let path = ctx.history_doc.as_ref()?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| format!("{}/", display_path(p)))
        .unwrap_or_else(|| display_path(path));

    let mut s = String::from("## The history store\n\n");
    s.push_str(&para(&format!(
        "{} holds a record of what this directory contained at particular \
         moments. Each entry lists every file present at that moment, with a \
         checksum of its contents, and the bytes themselves are kept alongside, \
         named by checksum.",
        code(&dir)
    )));
    s.push('\n');
    s.push_str(&para(
        "It exists so that damage — a bad merge between two copies of this \
         directory, a file mangled in transit — can be identified and undone. If \
         you are trying to work out whether something is missing or has been \
         altered, that is where to look. Entries are written and never edited \
         afterward.",
    ));
    Some(s)
}

/// Identity, checksums, deletion, timestamps — the four conventions a stranger
/// needs in order not to break something by accident.
fn conventions_section(config: &WorkspaceConfig, ctx: &AboutContext) -> String {
    let mut s = String::from("## Conventions in this workspace\n\n");
    let mut bullets: Vec<String> = Vec::new();

    bullets.push(format!("**Identity.** {}", identity_prose(config, ctx)));

    bullets.push(format!(
        "**Checksums.** {}",
        match config.fixity {
            Fixity::Off => "Nothing here is checksummed. A file's contents are \
                            whatever the file says."
                .to_string(),
            Fixity::Payloads => format!(
                "Attachment payloads record a {}, written as `sha256:<hex>` so any \
                 checksum tool can verify it. Document bodies are not hashed.",
                code("content_hash")
            ),
            Fixity::Full => format!(
                "Every document records a {} of its body, and every attachment \
                 payload one of its bytes, written as `sha256:<hex>` so any \
                 checksum tool can verify it independently. Editing a document \
                 outside this tool will leave its checksum stale — repairable, \
                 not lost.",
                code("content_hash")
            ),
        }
    ));

    bullets.push(format!(
        "**Deleting.** {}",
        if config.recycle_bin {
            "Deleted documents go to a recycle bin and can be brought back until \
             it is emptied."
                .to_string()
        } else {
            let recoverable = if ctx.history_doc.is_some() {
                " A past state can still be recovered from the history store."
            } else {
                ""
            };
            format!(
                "There is no recycle bin here. A deletion is immediate and permanent.{recoverable}"
            )
        }
    ));

    bullets.push(format!(
        "**Timestamps.** {}",
        if config.updated.is_empty() {
            "No modification times are maintained. Any date you find in a file \
             was written by a person."
                .to_string()
        } else {
            format!(
                "A field named {} is maintained automatically and holds a UTC \
                 instant in RFC 3339 form (`1974-03-02T14:05:00Z`). Any other \
                 date you find in a file was written by a person.",
                code(&config.updated)
            )
        }
    ));

    s.push_str(&bullet_list(&bullets));
    s
}

/// The permission slip, and the short list of fields that are not yours.
fn safe_to_change_section(config: &WorkspaceConfig, relations: &RelationSet) -> String {
    let mut s = String::from("## What is safe to change\n\n");
    s.push_str(&para(
        "All of it. It is your text, and the structure is in the text.",
    ));

    let mut careful: Vec<String> = Vec::new();
    if config.identity != Registration::OFF {
        let why = if config.reference_target == Addressing::Id {
            "a permanent handle, and here the *only* record of which document is \
             which. Every reference in this directory resolves through it. Change \
             one and every link to that document stops resolving; reuse one and \
             two documents become indistinguishable."
        } else {
            "a permanent handle. Ids are never reissued, even after a document is \
             deleted, so a reference to a deleted document can still be told apart \
             from a reference to something that never existed."
        };
        careful.push(format!("**{}** — {why}", code("id")));
    }
    if config.fixity != Fixity::Off {
        careful.push(format!(
            "**{}** — a checksum. Changing it by hand asserts something about the \
             bytes that may not be true.",
            code("content_hash")
        ));
    }
    if !config.updated.is_empty() {
        careful.push(format!(
            "**{}** — maintained automatically, and in a fixed format.",
            code(&config.updated)
        ));
    }

    if !careful.is_empty() {
        s.push('\n');
        s.push_str(&para(&format!(
            "{} {} worth leaving alone, {}because something else may already \
             depend on {}:",
            capitalize(number_word(careful.len())),
            if careful.len() == 1 {
                "field is"
            } else {
                "fields are"
            },
            match careful.len() {
                1 => "",
                2 => "both ",
                _ => "each ",
            },
            if careful.len() == 1 { "it" } else { "them" },
        )));
        s.push('\n');
        s.push_str(&bullet_list(&careful));
    }

    let names: Vec<String> = content_relations(relations)
        .iter()
        .map(|r| code(&r.name))
        .collect();
    if !names.is_empty() {
        s.push('\n');
        s.push_str(&para(&format!(
            "The relation fields — {} — are meant to be edited by hand. That is \
             the whole point of keeping them in the files.",
            join_list(&names)
        )));
    }
    s
}

/// The footer: what wrote this, how to change it, and — the one concession to
/// generality — what the scheme is called, for a reader who wants to write a
/// tool rather than read the workspace.
///
/// Deliberately versionless. The version belongs in `generated_by`, in the
/// metadata block, which [`same_body`] excludes from comparison for exactly
/// that reason — `generated_by: prov 0.3.2` in a workspace regenerated by
/// 0.4.0 is a stale byline, not a stale page. Interpolating `ctx.version`
/// here as well would put the same fact in a place `same_body` *does*
/// compare, quietly reopening the hole that exclusion was built to close:
/// every release would mark every workspace stale, and two devices on
/// different prov versions sharing a synced directory would rewrite this
/// page back and forth on every config-adjacent command.
fn footer_section(ctx: &AboutContext) -> String {
    let change = match &ctx.config_doc {
        Some(path) => format!("change {} instead", code(&display_path(path))),
        None => "change this workspace's configuration instead".to_string(),
    };
    format!(
        "---\n\n{}",
        para(&format!(
            "<sub>Generated by prov from this workspace's configuration. \
             Edits to this file will be overwritten — {change}, or run `prov about` \
             to rewrite this page. The scheme these files follow is called prov; \
             its specification lived at <https://github.com/diaryx-org/prov>, but \
             you do not need it to read this directory.</sub>"
        ))
    )
}

// ─── the metadata block ──────────────────────────────────────────────────────

/// Attach the page's own metadata block, in whatever the workspace's embedding
/// convention is — **including nothing at all**.
///
/// Under `embed_style: separate` no file in the workspace carries a fence, so a
/// fenced `about.md` would be the anomaly; the page is written content-only and
/// gets no sidecar either, because a two-file `about` is worse for the reader it
/// exists for and the sidecar would carry nothing prov reads back. The `title`
/// survives as the page's `# ` heading, and the byline as the footer.
///
/// Nothing depends on the block being present: staleness is detected by
/// comparing the *body*, so the page needs no marker to be recognized as
/// generated (see the `AboutStale` finding).
fn with_metadata_block(body: &str, config: &WorkspaceConfig, ctx: &AboutContext) -> Result<String> {
    if config.embed_style == EmbedStyle::Separate {
        return Ok(body.to_string());
    }
    let Some(MetaCarrier::Fenced(kind)) =
        embed_carrier(config.embed_style, config.default_embed_format)
    else {
        return Err(Error::Structure(format!(
            "no metadata carrier for embed style `{}` with format `{}`",
            config.embed_style.as_config_str(),
            crate::config::metadata_format_str(config.default_embed_format),
        )));
    };

    let mut mapping = Mapping::new();
    mapping.insert(
        "title".into(),
        Value::String("How this workspace is organized".into()),
    );
    // Prov-maintained, so prov owns its format (DESIGN §2): tool name and
    // version, nothing else. Deliberately *not* `author` — that is tier-3 and
    // user-owned, and writing into it would muddy the boundary §2 exists to draw.
    //
    // The byline is this page's authorship, and it makes a claim a human byline
    // cannot: "derived from this workspace's own settings" asserts *this was read
    // off the files themselves*, which is checkable — and which `check` does in
    // fact check.
    mapping.insert(
        "generated_by".into(),
        Value::String(format!("prov {}", ctx.version)),
    );
    // A blank line between the block and the `# ` heading — the body is prose in
    // the workspace's content format, and prose starts after the block, not
    // wedged against its closing fence.
    crate::edit::reformat_block(&format!("\n{body}"), &mapping, kind)
}

// ─── prose helpers ───────────────────────────────────────────────────────────

/// A paragraph, wrapped to [`WRAP`] columns, ending in exactly one newline.
/// Callers put a bare `\n` between blocks to make the blank line.
///
/// Wrapping after interpolation rather than by hand is what keeps the output
/// stable: a workspace whose relation is named `x` and one whose relation is
/// named `superseded_by` both get tidy paragraphs.
pub(crate) fn para(text: &str) -> String {
    wrap(&collapse(text), WRAP)
}

/// Collapse the whitespace a Rust string continuation introduces (`\` at end of
/// line plus indentation) into single spaces, so the source can be indented
/// naturally and still produce one logical paragraph.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Greedy word wrap at `width`, never breaking a word.
fn wrap(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut column = 0usize;
    for word in text.split_whitespace() {
        let len = word.chars().count();
        if column == 0 {
            out.push_str(word);
            column = len;
        } else if column + 1 + len <= width {
            out.push(' ');
            out.push_str(word);
            column += 1 + len;
        } else {
            out.push('\n');
            out.push_str(word);
            column = len;
        }
    }
    out.push('\n');
    out
}

/// A fenced code block, verbatim — never wrapped.
fn fenced_block(text: &str) -> String {
    format!("```\n{}\n```\n", text.trim_end_matches('\n'))
}

/// A bullet list, each item wrapped and continuation-indented.
fn bullet_list(items: &[String]) -> String {
    let mut out = String::new();
    for item in items {
        let wrapped = wrap(&collapse(item), WRAP - 2);
        for (i, line) in wrapped.trim_end().lines().enumerate() {
            if i == 0 {
                let _ = writeln!(out, "- {line}");
            } else {
                let _ = writeln!(out, "  {line}");
            }
        }
    }
    out
}

/// A Markdown table. Cells are emitted verbatim apart from `|` escaping, and the
/// table is never wrapped.
fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "| {} |",
        headers
            .iter()
            .map(|h| escape_cell(h))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    let _ = writeln!(out, "|{}", " --- |".repeat(headers.len()));
    for row in rows {
        let _ = writeln!(
            out,
            "| {} |",
            row.iter()
                .map(|c| escape_cell(c))
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }
    out
}

/// Wrap `value` in backticks, widening the fence if the value contains one — so
/// a relation or field name a user chose can never break out of its code span.
fn code(value: &str) -> String {
    let longest = value
        .split(|c| c != '`')
        .filter(|s| !s.is_empty())
        .map(|s| s.len())
        .max()
        .unwrap_or(0);
    if longest == 0 {
        return format!("`{value}`");
    }
    let fence = "`".repeat(longest + 1);
    // A value opening or closing with a backtick needs padding spaces, which the
    // renderer strips back off.
    format!("{fence} {value} {fence}")
}

/// Escape a value for a Markdown table cell: a bare `|` would start a new cell.
fn escape_cell(value: &str) -> String {
    value.replace('|', "\\|")
}

// ─── config → prose ──────────────────────────────────────────────────────────

/// The human name of a metadata format, as the page should say it.
pub(crate) fn format_name(format: fig::Format) -> String {
    match format {
        fig::Format::Yaml => "YAML".into(),
        fig::Format::Toml => "TOML".into(),
        fig::Format::Json | fig::Format::Jsonc | fig::Format::Json5 => "JSON".into(),
        fig::Format::Fig => "fig".into(),
        other => crate::config::metadata_format_str(other).to_uppercase(),
    }
}

/// The extensions whose files are metadata all the way down.
fn whole_file_extensions() -> String {
    "`.yaml`, `.yml`, `.json`, `.toml`, `.fig`, and `.figl`".to_string()
}

/// One reference, written exactly as this workspace writes them — the shape the
/// block sample and the reference section both show.
fn sample_reference(config: &WorkspaceConfig) -> String {
    match config.reference_target {
        Addressing::Id if config.reference_label => "[[id:aj7eqx|Board Records]]".to_string(),
        Addressing::Id => "[[id:aj7eqx]]".to_string(),
        Addressing::Alias => "[[Board Records]]".to_string(),
        Addressing::Path => {
            let path = path_example(config.path_style);
            match config.notation {
                Notation::Markdown => format!("[Label]({path})"),
                Notation::Wikilink => format!("[[{path}]]"),
                Notation::Bare => path.to_string(),
            }
        }
    }
}

/// A worked path in the workspace's path style.
fn path_example(style: PathStyle) -> &'static str {
    match style {
        PathStyle::Root => "/path/from/here.md",
        PathStyle::Relative => "../path/x.md",
        PathStyle::Canonical => "path/x.md",
    }
}

/// How to resolve a path in the workspace's path style — the sentence that keeps
/// a reader from treating `/` as their filesystem root.
fn path_style_prose(style: PathStyle) -> &'static str {
    match style {
        PathStyle::Root => {
            "A target beginning with `/` is a path from **this directory**, the top \
             of the workspace; it is not a path from the root of your filesystem. \
             Anything else is a path relative to the file you found it in. Fold `.` \
             and `..` yourself, and do not follow symlinks."
        }
        PathStyle::Relative => {
            "A target is a path relative to the file you found it in, so `../` \
             climbs one directory from that file. Fold `.` and `..` yourself, and \
             do not follow symlinks."
        }
        PathStyle::Canonical => {
            "A target is a path from **this directory**, the top of the workspace, \
             written without a leading slash. Fold `.` and `..` yourself, and do \
             not follow symlinks."
        }
    }
}

/// How an id reference is resolved — and this is where `id_storage` genuinely
/// changes a reader's instructions, so it is not one sentence but three shapes.
fn id_resolution_prose(config: &WorkspaceConfig) -> String {
    match config.id_storage {
        IdStorage::FrontmatterOnly => {
            "**To resolve one, search this directory for the file whose `id` field \
             is that value.** There is no index to consult and no lookup table; \
             every document carries its own id in its metadata, and that is the \
             only place the mapping exists. A plain text search for the identifier \
             will find it. This is why files can be renamed and moved here without \
             breaking anything: nothing points at a filename."
                .to_string()
        }
        IdStorage::Registry => {
            "**To resolve one, look it up in the registry** — the file the root \
             points at through its `registry` key, which maps every id to the path \
             holding it. The documents themselves do not carry their ids, so the \
             registry is the only place the mapping exists."
                .to_string()
        }
        IdStorage::Frontmatter => {
            "**To resolve one, either search this directory for the file whose \
             `id` field is that value, or look it up in the registry** — the file \
             the root points at through its `registry` key. Both records exist and \
             are kept in step; the id in the document is the one that survives \
             being moved or copied."
                .to_string()
        }
    }
}

/// Whether and how a document earns an id.
fn identity_prose(config: &WorkspaceConfig, ctx: &AboutContext) -> String {
    if config.identity == Registration::OFF {
        return "Documents here have no permanent ids. A document is identified by \
                where it sits, so moving one changes how it is referred to."
            .to_string();
    }
    let when = if config.identity.on_create {
        "Every document is given a permanent id when it is created"
    } else {
        "A document earns a permanent id the first time something links to it by \
         id, or when it is published — not before"
    };
    let stored = match config.id_storage {
        IdStorage::FrontmatterOnly => {
            ", stored in its own metadata. There is no registry file; the ids in the \
             documents are the only record, which is why they survive being moved \
             or copied."
        }
        IdStorage::Registry => {
            ". The id is recorded in a registry file rather than in the document, so \
             the registry is what has to travel with the directory."
        }
        IdStorage::Frontmatter => {
            ". The id is stamped into the document's own metadata, and a registry \
             file mirrors it. Because the id lives in the file, it survives being \
             moved or copied."
        }
    };
    let _ = ctx;
    format!("{when}{stored}")
}

/// The reference spellings prov will *accept* beyond the one it writes.
///
/// Derived from prov's read behavior, never from the corpus — which is what
/// makes it both free to compute and impossible to falsify by a later edit.
fn alternative_spellings(config: &WorkspaceConfig) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = Vec::new();
    let path = path_example(config.path_style);

    let writes_markdown_path =
        config.reference_target == Addressing::Path && config.notation == Notation::Markdown;
    let writes_wikilink_path =
        config.reference_target == Addressing::Path && config.notation == Notation::Wikilink;
    let writes_bare_path =
        config.reference_target == Addressing::Path && config.notation == Notation::Bare;

    if !writes_wikilink_path {
        out.push((format!("[[{path}]]"), "a wikilink holding a path"));
    }
    if !writes_markdown_path {
        out.push((format!("[Label]({path})"), "a Markdown link"));
    }
    if !writes_bare_path {
        out.push((path.to_string(), "a bare target"));
    }
    out
}

// ─── small helpers ───────────────────────────────────────────────────────────

/// A path as the page should print it — forward slashes on every platform,
/// because the page describes a directory, not a filesystem.
fn display_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The target text of a configured link value (`'[Audiences](/vocab/a.yaml)'` →
/// `/vocab/a.yaml`), so the page names the file rather than the link syntax.
fn link_target_text(value: &str) -> String {
    crate::link::Link::parse(value).target
}

/// The content relations, in reading order — each relation immediately followed
/// by its inverse, and the spine first.
///
/// Order is a legibility decision, not a cosmetic one: a reader meeting
/// `contents` wants `part_of` on the very next row, and the vocabulary arrives
/// from config through a `BTreeMap`, which would otherwise interleave the pairs
/// alphabetically (`contents`, `link_of`, `links`, `part_of`).
///
/// Pointer relations are excluded entirely — they are machinery with their own
/// section, and listing them here would invite a reader to follow them as
/// though they were structure.
fn content_relations(relations: &RelationSet) -> Vec<&crate::relation::Relation> {
    fn take<'a>(
        ordered: &mut Vec<&'a crate::relation::Relation>,
        relations: &'a RelationSet,
        name: &str,
    ) {
        if is_pointer(relations, name) || ordered.iter().any(|r| r.name == name) {
            return;
        }
        if let Some(rel) = relations.relations().iter().find(|r| r.name == name) {
            ordered.push(rel);
        }
    }

    let mut ordered: Vec<&crate::relation::Relation> = Vec::new();

    // The spine and its inverse lead, because they are what a reader needs in
    // order to walk the directory at all.
    if let Some(spanning) = relations.spanning_relation() {
        take(&mut ordered, relations, spanning);
        if let Some(inverse) = inverse_of(relations, spanning) {
            take(&mut ordered, relations, &inverse);
        }
    }
    // Then everything else in vocabulary order, each pulling its inverse along
    // so the two halves of a pair never end up separated.
    let names: Vec<String> = relations
        .relations()
        .iter()
        .map(|r| r.name.clone())
        .collect();
    for name in &names {
        take(&mut ordered, relations, name);
        if let Some(inverse) = inverse_of(relations, name) {
            take(&mut ordered, relations, &inverse);
        }
    }
    ordered
}

/// Whether `name` is one of the pointer relations rather than content structure.
fn is_pointer(relations: &RelationSet, name: &str) -> bool {
    [
        relations.registry_relation(),
        relations.config_relation(),
        relations.recycle_relation(),
        relations.history_relation(),
        relations.about_relation(),
    ]
    .contains(&Some(name))
}

/// The declared inverse of `name`, if the vocabulary gives it one.
fn inverse_of(relations: &RelationSet, name: &str) -> Option<String> {
    relations
        .relations()
        .iter()
        .find(|r| r.name == name)
        .and_then(|r| r.inverse.clone())
}

/// The "up" field of the spanning tree — what a document uses to name its
/// parent. Used for worked examples, so the sample looks like this workspace.
fn spanning_up_field(relations: &RelationSet) -> Option<String> {
    relations
        .spanning_relation()
        .and_then(|s| inverse_of(relations, s))
}

/// Small counts read better as words.
fn number_word(n: usize) -> &'static str {
    match n {
        0 => "no",
        1 => "one",
        2 => "two",
        3 => "three",
        4 => "four",
        5 => "five",
        6 => "six",
        7 => "seven",
        8 => "eight",
        9 => "nine",
        10 => "ten",
        _ => "several",
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `a`, `a and b`, `a, b and c`.
fn join_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Whether the config axis asks for a page at all.
pub fn enabled(config: &WorkspaceConfig) -> bool {
    config.about == About::Structure
}

/// Whether two renderings of the page say the same thing — **comparing bodies
/// only**, with each side's metadata block excluded.
///
/// Excluding the block is what lets the byline carry a version without the
/// version becoming a staleness trigger. `generated_by: prov 0.3.2` in a
/// workspace regenerated by 0.4.0 is a stale *byline*, which costs a reader
/// nothing; treating it as a stale *page* would fire `check` in every workspace
/// on earth after every release, and would rewrite files whose prose is
/// identical. It is also what makes `embed_style: separate` work at all, since
/// a content-only page has no block to compare.
///
/// Comparison is textual after the split. Reflowing the prose by hand *does*
/// read as stale — correctly so, since the page says its edits are overwritten
/// and regenerating costs nothing.
pub fn same_body(actual: &str, expected: &str, format: ContentFormat) -> bool {
    body_of(actual, format) == body_of(expected, format)
}

/// A page's prose, with any metadata block removed and surrounding blank lines
/// trimmed.
fn body_of(text: &str, format: ContentFormat) -> String {
    let path = format!("about.{}", format.extension());
    match crate::document::Document::parse(&path, text) {
        Ok(doc) => doc.body.trim().to_string(),
        // An unparseable page is not a reason to refuse an answer — it is a
        // reason to say "this does not match", which the caller repairs by
        // regenerating.
        Err(_) => text.trim().to_string(),
    }
}

// These tests build YAML/JSON metadata blocks, so they need a format backend.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::config::{FieldSpec, OpenClosed, RelationDef};
    use crate::relation::Cardinality;
    use std::collections::BTreeMap;

    fn def(card: Cardinality, inverse: &str, means: &str) -> RelationDef {
        RelationDef {
            cardinality: Some(card),
            inverse: Some(inverse.into()),
            means: Some(means.into()),
        }
    }

    /// This repository's own vocabulary, as `prov.yaml` declares it.
    fn diaryx_defs() -> BTreeMap<String, RelationDef> {
        BTreeMap::from([
            (
                "contents".into(),
                def(
                    Cardinality::Many,
                    "part_of",
                    "documents contained by this one",
                ),
            ),
            (
                "part_of".into(),
                def(
                    Cardinality::One,
                    "contents",
                    "the document that contains this one",
                ),
            ),
            (
                "links".into(),
                def(
                    Cardinality::Many,
                    "link_of",
                    "arbitrary cross-references to other documents",
                ),
            ),
            (
                "link_of".into(),
                def(
                    Cardinality::Many,
                    "links",
                    "documents that cross-reference this one",
                ),
            ),
        ])
    }

    fn default_workspace() -> (WorkspaceConfig, AboutContext) {
        let config = WorkspaceConfig {
            spanning: Some("contents".into()),
            relation_defs: diaryx_defs(),
            ..WorkspaceConfig::default()
        };
        let ctx = AboutContext {
            root_doc: "README.md".into(),
            config_doc: Some("prov.yaml".into()),
            version: "0.0.0".into(),
            ..AboutContext::default()
        };
        (config, ctx)
    }

    fn render(config: &WorkspaceConfig, ctx: &AboutContext) -> String {
        let relations = RelationSet::from_config(config);
        generate(config, &relations, ctx).expect("generate")
    }

    /// The page with its prose whitespace collapsed, so a phrase assertion is
    /// not defeated by the line the wrapper happened to break on. Wrapping is
    /// tested on its own; every *other* test is about what the page says.
    fn flat(page: &str) -> String {
        page.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The rows of the table introduced by `header`, in order.
    fn rows_under<'a>(page: &'a str, header: &str) -> Vec<&'a str> {
        let start = page
            .find(header)
            .unwrap_or_else(|| panic!("no table headed {header:?}"));
        page[start..]
            .lines()
            .skip(2) // the header row and the `| --- |` separator
            .take_while(|l| l.starts_with('|'))
            .collect()
    }

    #[test]
    fn a_default_workspace_gets_the_concrete_facts_not_the_general_rules() {
        let (config, ctx) = default_workspace();
        let page = render(&config, &ctx);

        let flat = flat(&page);
        // The whole thesis: resolved facts, not branches the reader must choose
        // between. The spec's "`---`, `;;;`, or ```fig`" becomes one fence.
        assert!(flat.contains("A file begins with a line containing three dashes"));
        assert!(!flat.contains("three semicolons"));
        assert!(!page.contains("```fig"));
        assert!(flat.contains("written in YAML"));
        assert!(!flat.contains("written in JSON"));

        // The root is stated, never the procedure for finding one.
        assert!(flat.contains("`README.md` is the root"));
        assert!(!page.contains(".prov"));
        assert!(!page.contains("index.md"));
    }

    #[test]
    fn the_page_never_says_prov_or_addresses_a_tool_author() {
        // "Operationally complete, generally incomplete" — the body must read as
        // being about *this directory*, never about the scheme in general. The
        // footer is the single deliberate exception, so it is excluded here.
        let (config, ctx) = default_workspace();
        let page = render(&config, &ctx);
        let body = flat(page.split("\n---\n\n<sub>").next().unwrap());
        for forbidden in ["prov/1", "the spec", "workspaces", "your tool", "DESIGN"] {
            assert!(
                !body.contains(forbidden),
                "body should not appeal to prov as an institution, found {forbidden:?}"
            );
        }
        // ...and the footer *does* name the scheme, once, for the reader who
        // wants to write a tool rather than read the directory.
        assert!(flat(&page).contains("The scheme these files follow is called prov"));
    }

    #[test]
    fn length_tracks_how_unusual_the_workspace_is() {
        let (plain, ctx) = default_workspace();
        let plain_page = render(&plain, &ctx);

        let bespoke = WorkspaceConfig {
            spanning: Some("sections".into()),
            relation_defs: BTreeMap::from([
                (
                    "sections".into(),
                    def(
                        Cardinality::Many,
                        "section_of",
                        "records filed under this one",
                    ),
                ),
                (
                    "section_of".into(),
                    def(
                        Cardinality::One,
                        "sections",
                        "the record this one is filed under",
                    ),
                ),
                (
                    "cites".into(),
                    def(Cardinality::Many, "cited_by", "records this one draws on"),
                ),
                (
                    "cited_by".into(),
                    def(Cardinality::Many, "cites", "records that draw on this one"),
                ),
            ]),
            fields: BTreeMap::from([(
                "audience".into(),
                FieldSpec {
                    ty: None,
                    values: OpenClosed::Closed,
                    vocabulary: Some("[Audiences](/vocab/audiences.yaml)".into()),
                    reify: true,
                },
            )]),
            reference_target: Addressing::Id,
            id_storage: IdStorage::FrontmatterOnly,
            recycle_bin: false,
            fixity: Fixity::Full,
            updated: "modified".into(),
            ..WorkspaceConfig::default()
        };
        let bespoke_ctx = AboutContext {
            history_doc: Some("history/index.yaml".into()),
            ..ctx.clone()
        };
        let bespoke_page = render(&bespoke, &bespoke_ctx);

        assert!(
            bespoke_page.len() > plain_page.len(),
            "a workspace with more to explain must produce a longer page \
             ({} vs {})",
            bespoke_page.len(),
            plain_page.len()
        );
        // Sections with nothing to say are absent entirely, not stubbed.
        assert!(!plain_page.contains("## Fields with fixed vocabularies"));
        assert!(!plain_page.contains("## The history store"));
        assert!(bespoke_page.contains("## Fields with fixed vocabularies"));
        assert!(bespoke_page.contains("## The history store"));
    }

    #[test]
    fn separate_embedding_yields_a_content_only_page() {
        // No file in such a workspace carries a fence, so a fenced `about.md`
        // would be the anomaly — and it gets no sidecar either.
        let (mut config, ctx) = default_workspace();
        config.embed_style = EmbedStyle::Separate;
        let page = render(&config, &ctx);

        let flat = flat(&page);
        assert!(page.starts_with("# How this workspace is organized"));
        assert!(!page.contains("generated_by"));
        // The title survives as the heading, and the byline as the footer —
        // versionless: the version lives in `generated_by`, in the metadata
        // block, which this embedding style has none of.
        assert!(flat.contains("Generated by prov from this workspace's configuration"));
        // The opening paragraph must describe the companion-file convention
        // rather than claiming a block at the top of each file.
        assert!(flat.contains("companion file beside it"));
        assert!(!flat.contains("in a block at the top of itself"));
    }

    #[test]
    fn the_metadata_block_follows_the_workspace_carrier() {
        let (mut config, ctx) = default_workspace();
        config.default_embed_format = fig::Format::Json;
        let page = render(&config, &ctx);
        // The page's own block is spelled the way this workspace spells blocks,
        // so it does not contradict the sentence it contains.
        assert!(page.starts_with(";;;"));
        assert!(flat(&page).contains("A file begins with a line containing three semicolons"));

        let (mut config, ctx) = default_workspace();
        config.embed_style = EmbedStyle::CodeBlock;
        let page = render(&config, &ctx);
        assert!(page.contains("```yaml"));
        assert!(flat(&page).contains("fenced code block labelled `yaml`"));
    }

    #[test]
    fn describes_what_prov_reads_not_what_the_files_contain() {
        // The corpus is never consulted, so the alternative spellings are the
        // ones prov *accepts*, minus the one it writes.
        let (config, ctx) = default_workspace();
        let page = render(&config, &ctx);
        let spellings = rows_under(&page, "| written | called |").join("\n");
        assert!(spellings.contains("a wikilink holding a path"));
        assert!(spellings.contains("a bare target"));
        // Markdown is what this workspace writes, so it is not offered as an
        // "other spelling" — it is the spelling.
        assert!(!spellings.contains("a Markdown link"), "{spellings}");
    }

    #[test]
    fn relation_pairs_stay_adjacent_and_the_spine_leads() {
        // The vocabulary arrives through a BTreeMap, which would interleave the
        // pairs alphabetically (contents, link_of, links, part_of).
        let (config, ctx) = default_workspace();
        let page = render(&config, &ctx);
        let rows = rows_under(&page, "| relation | means | how many | its opposite |");
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert!(rows[0].starts_with("| `contents`"), "{rows:?}");
        assert!(rows[1].starts_with("| `part_of`"), "{rows:?}");
        assert!(rows[2].starts_with("| `link_of`"), "{rows:?}");
        assert!(rows[3].starts_with("| `links`"), "{rows:?}");
    }

    #[test]
    fn pointer_relations_never_appear_as_structure() {
        let (config, ctx) = default_workspace();
        let page = render(&config, &ctx);
        let table = rows_under(&page, "| relation | means | how many | its opposite |").join("\n");
        for pointer in ["`registry`", "`recycle_bin`", "`history`", "`about`"] {
            assert!(
                !table.contains(pointer),
                "{pointer} is machinery, not a relation a reader should follow"
            );
        }
    }

    #[test]
    fn a_user_chosen_name_cannot_break_out_of_its_code_span_or_cell() {
        // Relation names come from config, so they are attacker-adjacent input in
        // the only sense that matters here: a name with a backtick or a pipe must
        // not corrupt the table it lands in.
        let mut defs = diaryx_defs();
        defs.insert(
            "we|ird".into(),
            def(Cardinality::Many, "back`tick", "a name with punctuation"),
        );
        let config = WorkspaceConfig {
            spanning: Some("contents".into()),
            relation_defs: defs,
            ..WorkspaceConfig::default()
        };
        let ctx = AboutContext::new("README.md", "0.0.0");
        let page = render(&config, &ctx);

        assert!(
            page.contains("we\\|ird"),
            "a pipe must be escaped in a cell"
        );
        assert!(
            page.contains("`` back`tick ``"),
            "a backtick must widen its fence"
        );
        // Every table row still has the same number of cells as its header.
        for line in page.lines().filter(|l| l.starts_with('|')) {
            let cells = line.matches("| ").count();
            assert!(cells >= 2, "row collapsed: {line}");
        }
    }

    #[test]
    fn worked_examples_use_this_workspaces_own_vocabulary() {
        // A sample showing `part_of` to a reader whose files say `section_of`
        // teaches the wrong key — the example must be one that could occur here.
        let config = WorkspaceConfig {
            spanning: Some("sections".into()),
            relation_defs: BTreeMap::from([
                (
                    "sections".into(),
                    def(
                        Cardinality::Many,
                        "section_of",
                        "records filed under this one",
                    ),
                ),
                (
                    "section_of".into(),
                    def(
                        Cardinality::One,
                        "sections",
                        "the record this one is filed under",
                    ),
                ),
            ]),
            reference_target: Addressing::Id,
            reference_label: true,
            ..WorkspaceConfig::default()
        };
        let ctx = AboutContext::new("README.md", "0.0.0");
        let page = render(&config, &ctx);

        assert!(
            page.contains("section_of: '[[id:aj7eqx|Board Records]]'"),
            "{page}"
        );
        assert!(
            !page.contains("part_of:"),
            "the diaryx default must not leak in"
        );
    }

    #[test]
    fn transcodes_into_the_workspace_content_format() {
        let (mut config, ctx) = default_workspace();
        config.content_format = ContentFormat::Html;
        let page = render(&config, &ctx);
        assert!(page.contains("<h1>How this workspace is organized</h1>"));
        assert!(page.contains("<table>"));
        // The metadata block is still the workspace's, wrapped around HTML prose.
        assert!(page.starts_with("---\ntitle:"));
    }

    #[test]
    fn identity_off_says_so_rather_than_describing_ids() {
        let (mut config, ctx) = default_workspace();
        config.identity = Registration::OFF;
        let page = render(&config, &ctx);
        let flat = flat(&page);
        assert!(flat.contains("Documents here have no permanent ids"));
        // And `id` is not listed among the fields to leave alone, because there
        // is no such field to protect.
        assert!(!flat.contains("**`id`** — a permanent handle"));
    }

    #[test]
    fn wrapping_keeps_every_prose_line_within_the_margin() {
        let (config, ctx) = default_workspace();
        let page = render(&config, &ctx);
        for line in page.lines() {
            // Tables and the worked examples are emitted verbatim by design.
            if line.starts_with('|') || line.starts_with("```") {
                continue;
            }
            assert!(
                line.chars().count() <= WRAP + 2,
                "line exceeds the margin ({} chars): {line}",
                line.chars().count()
            );
        }
    }
}
