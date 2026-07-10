//! `colophon` — command-line companion for the colophon library.
//!
//! A thin adapter: parse arguments, call into the library, render the result.
//! All logic lives in `colophon`; this crate is I/O and presentation only.
//!
//! Single-document commands (`show`, `links`, `meta`, `get`, `body`, `set`,
//! `unset`) operate on the pure layers. Workspace commands (`tree`, `check`,
//! `new`, `mv`, `rm`) drive the library's [`colophon::StdFs`]-backed engine,
//! rooted at the current directory, through the dependency-free
//! [`colophon::block_on`] executor.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use colophon::tree::{Node, NodeKind};
use colophon::{
    ContentFormat, Document, FileIndex, Format, Id, IndexStore, LinkStyle, Mapping, Minter,
    Registration, RelationSet, StdFs, Trigger, Value, Workspace, WorkspaceConfig, block_on, edit,
    link, meta,
};

/// The filename stem of the registry document the CLI creates on first
/// `colophon id` — visible, beside the root, and *linked from the root's own
/// metadata* via the `registry` relation. Its extension is the workspace's
/// metadata format (see [`sidecar_name`]). Where the registry lives is a fact
/// about the workspace, declared in it; the CLI only supplies this default when
/// bootstrapping one. (It can equally be a `.md` file whose frontmatter carries
/// the records — anything the pointer targets.)
const REGISTRY_STEM: &str = "registry";

/// The filename stem of the config document the CLI creates on first
/// `colophon config <k> <v>` (or at `init`) — beside the root, linked via the
/// `config` relation (the reachability move the registry uses). Workspace policy
/// lives here rather than bloating the root or hiding in a dotfile.
const CONFIG_STEM: &str = "colophon";

/// The whole-file extension for a metadata format: the config and registry
/// sidecars are written in the workspace's *chosen metadata format*, not always
/// YAML — `yaml`/`json`/`figl`. Mirrors [`colophon::document::whole_file_format`],
/// which parses them back.
fn sidecar_ext(format: Format) -> &'static str {
    match format {
        #[cfg(feature = "json")]
        Format::Json => "json",
        #[cfg(feature = "fig-lang")]
        Format::Fig => "figl",
        _ => "yaml",
    }
}

/// The sidecar filename for `stem` in metadata `format` (e.g. `colophon.figl`).
fn sidecar_name(stem: &str, format: Format) -> String {
    format!("{stem}.{}", sidecar_ext(format))
}

/// A self-describing plaintext workspace, from the command line.
#[derive(Parser)]
#[command(name = "colophon", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize a new workspace here: write a self-describing root document
    /// that the other commands can discover. The starting point — `tree`,
    /// `new`, and `check` all need a root to work from. On a terminal, prompts
    /// for anything not given as a flag; pass `--yes` to take every default.
    Init {
        /// Directory to initialize (default: the current directory). Created if
        /// it does not exist.
        dir: Option<PathBuf>,
        /// Title for the root document (default: the directory's name, titleized).
        #[arg(long)]
        title: Option<String>,
        /// Author to record in the root's metadata (default: none).
        #[arg(long)]
        author: Option<String>,
        /// Metadata (frontmatter) format for the root document (default: yaml).
        #[arg(long, value_enum)]
        meta: Option<MetaFormat>,
        /// Body-prose grammar; sets the root file's extension (default: markdown).
        #[arg(long, value_enum)]
        content: Option<ContentLang>,
        /// How path links are written (default: markdown-root).
        #[arg(long, value_enum)]
        link_style: Option<LinkStyleArg>,
        /// Identity model: diaryx (paths only) or obsidian (stable IDs) (default: diaryx).
        #[arg(long, value_enum)]
        identity: Option<IdentityArg>,
        /// Accept every default without prompting.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Summarize a document: its metadata, spanning children, and declared links.
    Show {
        /// Path to a document (plaintext with embedded metadata).
        file: PathBuf,
    },
    /// List a document's links as `relation<TAB>target`, one per line.
    Links {
        /// Path to a document.
        file: PathBuf,
        /// Only show links declared by this relation (e.g. `contents`).
        #[arg(long)]
        relation: Option<String>,
    },
    /// Print a document's metadata block (without fences).
    Meta {
        /// Path to a document.
        file: PathBuf,
        /// Output format (default: the format the document already uses).
        #[arg(long, value_enum)]
        format: Option<MetaFormat>,
    },
    /// Print one metadata field by dotted path (e.g. `title`, `contents.0`).
    Get {
        /// Path to a document.
        file: PathBuf,
        /// Dotted key path; an all-digit segment indexes a sequence.
        key: String,
    },
    /// Print a document's body (everything outside the metadata block).
    Body {
        /// Path to a document.
        file: PathBuf,
    },
    /// Render a document's body to HTML (Markdown/Djot, via `twig`).
    Render {
        /// Path to a document.
        file: PathBuf,
    },
    /// Set a metadata field (comment- and format-preserving; creates the
    /// block when the document has none).
    Set {
        /// Path to a document.
        file: PathBuf,
        /// Dotted key path.
        key: String,
        /// Value; `true`/`false`, integers, floats, and `null` are typed,
        /// everything else is a string.
        value: String,
    },
    /// Remove a metadata field (comment- and format-preserving).
    Unset {
        /// Path to a document.
        file: PathBuf,
        /// Dotted key path.
        key: String,
    },
    /// Print the containment tree that unfolds from a root document.
    Tree {
        /// The document to discover from (default: the workspace root).
        root: Option<PathBuf>,
    },
    /// Check workspace integrity from a root: broken links, case mismatches,
    /// duplicate containment, missing inverse links, dangling IDs. Exits 1 on
    /// findings.
    Check {
        /// The document to check from (default: the workspace root).
        root: Option<PathBuf>,
        /// Interactively repair fixable findings (currently: missing inverse
        /// links). Metadata edits only — body-link findings are left for a
        /// structure-aware pass, so code that looks like a link is never touched.
        #[arg(long)]
        fix: bool,
    },
    /// Create a document as a child of a parent, linking both directions.
    New {
        /// Path of the document to create.
        path: PathBuf,
        /// The parent document (gains a spanning link to the new file).
        #[arg(long, short)]
        parent: PathBuf,
    },
    /// Move/rename a document, maintaining every affected link: every inbound
    /// reference across the workspace (parent entry, children's inverses,
    /// overlay links, body wikilinks) and the document's own relative links.
    Mv {
        /// Current path.
        from: PathBuf,
        /// New path.
        to: PathBuf,
    },
    /// Delete a document, removing its parent's spanning entry. Refuses when
    /// the document has children unless --force.
    Rm {
        /// Path of the document to delete.
        path: PathBuf,
        /// Delete even when the document still contains children (orphans them).
        #[arg(long)]
        force: bool,
    },
    /// Ensure a document has a stable ID and print its `colophon:<id>` target.
    /// Registers it in the workspace's registry document (bootstrapping
    /// registry.yaml + the root's `registry` pointer on first use) — link that
    /// target from any document and it survives moves.
    Id {
        /// Path to a document.
        file: PathBuf,
    },
    /// Resolve a stable ID (with or without the `colophon:` prefix) to its
    /// current path.
    Resolve {
        /// The ID to resolve.
        id: String,
    },
    /// List the documents that link to a document (its backlinks), across the
    /// workspace, as `source<TAB>site<TAB>path|id`.
    Backlinks {
        /// The document whose backlinks to list.
        file: PathBuf,
    },
    /// Get or set workspace config (e.g. `link_format`, `identity`). With a
    /// value, writes it to the linked config document — creating and linking
    /// `colophon.yaml` from the root on first use. With a key only, prints that
    /// value; with no key, prints the effective config.
    Config {
        /// The config key (e.g. `link_format`, `identity`). Omit to print all.
        key: Option<String>,
        /// The value to set. Omit to read.
        value: Option<String>,
    },
}

/// CLI spelling of the metadata formats colophon compiles in. Variants track the
/// crate's format features: YAML is always available; JSON and the native fig
/// dialect appear only when their features are enabled, so `--format` never
/// offers a format whose parser is not in the binary.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MetaFormat {
    Yaml,
    #[cfg(feature = "json")]
    Json,
    #[cfg(feature = "fig-lang")]
    Fig,
}

impl MetaFormat {
    /// The lowercase spelling for the `init` summary line.
    fn label(self) -> &'static str {
        match self {
            MetaFormat::Yaml => "yaml",
            #[cfg(feature = "json")]
            MetaFormat::Json => "json",
            #[cfg(feature = "fig-lang")]
            MetaFormat::Fig => "fig",
        }
    }
}

/// CLI spelling of the body-prose grammars `twig` parses. Unlike the metadata
/// formats these are always available (twig is a required dependency), so no
/// variant is feature-gated.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ContentLang {
    Markdown,
    Djot,
    Html,
}

impl ContentLang {
    /// The root document's file extension for this grammar.
    fn ext(self) -> &'static str {
        match self {
            ContentLang::Markdown => "md",
            ContentLang::Djot => "dj",
            ContentLang::Html => "html",
        }
    }

    /// A title heading in this grammar — the seed body of the root document.
    fn heading(self, title: &str) -> String {
        match self {
            // Markdown and Djot share ATX heading syntax.
            ContentLang::Markdown | ContentLang::Djot => format!("# {title}\n"),
            ContentLang::Html => format!("<h1>{title}</h1>\n"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            ContentLang::Markdown => "markdown",
            ContentLang::Djot => "djot",
            ContentLang::Html => "html",
        }
    }
}

impl From<ContentLang> for ContentFormat {
    fn from(c: ContentLang) -> Self {
        match c {
            ContentLang::Markdown => ContentFormat::Markdown,
            ContentLang::Djot => ContentFormat::Djot,
            ContentLang::Html => ContentFormat::Html,
        }
    }
}

/// CLI spelling of the workspace link styles ([`colophon::LinkStyle`]).
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LinkStyleArg {
    MarkdownRoot,
    MarkdownRelative,
    PlainRelative,
    PlainCanonical,
}

impl From<LinkStyleArg> for LinkStyle {
    fn from(l: LinkStyleArg) -> Self {
        match l {
            LinkStyleArg::MarkdownRoot => LinkStyle::MarkdownRoot,
            LinkStyleArg::MarkdownRelative => LinkStyle::MarkdownRelative,
            LinkStyleArg::PlainRelative => LinkStyle::PlainRelative,
            LinkStyleArg::PlainCanonical => LinkStyle::PlainCanonical,
        }
    }
}

/// The two identity models `init` offers, each a preset over the config's
/// `identity` + `id_links` knobs: **Diaryx** is paths-only (no IDs), **Obsidian**
/// mints stable IDs lazily and authors links by ID (so moves rewrite nothing).
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum IdentityArg {
    Diaryx,
    Obsidian,
}

impl IdentityArg {
    /// `(registration triggers, whether colophon authors id links)`.
    fn to_config(self) -> (Registration, bool) {
        match self {
            IdentityArg::Diaryx => (Registration::OFF, false),
            IdentityArg::Obsidian => (Registration::LAZY, true),
        }
    }

    fn label(self) -> &'static str {
        match self {
            IdentityArg::Diaryx => "diaryx",
            IdentityArg::Obsidian => "obsidian",
        }
    }
}

impl From<MetaFormat> for Format {
    fn from(f: MetaFormat) -> Format {
        match f {
            MetaFormat::Yaml => Format::Yaml,
            #[cfg(feature = "json")]
            MetaFormat::Json => Format::Json,
            #[cfg(feature = "fig-lang")]
            MetaFormat::Fig => Format::Fig,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Show { file } => cmd_show(&file),
        Command::Links { file, relation } => cmd_links(&file, relation.as_deref()),
        Command::Meta { file, format } => cmd_meta(&file, format),
        Command::Get { file, key } => cmd_get(&file, &key),
        Command::Body { file } => cmd_body(&file),
        Command::Render { file } => cmd_render(&file),
        Command::Init { dir, title, author, meta, content, link_style, identity, yes } => {
            cmd_init(dir.as_deref(), title, author, meta, content, link_style, identity, yes)
        }
        Command::Set { file, key, value } => cmd_set(&file, &key, &value),
        Command::Unset { file, key } => cmd_unset(&file, &key),
        Command::Tree { root } => cmd_tree(root.as_deref()),
        Command::Check { root, fix } => cmd_check(root.as_deref(), fix),
        Command::New { path, parent } => cmd_new(&path, &parent),
        Command::Mv { from, to } => cmd_mv(&from, &to),
        Command::Rm { path, force } => cmd_rm(&path, force),
        Command::Id { file } => cmd_id(&file),
        Command::Resolve { id } => cmd_resolve(&id),
        Command::Backlinks { file } => cmd_backlinks(&file),
        Command::Config { key, value } => cmd_config(key.as_deref(), value.as_deref()),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("colophon: {err}");
            ExitCode::FAILURE
        }
    }
}

type CmdResult = Result<ExitCode, Box<dyn std::error::Error>>;

/// The relation vocabulary. For now the diaryx preset; configurable vocabularies
/// (and a `--relations` flag) come later.
fn relation_set() -> RelationSet {
    RelationSet::diaryx()
}

/// The discovered workspace context: where the root is, which document is the
/// root, and where the root says the registry lives.
struct Ctx {
    /// Absolute path of the workspace root directory.
    root_dir: PathBuf,
    /// The root document, relative to `root_dir`.
    root_doc: PathBuf,
    /// The registry document the root declares (relative to `root_dir`), if any.
    registry: Option<PathBuf>,
    /// The effective workspace config (root frontmatter overlaid by the linked
    /// config document, over defaults).
    config: WorkspaceConfig,
}

type AnyError = Box<dyn std::error::Error>;

/// Find the workspace root by walking up from the current directory: in each
/// directory, a candidate root is a document (any content grammar — see
/// [`ROOT_EXTS`]) with metadata and no `part_of` (nothing contains it). A file
/// stemmed `index`, then `readme`, wins ties.
fn find_root() -> Result<Ctx, AnyError> {
    let cwd = std::env::current_dir()?;
    for dir in cwd.ancestors() {
        let mut candidates: Vec<String> = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_root_ext = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| ROOT_EXTS.contains(&e.to_ascii_lowercase().as_str()));
            if !is_root_ext {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok(doc) = Document::parse(&path, &text) else { continue };
            if doc.has_meta() && doc.meta.get("part_of").is_none() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    candidates.push(name.to_string());
                }
            }
        }
        // Prefer a file stemmed `index`, then `readme` (any extension); failing
        // that, a lone candidate. Two-plus unnamed candidates are ambiguous.
        let stem_is = |name: &str, want: &str| {
            Path::new(name)
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case(want))
        };
        let chosen = candidates
            .iter()
            .find(|n| stem_is(n, "index"))
            .or_else(|| candidates.iter().find(|n| stem_is(n, "readme")))
            .cloned()
            .or_else(|| (candidates.len() == 1).then(|| candidates[0].clone()));
        match chosen {
            Some(root_doc) => {
                let root_dir = dir.to_path_buf();
                let root_doc = PathBuf::from(root_doc);
                // Ask the root where its registry lives (the pointer relation).
                let probe: Workspace<StdFs> = Workspace::builder(StdFs).root(&root_dir).build();
                let registry = block_on(probe.registry_path(&root_doc))?;
                // Build the effective config: defaults, overlaid by the root
                // frontmatter (diaryx compat, e.g. `link_format`), overlaid by
                // the linked config document (which wins).
                let mut config = WorkspaceConfig::default();
                if let Some(text) = std::fs::read_to_string(root_dir.join(&root_doc)).ok()
                    && let Ok(doc) = Document::parse(&root_doc, &text)
                {
                    config.apply(&doc.meta);
                }
                if let Ok(Some(config_doc)) = block_on(probe.config_path(&root_doc))
                    && let Some(text) = std::fs::read_to_string(root_dir.join(&config_doc)).ok()
                    && let Ok(doc) = Document::parse(&config_doc, &text)
                {
                    config.apply(&doc.meta);
                }
                return Ok(Ctx { root_dir, root_doc, registry, config });
            }
            None if candidates.len() > 1 => {
                return Err(format!(
                    "ambiguous workspace root in {}: {} (rename one, or add part_of)",
                    dir.display(),
                    candidates.join(", ")
                )
                .into());
            }
            None => continue,
        }
    }
    Err("no workspace root found: no ancestor directory has a .md document \
with metadata and no part_of"
        .into())
}

/// The workspace the multi-document commands drive: rooted at the discovered
/// root, a lazy identity policy, and the registry the root declares (an empty
/// in-memory one when the root declares none — see `ensure_registry`).
fn workspace(ctx: &Ctx) -> Result<Workspace<StdFs, Minter, FileIndex>, AnyError> {
    let index = match &ctx.registry {
        Some(rel) => {
            let full = ctx.root_dir.join(rel);
            let text = match std::fs::read_to_string(&full) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(e.into()),
            };
            FileIndex::parse(rel, &text)?
        }
        // No registry declared yet: an empty in-memory one in the workspace's
        // metadata format, so a later bootstrap writes that format.
        None => FileIndex::new(ctx.config.default_embed_format),
    };
    Ok(Workspace::builder(StdFs)
        .root(&ctx.root_dir)
        .identity(Minter::with(ctx.config.identity, entropy_seed()))
        .index(index)
        .link_style(ctx.config.link_format)
        .id_links(ctx.config.id_links)
        .default_embed_format(ctx.config.default_embed_format)
        .build())
}

/// Make sure the workspace *declares* a registry, bootstrapping one when it
/// does not: create `registry.<ext>` (in the workspace's metadata format) beside
/// the root (self-described with a title and a part_of back to the root) and add
/// the `registry` pointer to the root's metadata — comment-preservingly, like
/// any other edit.
fn ensure_registry(ctx: &mut Ctx) -> Result<(), AnyError> {
    if ctx.registry.is_some() {
        return Ok(());
    }
    let format = ctx.config.default_embed_format;
    let registry_rel = PathBuf::from(sidecar_name(REGISTRY_STEM, format));
    let registry_full = ctx.root_dir.join(&registry_rel);
    if !registry_full.exists() {
        let mut seed = colophon::Mapping::new();
        seed.insert("title".into(), Value::String("ID registry".into()));
        seed.insert(
            "part_of".into(),
            Value::String(ctx.root_doc.to_string_lossy().into_owned()),
        );
        std::fs::write(&registry_full, meta::serialize_mapping(&seed, format)?)?;
    }
    let registry_name = registry_rel.to_string_lossy().into_owned();
    let root_full = ctx.root_dir.join(&ctx.root_doc);
    let text = std::fs::read_to_string(&root_full)?;
    let doc = Document::parse(&ctx.root_doc, &text)?;
    let updated = edit::set_in_text(&text, doc.carrier, "registry", edit::infer_scalar(&registry_name))?;
    std::fs::write(&root_full, updated)?;
    eprintln!("initialized {} (linked from {})", registry_rel.display(), ctx.root_doc.display());
    ctx.registry = Some(registry_rel);
    Ok(())
}

/// Persist the registry when a command changed it, to wherever the root says
/// it lives.
fn save_index(ctx: &Ctx, ws: &mut Workspace<StdFs, Minter, FileIndex>) -> Result<(), AnyError> {
    if !ws.index().is_dirty() {
        return Ok(());
    }
    let Some(rel) = &ctx.registry else {
        return Err("the registry changed but no registry document is declared".into());
    };
    let rendered = ws.index_mut().render()?;
    std::fs::write(ctx.root_dir.join(rel), rendered)?;
    ws.index_mut().mark_clean();
    Ok(())
}

/// Re-anchor a (cwd-relative) CLI path to the discovered workspace root.
fn ws_rel(ctx: &Ctx, path: &Path) -> Result<PathBuf, AnyError> {
    let abs = link::normalize(std::env::current_dir()?.join(path));
    abs.strip_prefix(&ctx.root_dir)
        .map(Path::to_path_buf)
        .map_err(|_| {
            format!(
                "{} is outside the workspace root {}",
                path.display(),
                ctx.root_dir.display()
            )
            .into()
        })
}

/// A seed for the minter from OS-seeded hasher state — dependency-free
/// randomness. (Uniqueness is enforced by rejection against the registry;
/// the seed only needs to differ between runs.)
fn entropy_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::hash::RandomState::new().build_hasher().finish()
}

fn load(file: &Path) -> Result<(String, Document), Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(file)?;
    let doc = Document::parse(file, &text)?;
    Ok((text, doc))
}

/// The root-document extensions `init` will not overwrite — every content
/// grammar's `index.*`, so a re-run is caught before it prompts (mirrors the
/// set `find_root` treats as root candidates).
const ROOT_EXTS: &[&str] = &["md", "markdown", "dj", "djot", "html", "htm"];

/// Initialize a workspace: write a self-describing root document (`index.<ext>`)
/// so `find_root` can discover it. Each field comes from its flag if given;
/// otherwise, on a terminal (and without `--yes`), the user is prompted, and in
/// every other case the default applies (title = directory name, no author,
/// YAML metadata, Markdown content). The frontmatter block is synthesized by
/// the same carrier-aware editor `set` uses, so the file is a normal document
/// from the start.
#[allow(clippy::too_many_arguments)]
fn cmd_init(
    dir: Option<&Path>,
    title: Option<String>,
    author: Option<String>,
    meta: Option<MetaFormat>,
    content: Option<ContentLang>,
    link_style: Option<LinkStyleArg>,
    identity: Option<IdentityArg>,
    yes: bool,
) -> CmdResult {
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None => std::env::current_dir()?,
    };
    std::fs::create_dir_all(&dir)?;
    // Canonicalize (now that the directory exists) for a stable absolute name —
    // both for the default title and the confirmation line.
    let dir = dir.canonicalize()?;

    // Bail before prompting if this directory is already a workspace.
    for ext in ROOT_EXTS {
        let existing = dir.join(format!("index.{ext}"));
        if existing.exists() {
            return Err(format!(
                "{} already exists — this looks like an initialized workspace",
                existing.display()
            )
            .into());
        }
    }

    let default_title = link::path_to_title(&dir);
    // Prompt only on a real terminal and only when `--yes` wasn't passed.
    let interactive = !yes && std::io::stdin().is_terminal();
    let prompting = interactive
        && (title.is_none()
            || author.is_none()
            || meta.is_none()
            || content.is_none()
            || link_style.is_none()
            || identity.is_none());
    if prompting {
        cliclack::intro("colophon init")?;
    }

    // Each field: flag wins; else prompt when interactive; else the default.
    let title = match title {
        Some(t) if !t.is_empty() => t,
        _ if interactive => cliclack::input("Title")
            .default_input(&default_title)
            .placeholder(&default_title)
            .interact::<String>()?,
        _ => default_title,
    };
    let author = match author {
        Some(a) => (!a.trim().is_empty()).then(|| a.trim().to_string()),
        None if interactive => {
            let entered: String = cliclack::input("Author")
                .required(false)
                .placeholder("optional — leave blank to omit")
                .interact()?;
            (!entered.trim().is_empty()).then(|| entered.trim().to_string())
        }
        None => None,
    };
    let meta = match meta {
        Some(m) => m,
        None if interactive => prompt_meta_format()?,
        None => MetaFormat::Yaml,
    };
    let content = match content {
        Some(c) => c,
        None if interactive => cliclack::select("Content format")
            .initial_value(ContentLang::Markdown)
            .item(ContentLang::Markdown, "Markdown", ".md")
            .item(ContentLang::Djot, "Djot", ".dj")
            .item(ContentLang::Html, "HTML", ".html")
            .interact()?,
        None => ContentLang::Markdown,
    };
    let link_style = match link_style {
        Some(l) => l,
        None if interactive => cliclack::select("Link style")
            .initial_value(LinkStyleArg::MarkdownRoot)
            .item(LinkStyleArg::MarkdownRoot, "Markdown, workspace-absolute", "[Title](/path.md)")
            .item(LinkStyleArg::MarkdownRelative, "Markdown, relative", "[Title](../path.md)")
            .item(LinkStyleArg::PlainRelative, "Plain relative path", "../path.md")
            .item(LinkStyleArg::PlainCanonical, "Plain workspace path", "path.md")
            .interact()?,
        None => LinkStyleArg::MarkdownRoot,
    };
    let identity = match identity {
        Some(i) => i,
        None if interactive => cliclack::select("Identity")
            .initial_value(IdentityArg::Diaryx)
            .item(IdentityArg::Diaryx, "Diaryx — paths only", "no IDs; plain-path links")
            .item(IdentityArg::Obsidian, "Obsidian — stable IDs", "id links survive moves")
            .interact()?,
        None => IdentityArg::Diaryx,
    };

    // Assemble the workspace preferences these choices encode.
    let (registration, id_links) = identity.to_config();
    let ws_config = WorkspaceConfig {
        link_format: link_style.into(),
        identity: registration,
        id_links,
        default_embed_format: meta.into(),
        content_format: content.into(),
    };

    // Write the root document: title, optional author, and a `config` pointer at
    // the config document we're about to create. Body first, then synthesize the
    // chosen frontmatter block around it (leading blank line = a conventional gap
    // after the closing fence).
    let meta_format: Format = meta.into();
    let config_name = sidecar_name(CONFIG_STEM, meta_format);
    let root_name = format!("index.{}", content.ext());
    let root = dir.join(&root_name);
    let carrier = colophon::document::frontmatter_carrier(meta_format);
    let body = format!("\n{}", content.heading(&title));
    let mut editor = edit::MetaEditor::open_or_init(&body, Some(carrier))?;
    editor.set_value(&edit::key_path("title"), edit::infer_scalar(&title))?;
    if let Some(author) = &author {
        editor.set_value(&edit::key_path("author"), edit::infer_scalar(author))?;
    }
    editor.set_value(&edit::key_path("config"), edit::infer_scalar(&config_name))?;
    std::fs::write(&root, editor.render()?)?;

    // Write the config document beside the root, in the chosen metadata format:
    // self-describing (title + `part_of` back to the root, in the chosen link
    // style) plus the recorded preferences. A whole-file config document (DESIGN
    // §6/§7), the same shape as the registry.
    let config_rel = PathBuf::from(&config_name);
    let part_of = link::format_link(ws_config.link_format, &config_rel, Path::new(&root_name), &title);
    let mut config_map = Mapping::new();
    config_map.insert("title".into(), Value::String("colophon config".into()));
    config_map.insert("part_of".into(), Value::String(part_of));
    for (key, value) in ws_config.to_mapping() {
        config_map.insert(key, value);
    }
    std::fs::write(dir.join(&config_rel), meta::serialize_mapping(&config_map, meta_format)?)?;

    let author_note = author.as_deref().map(|a| format!(", author {a}")).unwrap_or_default();
    let details = format!(
        "root: {root_name} — {title}{author_note}\n\
         config: {config_name} — metadata {}, content {}, link style {}, identity {}",
        meta.label(),
        content.label(),
        ws_config.link_format.as_config_str(),
        identity.label(),
    );
    let next = format!("next: colophon new <path> --parent {root_name}");
    if prompting {
        cliclack::outro(format!("initialized {}\n{details}\n{next}", dir.display()))?;
    } else {
        println!("initialized {}", dir.display());
        for line in details.lines() {
            println!("  {line}");
        }
        println!("{next}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Prompt for the metadata format, offering only the formats compiled into this
/// binary (the same set `--meta` and `--format` accept).
fn prompt_meta_format() -> std::io::Result<MetaFormat> {
    let mut select = cliclack::select("Metadata format")
        .initial_value(MetaFormat::Yaml)
        .item(MetaFormat::Yaml, "YAML", "--- frontmatter");
    #[cfg(feature = "json")]
    {
        select = select.item(MetaFormat::Json, "JSON", ";;; frontmatter");
    }
    #[cfg(feature = "fig-lang")]
    {
        select = select.item(MetaFormat::Fig, "fig", "```fig block");
    }
    select.interact()
}

fn cmd_show(file: &Path) -> CmdResult {
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

    let children = set.children(&doc.meta);
    if let Some(spanning) = set.spanning_relation() {
        println!("  {spanning} ({} children):", children.len());
        for child in &children {
            println!("    - {child}");
        }
    }

    // Overlay relations (everything that isn't the spanning tree), grouped and
    // printed in the vocabulary's declared order.
    let spanning = set.spanning_relation();
    let edges = set.edges(&doc.meta);
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

fn cmd_links(file: &Path, relation: Option<&str>) -> CmdResult {
    let (_, doc) = load(file)?;
    for edge in relation_set().edges(&doc.meta) {
        if relation.is_none_or(|want| want == edge.relation) {
            println!("{}\t{}", edge.relation, edge.target);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_meta(file: &Path, format: Option<MetaFormat>) -> CmdResult {
    let (_, doc) = load(file)?;
    let Some(mapping) = doc.meta.as_mapping() else {
        return Err("document has no embedded metadata".into());
    };
    // Default to the format the document already uses.
    let format = format.map(Format::from).unwrap_or_else(|| {
        doc.carrier.map(|c| c.format()).unwrap_or(Format::Yaml)
    });
    print!("{}", meta::serialize_mapping(mapping, format)?);
    Ok(ExitCode::SUCCESS)
}

fn cmd_get(file: &Path, key: &str) -> CmdResult {
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

fn cmd_body(file: &Path) -> CmdResult {
    let (_, doc) = load(file)?;
    print!("{}", doc.body);
    Ok(ExitCode::SUCCESS)
}

fn cmd_render(file: &Path) -> CmdResult {
    let (_, doc) = load(file)?;
    let format = colophon::ContentFormat::from_extension(file).ok_or_else(|| {
        format!(
            "{}: not a recognized body format (expected .md/.markdown or .dj/.djot)",
            file.display()
        )
    })?;
    let html = colophon::render_html(&doc.body, format)?;
    print!("{html}");
    Ok(ExitCode::SUCCESS)
}

fn cmd_set(file: &Path, key: &str, value: &str) -> CmdResult {
    let (text, doc) = load(file)?;
    let updated = edit::set_in_text(&text, doc.carrier, key, edit::infer_scalar(value))?;
    std::fs::write(file, updated)?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_unset(file: &Path, key: &str) -> CmdResult {
    let (text, doc) = load(file)?;
    let updated = edit::unset_in_text(&text, doc.carrier, key)?;
    std::fs::write(file, updated)?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_tree(root: Option<&Path>) -> CmdResult {
    let ctx = find_root()?;
    let root = match root {
        Some(r) => ws_rel(&ctx, r)?,
        None => ctx.root_doc.clone(),
    };
    let node = block_on(workspace(&ctx)?.tree(&root))?;
    print_node(&node, "", true, true);
    Ok(ExitCode::SUCCESS)
}

/// Render one tree node: `path — title (marker)`, then its children with
/// box-drawing connectors.
fn print_node(node: &Node, prefix: &str, is_last: bool, is_root: bool) {
    let connector = if is_root {
        String::new()
    } else {
        format!("{prefix}{}", if is_last { "└── " } else { "├── " })
    };
    let name = node
        .title
        .as_deref()
        .or(node.label.as_deref())
        .map(|t| format!("{} — {t}", node.path.display()))
        .unwrap_or_else(|| node.path.display().to_string());
    let marker = match &node.kind {
        NodeKind::Doc => String::new(),
        NodeKind::Missing => " (missing)".to_string(),
        NodeKind::Cycle => " (cycle!)".to_string(),
        NodeKind::Unreadable(e) => format!(" (unreadable: {e})"),
        NodeKind::UnresolvedId(id) => format!(" (unresolved id: {id})"),
    };
    println!("{connector}{name}{marker}");
    let child_prefix = if is_root {
        String::new()
    } else {
        format!("{prefix}{}", if is_last { "    " } else { "│   " })
    };
    for (i, child) in node.children.iter().enumerate() {
        print_node(child, &child_prefix, i + 1 == node.children.len(), false);
    }
}

fn cmd_check(root: Option<&Path>, fix: bool) -> CmdResult {
    let ctx = find_root()?;
    let root = match root {
        Some(r) => ws_rel(&ctx, r)?,
        None => ctx.root_doc.clone(),
    };
    let mut ws = workspace(&ctx)?;
    let findings = block_on(ws.check(&root))?;
    if fix {
        return cmd_check_fix(&mut ws, &findings);
    }
    for finding in &findings {
        println!("{finding}");
    }
    if findings.is_empty() {
        println!("ok: no findings");
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("{} finding(s)", findings.len());
        Ok(ExitCode::FAILURE)
    }
}

/// Interactive repair: walk the findings, and for each one that has a safe,
/// metadata-only fix, show it and ask before applying. Findings with no fix are
/// printed as needing attention. `suggest_fix` is consulted lazily, so a fix
/// applied to one document correctly declines a now-stale finding later.
fn cmd_check_fix(
    ws: &mut Workspace<StdFs, Minter, FileIndex>,
    findings: &[colophon::Finding],
) -> CmdResult {
    let mut applied = 0usize;
    let mut needs_attention = 0usize;
    let mut apply_all = false;
    for finding in findings {
        let Some(fix) = block_on(ws.suggest_fix(finding))? else {
            println!("•  {finding}");
            needs_attention += 1;
            continue;
        };
        println!("⚑  {finding}");
        println!("   → {fix}");
        let apply = apply_all
            || match prompt("   apply? [y]es / [n]o / [a]ll / [q]uit: ")?.as_str() {
                "a" | "all" => {
                    apply_all = true;
                    true
                }
                "y" | "yes" => true,
                "q" | "quit" => {
                    println!("stopped; {applied} fix(es) applied");
                    return Ok(ExitCode::SUCCESS);
                }
                _ => false,
            };
        if apply {
            block_on(ws.apply_fix(&fix))?;
            applied += 1;
        }
    }
    println!("applied {applied} fix(es); {needs_attention} finding(s) need attention");
    Ok(ExitCode::SUCCESS)
}

/// Prompt on stderr, read a trimmed, lowercased line from stdin (EOF → empty).
fn prompt(message: &str) -> Result<String, AnyError> {
    use std::io::Write;
    eprint!("{message}");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_lowercase())
}

fn cmd_new(path: &Path, parent: &Path) -> CmdResult {
    let mut ctx = find_root()?;
    // Authoring id links (or an eager policy) mints IDs, so ensure a registry to
    // persist them exists *before* the workspace is built over it.
    let mints = (ctx.config.id_links && ctx.config.identity.fires_on(Trigger::Link))
        || ctx.config.identity.fires_on(Trigger::Create);
    if mints {
        ensure_registry(&mut ctx)?;
    }
    let mut ws = workspace(&ctx)?;
    block_on(ws.create(&ws_rel(&ctx, path)?, &ws_rel(&ctx, parent)?))?;
    save_index(&ctx, &mut ws)?;
    println!("created {} (in {})", path.display(), parent.display());
    Ok(ExitCode::SUCCESS)
}

fn cmd_mv(from: &Path, to: &Path) -> CmdResult {
    let ctx = find_root()?;
    let mut ws = workspace(&ctx)?;
    block_on(ws.rename(&ws_rel(&ctx, from)?, &ws_rel(&ctx, to)?))?;
    save_index(&ctx, &mut ws)?;
    println!("moved {} -> {}", from.display(), to.display());
    Ok(ExitCode::SUCCESS)
}

fn cmd_rm(path: &Path, force: bool) -> CmdResult {
    let ctx = find_root()?;
    let mut ws = workspace(&ctx)?;
    let danglers = block_on(ws.delete(&ws_rel(&ctx, path)?, force))?;
    save_index(&ctx, &mut ws)?;
    println!("deleted {}", path.display());
    for finding in &danglers {
        eprintln!("warning: now dangling — {finding}");
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_id(file: &Path) -> CmdResult {
    let mut ctx = find_root()?;
    if !ctx.config.identity.fires_on(Trigger::Link) {
        return Err("identity is off in this workspace's config \
             (run `colophon config identity lazy` to enable stable IDs)"
            .into());
    }
    ensure_registry(&mut ctx)?;
    let mut ws = workspace(&ctx)?;
    let id = block_on(ws.register(&ws_rel(&ctx, file)?, Trigger::Link))?;
    save_index(&ctx, &mut ws)?;
    println!("{}", link::id_target(&id));
    Ok(ExitCode::SUCCESS)
}

fn cmd_config(key: Option<&str>, value: Option<&str>) -> CmdResult {
    let ctx = find_root()?;
    match (key, value) {
        // No key: print the effective config (defaults + root + config document).
        (None, _) => {
            print!("{}", meta::serialize_mapping(&ctx.config.to_mapping(), Format::Yaml)?);
        }
        // Key only: read that value from the linked config document.
        (Some(key), None) => {
            let ws = workspace(&ctx)?;
            match block_on(ws.config_get(&ctx.root_doc, key))? {
                Some(v) => match v.as_str() {
                    Some(s) => println!("{s}"),
                    None => println!("{}", meta::serialize_value(&v, Format::Yaml)?.trim_end()),
                },
                None => {
                    eprintln!("colophon: {key} is not set");
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        // Key + value: materialize/link the config document if needed, then set.
        (Some(key), Some(value)) => {
            let mut ctx = ctx;
            let config_doc = ensure_config(&mut ctx)?;
            let full = ctx.root_dir.join(&config_doc);
            let text = std::fs::read_to_string(&full)?;
            let doc = Document::parse(&config_doc, &text)?;
            let updated = edit::set_in_text(&text, doc.carrier, key, edit::infer_scalar(value))?;
            std::fs::write(&full, updated)?;
            println!("set {key} = {value} in {}", config_doc.display());
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Ensure the workspace *declares* a config document, bootstrapping one when it
/// does not: create `colophon.<ext>` (in the workspace's metadata format) beside
/// the root (self-described with a title and a `part_of` back to the root, in
/// the workspace link style) and add the `config` pointer to the root's
/// metadata. Returns its path relative to the root. Mirrors [`ensure_registry`].
fn ensure_config(ctx: &mut Ctx) -> Result<PathBuf, AnyError> {
    let ws = workspace(ctx)?;
    if let Some(existing) = block_on(ws.config_path(&ctx.root_doc))? {
        return Ok(existing);
    }
    let format = ctx.config.default_embed_format;
    let config_name = sidecar_name(CONFIG_STEM, format);
    let config_rel = PathBuf::from(&config_name);
    let config_full = ctx.root_dir.join(&config_rel);
    if !config_full.exists() {
        // The root's title (or a title from its filename) labels the back-link.
        let root_full = ctx.root_dir.join(&ctx.root_doc);
        let root_title = std::fs::read_to_string(&root_full)
            .ok()
            .and_then(|t| Document::parse(&ctx.root_doc, &t).ok())
            .and_then(|d| d.meta.get("title").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| colophon::path_to_title(&ctx.root_doc));
        let part_of =
            colophon::format_link(ctx.config.link_format, &config_rel, &ctx.root_doc, &root_title);
        let mut seed = colophon::Mapping::new();
        seed.insert("title".into(), Value::String("colophon config".into()));
        seed.insert("part_of".into(), Value::String(part_of));
        std::fs::write(&config_full, meta::serialize_mapping(&seed, format)?)?;
    }
    // Link it from the root via the `config` relation.
    let root_full = ctx.root_dir.join(&ctx.root_doc);
    let text = std::fs::read_to_string(&root_full)?;
    let doc = Document::parse(&ctx.root_doc, &text)?;
    let updated = edit::set_in_text(&text, doc.carrier, "config", edit::infer_scalar(&config_name))?;
    std::fs::write(&root_full, updated)?;
    eprintln!("initialized {} (linked from {})", config_rel.display(), ctx.root_doc.display());
    Ok(config_rel)
}

fn cmd_backlinks(file: &Path) -> CmdResult {
    let ctx = find_root()?;
    let target = ws_rel(&ctx, file)?;
    let links = block_on(workspace(&ctx)?.backlinks_to(&ctx.root_doc, &target))?;
    for backlink in &links {
        let kind = if backlink.by_id { "id" } else { "path" };
        println!("{}\t{}\t{kind}", backlink.source.display(), backlink.site);
    }
    if links.is_empty() {
        eprintln!("no backlinks to {}", target.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_resolve(id: &str) -> CmdResult {
    let ctx = find_root()?;
    let ws = workspace(&ctx)?;
    let id = Id(id.strip_prefix(link::ID_SCHEME).unwrap_or(id).to_string());
    match ws.index().resolve(&id) {
        Some(path) => {
            println!("{}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        None if ws.index().is_tombstoned(&id) => {
            eprintln!("colophon: {id} is tombstoned — its document was deleted");
            Ok(ExitCode::FAILURE)
        }
        None => {
            eprintln!("colophon: {id} is not in this workspace's registry");
            Ok(ExitCode::FAILURE)
        }
    }
}
