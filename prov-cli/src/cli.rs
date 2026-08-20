//! The command-line surface: the `clap` argument grammar and the enums that
//! mirror the library's config axes.
//!
//! Every type here is a CLI *spelling* of a library concept — a `--layout` flag
//! that maps to [`prov::Layout`], a `--reference` value that maps to
//! [`prov::Addressing`], and so on — kept in one module so the argument
//! grammar is the CLI's business and the library enums stay free of `clap`. The
//! command *handlers* live elsewhere (`main.rs` and its sibling modules); this is
//! only the shape of what the user types.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use prov::{
    Addressing, ContentFormat, EmbedStyle, Format, IdStorage, Layout, LinkStyle, Notation,
    Registration, RelationStyleConfig, WorkspaceConfig, Wrapper,
};

/// `--layout` — the CLI mirror of [`Layout`], so the flag's spelling is the
/// CLI's business and the library enum stays free of clap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum LayoutArg {
    /// A directory per route segment, each holding an `index` node.
    Nested,
    /// Every synthesized node beside the start document.
    Flat,
}

impl From<LayoutArg> for Layout {
    fn from(arg: LayoutArg) -> Self {
        match arg {
            LayoutArg::Nested => Layout::Nested,
            LayoutArg::Flat => Layout::Flat,
        }
    }
}

/// The filename stem of the registry document the CLI creates on first
/// `prov id` — visible, beside the root, and *linked from the root's own
/// metadata* via the `registry` relation. Its extension is the workspace's
/// metadata format (see [`sidecar_name`]). Where the registry lives is a fact
/// about the workspace, declared in it; the CLI only supplies this default when
/// bootstrapping one. (It can equally be a `.md` file whose frontmatter carries
/// the records — anything the pointer targets.)
pub(crate) const REGISTRY_STEM: &str = "registry";

/// The filename stem of the config document the CLI creates on first
/// `prov config <k> <v>` (or at `init`) — beside the root, linked via the
/// `config` relation (the reachability move the registry uses). Workspace policy
/// lives here rather than bloating the root or hiding in a dotfile.
pub(crate) const CONFIG_STEM: &str = "prov";

/// The whole-file extension for a metadata format: the config and registry
/// sidecars are written in the workspace's *chosen metadata format*, not always
/// YAML — `yaml`/`json`/`figl`. Mirrors [`prov::document::whole_file_format`],
/// which parses them back.
pub(crate) fn sidecar_ext(format: Format) -> &'static str {
    match format {
        #[cfg(feature = "json")]
        Format::Json => "json",
        #[cfg(feature = "toml")]
        Format::Toml => "toml",
        #[cfg(feature = "fig-lang")]
        Format::Fig => "figl",
        _ => "yaml",
    }
}

/// The sidecar filename for `stem` in metadata `format` (e.g. `prov.figl`).
pub(crate) fn sidecar_name(stem: &str, format: Format) -> String {
    format!("{stem}.{}", sidecar_ext(format))
}

/// What `prov --version` prints.
///
/// The package version on a clean build of the release tag, and the package
/// version plus the commit it was built from on anything else — see
/// `build.rs`, which resolves the git state and emits `PROV_VERSION`. The
/// parenthetical is how a bug report distinguishes a dev build from the
/// release that shares its version number.
pub(crate) const VERSION: &str = env!("PROV_VERSION");

/// A self-describing plaintext workspace, from the command line.
#[derive(Parser)]
#[command(name = "prov", version = VERSION, about, long_about = None)]
pub(crate) struct Cli {
    /// Run as if started in this directory — discover the workspace root from
    /// here instead of the current directory (like `git -C`, this goes *before*
    /// the subcommand: `prov -C ~/vault check`). Also settable via the `PROV_ROOT`
    /// environment variable; the flag wins. Lets a script or cron operate on a
    /// vault without `cd`-ing into it. Relative path arguments resolve here too.
    #[arg(short = 'C', long = "root", value_name = "DIR")]
    pub(crate) root: Option<PathBuf>,
    /// Keep this device's fixity cache here instead of the default location
    /// (`prov cache` prints the file in use). Also settable via
    /// `PROV_CACHE_DIR`, or `XDG_CACHE_HOME`; the flag wins. The cache lets
    /// `history-capture` skip reading and hashing files whose timestamp and size
    /// say they have not changed — it is disposable, lives outside the
    /// workspace, and deleting it costs one slow capture.
    #[arg(long = "cache-dir", value_name = "DIR")]
    pub(crate) cache_dir: Option<PathBuf>,
    /// Ignore the fixity cache: read and hash every file, and remember nothing.
    /// For reproducing a capture from scratch, or for not writing to disk on a
    /// machine you would rather leave no trace on.
    ///
    /// Not needed for integrity: `check` never consults the cache in the first
    /// place, since bit-rot is precisely the change a timestamp cannot see.
    #[arg(long = "no-cache")]
    pub(crate) no_cache: bool,
    /// Read this device's map of other workspaces from here instead of the
    /// default location (`prov peer list` prints the file in use). Also settable
    /// via `PROV_PEERS`, or `XDG_CONFIG_HOME`; the flag wins. The map says where
    /// a workspace named by a cross-workspace reference (`id:<name>/<id>`)
    /// lives; it is device-local by design, since the same archive is read from
    /// different machines.
    #[arg(long = "peers", value_name = "FILE")]
    pub(crate) peers: Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// What `prov peer` is being asked to do.
#[derive(Subcommand)]
pub(crate) enum PeerAction {
    /// List the workspaces this device can resolve, and the file holding them.
    List,
    /// Record where a workspace lives on this device. NAME must be what that
    /// workspace calls itself (its `workspace_id`) — that is the name every
    /// reference to it will be written with.
    Add {
        /// The workspace's own name.
        name: String,
        /// Its root directory on this device.
        dir: PathBuf,
    },
    /// Forget a workspace. Its references keep working exactly as well as they
    /// did — which is to say they are still carried, just no longer followable.
    Remove {
        /// The workspace's own name.
        name: String,
    },
    /// Resolve a cross-workspace reference (`id:<name>/<id>`) to a file on this
    /// device, by looking the id up in that workspace's own registry. The peer
    /// is asked what it calls itself first: a directory that turns out to be a
    /// different workspace is reported, never followed.
    Resolve {
        /// The reference, with or without the `id:` scheme.
        #[arg(value_name = "REFERENCE")]
        reference: String,
        /// Follow a peer whose name could not be checked — one that is
        /// anonymous, or that could not be opened as a workspace. Never accepts
        /// a peer that calls itself something else; that is not missing
        /// evidence, it is evidence of the wrong archive.
        #[arg(long)]
        unverified: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum Command {
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
        /// Config language for the root's metadata: yaml/toml/json/fig
        /// (default: yaml). `fig` is unavailable with `--embed delimited`.
        #[arg(long, value_enum)]
        meta: Option<MetaFormat>,
        /// How that metadata is embedded: delimited, code-block, html-script,
        /// html-code, or separate. Must suit `--content` (default: the first
        /// style that content grammar offers).
        #[arg(long, value_enum)]
        embed: Option<EmbedArg>,
        /// Body-prose grammar; sets the root file's extension (default: markdown).
        #[arg(long, value_enum)]
        content: Option<ContentLang>,
        /// The syntactic wrapper prov authors references in: markdown
        /// (`[Title](target)`) or wikilink (`[[target]]`) (default: markdown).
        /// The first style axis — pick it, then `--reference` picks the target.
        #[arg(long, value_enum)]
        wrapper: Option<WrapperArg>,
        /// What references address their target by: path, id, alias (by title),
        /// or split (readable `contents` down / durable `part_of` up). `id` and
        /// `split` require `--identity` ≠ off; `alias`/`split` are by-title links
        /// with no markdown form, so the interactive menu offers them only under
        /// `--wrapper wikilink` (default: path).
        #[arg(long, value_enum)]
        reference: Option<ReferenceArg>,
        /// How *path* references are formatted — only used when a target is
        /// addressed by path (default: markdown-root).
        #[arg(long, value_enum)]
        link_style: Option<LinkStyleArg>,
        /// When documents earn a stable ID: off (paths only), lazy (on
        /// link-by-id or publish), or eager (at creation) (default: lazy).
        #[arg(long, value_enum)]
        identity: Option<IdentityArg>,
        /// Where IDs live: frontmatter (stamped into each document's `id` field,
        /// with the registry kept as a cache), registry (only in the registry
        /// document), or frontmatter-only (no registry document — self-describing,
        /// but no tombstones) (default: frontmatter).
        #[arg(long, value_enum)]
        id_storage: Option<IdStorageArg>,
        /// Content-checksum coverage for bit-rot detection: payloads (attachment
        /// files only — the default, frictionless), full (also document bodies,
        /// paired with `prov edit`), or off. Verified by `prov check`.
        #[arg(long, value_enum)]
        fixity: Option<FixityArg>,
        /// Delete straight to a hard delete instead of the recoverable recycle bin
        /// (the recycle bin is on by default — the safe archival posture).
        #[arg(long)]
        no_recycle_bin: bool,
        /// Frontmatter field `prov edit` stamps with an RFC 3339 UTC timestamp
        /// on a content change (e.g. `updated`). Omitted → the feature is off.
        #[arg(long, value_name = "FIELD")]
        updated_field: Option<String>,
        /// What this workspace calls itself, so another workspace can reference
        /// it (`id:<NAME>/<id>`). Omitted → anonymous, which is fine until
        /// something else needs to point here. No `/`, `:` or whitespace.
        #[arg(long, value_name = "NAME")]
        workspace_id: Option<String>,
        /// What to do with content documents already in the directory: `flat`
        /// links each one under the new root; `mirror` folds the folder tree into
        /// the containment tree (each directory becomes a node, synthesizing a
        /// folder index where none exists); `none` leaves them unlinked. Omit to
        /// be asked on a terminal (and to leave them unlinked otherwise).
        #[arg(long, value_enum)]
        adopt: Option<AdoptArg>,
        /// Also give the directory's *non-document* files (images, PDFs, data,
        /// binaries) each a metadata sidecar, linked under the root. Omit to be
        /// asked on a terminal; non-interactive leaves them alone (they stay
        /// invisible to `prov check` until attached).
        #[arg(long)]
        attach: bool,
        /// Accept every default without prompting.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Where the *other* workspaces are — this device's map from a workspace
    /// name to a directory.
    ///
    /// A workspace names itself in its own config (`workspace_id`), and a
    /// reference across workspaces is written `id:<name>/<id>`. What that name
    /// resolves to is a fact about *this machine*, not about the archive, so it
    /// is kept here rather than in `prov.yaml` — the same reasoning that keeps
    /// the fixity cache's location out of the workspace.
    ///
    /// Nothing depends on this map: a cross-workspace reference is carried, left
    /// alone by moves, and never reported broken, whether or not a peer is
    /// recorded. The map only makes such a reference *followable*.
    Peer {
        #[command(subcommand)]
        action: PeerAction,
    },
    /// Summarize a document: its metadata, spanning children, and declared links.
    Show {
        /// Path to a document (plaintext with embedded metadata).
        #[arg(value_name = "TARGET")]
        file: String,
    },
    /// List a document's links as `relation<TAB>target`, one per line.
    Links {
        /// Path to a document.
        #[arg(value_name = "TARGET")]
        file: String,
        /// Only show links declared by this relation (e.g. `contents`).
        #[arg(long)]
        relation: Option<String>,
    },
    /// Print a document's metadata block (without fences).
    Meta {
        /// Path to a document.
        #[arg(value_name = "TARGET")]
        file: String,
        /// Output format (default: the format the document already uses).
        #[arg(long, value_enum)]
        format: Option<MetaFormat>,
    },
    /// Print one metadata field by dotted path (e.g. `title`, `contents.0`).
    Get {
        /// Path to a document.
        #[arg(value_name = "TARGET")]
        file: String,
        /// Dotted key path; an all-digit segment indexes a sequence.
        key: String,
    },
    /// Print a document's body (everything outside the metadata block).
    Body {
        /// Path to a document.
        #[arg(value_name = "TARGET")]
        file: String,
    },
    /// Render a document's body to HTML (Markdown/Djot, via `twig`).
    Render {
        /// Path to a document.
        #[arg(value_name = "TARGET")]
        file: String,
    },
    /// Open a document in `$EDITOR` and, on save, recompute its content checksum
    /// (under the `full` fixity tier) so a body edit keeps its fixity true rather
    /// than becoming a `check` finding. The prov-mediated edit path.
    Edit {
        /// The document to edit: a path, a title route (`@Daily/2026/07`), or an
        /// id (`id:fpk38j`).
        #[arg(value_name = "TARGET")]
        file: String,
    },
    /// Set a metadata field (comment- and format-preserving; creates the
    /// block when the document has none).
    Set {
        /// Path to a document.
        #[arg(value_name = "TARGET")]
        file: String,
        /// Dotted key path.
        key: String,
        /// Value; `true`/`false`, integers, floats, and `null` are typed,
        /// everything else is a string.
        value: String,
    },
    /// Remove a metadata field (comment- and format-preserving).
    Unset {
        /// Path to a document.
        #[arg(value_name = "TARGET")]
        file: String,
        /// Dotted key path.
        key: String,
    },
    /// List the views this workspace declares, or execute one.
    ///
    /// A view is the second way through the same documents the containment tree
    /// already holds — "the entries under Daily, by month". With no NAME, print
    /// what the workspace declares; with one, print its groups and the
    /// documents under each.
    Views {
        /// The view to execute (default: list every declared view).
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
    /// List the exports this workspace declares, or preview one.
    ///
    /// An export is a named, closed-by-default set of documents that may leave
    /// the workspace: a document is in it only if it itself declares the
    /// export's gate value, and a document that declares nothing leaves in
    /// nothing. With no NAME, print what the workspace declares; with one,
    /// print the plan — what leaves, what the gate held back, what the view
    /// scoped out. A preview moves nothing.
    Exports {
        /// The export to preview (default: list every declared export).
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
    /// Print the containment tree that unfolds from a root document.
    Tree {
        /// The document to discover from (default: the workspace root).
        #[arg(value_name = "TARGET")]
        root: Option<String>,
    },
    /// Interactively explore the workspace: view a document and follow any of its
    /// links — or its backlinks — moving through the graph from the terminal.
    Explore {
        /// The document to start from (default: the workspace root).
        file: Option<PathBuf>,
    },
    /// Check workspace integrity from a root: broken links, case mismatches,
    /// duplicate containment, missing inverse links, dangling IDs. Exits 1 on
    /// findings.
    Check {
        /// The document to check from (default: the workspace root).
        #[arg(value_name = "TARGET")]
        root: Option<String>,
        /// Repair findings. Bare `--fix` (or `--fix ask`) walks them and offers
        /// each finding's repairs to choose from; `--fix mechanical` applies only
        /// the repairs that are pure functions of an authority — no prompts, for
        /// scripts. Structure edits only: prose is rewritten solely where the
        /// parser itself reported a link, so code that looks like one is never
        /// touched, and no fix ever deletes a file.
        #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "ask")]
        fix: Option<FixModeArg>,
        /// Report only the findings lodged against this document — the ones
        /// whose repair rewrites it, or that name it as the file to go and look
        /// at. A filter on the *results*, not on the walk: the whole workspace
        /// is still checked, because the findings that matter most about one
        /// document (nothing links to it, its parent dropped it, an inbound
        /// label went stale) are only visible from the graph. Checking *from*
        /// the document instead is the positional argument, and cannot see
        /// them.
        #[arg(long, value_name = "TARGET")]
        only: Option<String>,
        /// Print findings to stdout as a JSON array instead of one line each —
        /// `kind`, `subject`, the human `message`, and the finding's own
        /// fields. Empty findings print `[]`, so "clean" and "no output" stay
        /// distinguishable.
        ///
        /// Nothing is written to stderr in this mode: the count line is
        /// narration for a person, and the array already says how many it holds.
        /// The exit code is unchanged, though — findings still exit non-zero, so
        /// `check` keeps working as a CI gate. In a shell that aborts a pipeline
        /// on a non-zero exit (nushell), capture the status instead of piping
        /// through it: `(prov check --json | complete).stdout | from json`.
        ///
        /// Not available with `--fix`, whose stdout means something else (the
        /// findings a repair introduced).
        #[arg(long, conflicts_with = "fix")]
        json: bool,
    },
    /// Record that a document changed **outside prov** — restamp its content
    /// checksum, and stamp the workspace's `updated` field with the current
    /// time. What `prov edit` does automatically for an edit it hosted, for the
    /// edits it did not: another editor, a sync, a script.
    ///
    /// `check --fix` is not this. It offers to re-stamp the checksum (as a
    /// judgment, so `--fix mechanical` skips it), but it never writes
    /// `updated` — nothing on disk tells it when the edit happened — so it
    /// repairs the hash while erasing the only remaining sign the file changed.
    ///
    /// Idempotent: with a checksum on record, the stamps land only when the
    /// bytes actually drifted, so re-running changes nothing and this is safe
    /// in a sync hook.
    ///
    /// Refuses a manifest node by name: its checksum covers the manifest
    /// document, rebuilding which means re-reading every file it lists —
    /// `prov manifest <target> --update` is the verb that pays that cost on
    /// purpose. `--all` skips a manifest node it meets rather than paying it
    /// unasked.
    Stamp {
        /// The document to stamp. An attachment's payload is covered through
        /// its sidecar, so either handle works. Omit with `--all`.
        #[arg(value_name = "TARGET")]
        target: Option<String>,
        /// Bring the whole workspace's fixity up to date: correct every
        /// checksum whose bytes have drifted, and seed one for every document
        /// that never had one.
        ///
        /// It writes `updated` only for the documents that actually drifted. A
        /// checksum restates the bytes, so it is owed wherever it is missing or
        /// wrong; a timestamp claims an edit happened, and a sweep across a
        /// workspace it merely read has no evidence for that. Naming a single
        /// target is that evidence — which is the one way `--all` differs from
        /// running this per file.
        #[arg(long, conflicts_with = "target")]
        all: bool,
        /// Restamp the checksum but leave the timestamp alone — for keeping
        /// fixity true without claiming an edit time.
        #[arg(long)]
        no_timestamp: bool,
        /// Print what would be stamped, and write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Create a document as a child of a parent, linking both directions. The
    /// positional is the new document's **title** — prov derives a readable
    /// filename from it (a slug plus the workspace's content extension) in the
    /// parent's directory, and records the title in the document's metadata,
    /// where structure lives. Override the derived filename with `--as` (an exact
    /// path) or just its extension with `--ext`.
    New {
        /// Title of the new document (recorded in its metadata; a readable
        /// filename is slugged from it unless `--as` overrides).
        title: String,
        /// The parent document that gains a spanning link to the new one: a path
        /// (`daily.md`), a title route (`@Daily/2026/07`), or an id
        /// (`id:fpk38j`). A route's missing segments are an error unless `-p`.
        #[arg(long = "in", short = 'i', value_name = "TARGET")]
        in_target: String,
        /// `mkdir -p` for containment — idempotent creation. Creates any missing
        /// route segments (when `--in` is a route), *and* treats an
        /// already-existing leaf (a same-titled child) as a no-op instead of an
        /// error. Safe to re-run — a daily-note cron can call the same command
        /// every day. A path held by a *different*-titled document still errors.
        #[arg(long = "parents", short = 'p', requires = "in_target")]
        parents: bool,
        /// Where `-p` writes the nodes it creates: `nested` (a directory per
        /// segment, `daily/2026/index.md`) or `flat` (all beside the start,
        /// `daily.md`, `2026.md`). File placement only — containment is the links
        /// either way (default: nested).
        #[arg(long, value_enum, default_value_t = LayoutArg::Nested, requires = "parents")]
        layout: LayoutArg,
        /// Print what `--in` resolves to and what `-p` would create, then stop.
        #[arg(long, requires = "in_target")]
        dry_run: bool,
        /// Use this exact workspace path instead of a title-derived name (the
        /// title is still taken from the positional). Wins over `--ext`.
        #[arg(long = "as")]
        as_path: Option<PathBuf>,
        /// Override just the derived filename's extension (e.g. `djot`, `yaml`);
        /// ignored under `--as`. Default: the workspace's content format.
        #[arg(long)]
        ext: Option<String>,
    },
    /// Give an arbitrary file (an image, a PDF, any binary) workspace-linked
    /// metadata: write a sidecar `<file>.yaml` beside it carrying its title,
    /// links, and any ID, and link it as a child of a parent. The file's bytes
    /// are never read or rewritten — only linked, moved, and validated with it.
    Attach {
        /// The file to attach. Anything prov can't read as a document; a
        /// readable document should be created with `new` (it carries its own
        /// metadata) rather than shadowed by a sidecar — unless you mean it, see
        /// `--opaque`. Omit with `--all`.
        payload: Option<PathBuf>,
        /// The parent that gains a spanning link to the attachment (default: the
        /// workspace root): a path (`daily.md`), a title route
        /// (`@Daily/2026/07`), or an id (`id:fpk38j`).
        #[arg(long = "in", short = 'i', value_name = "TARGET")]
        in_target: Option<String>,
        /// Create any route segments that don't exist yet — `mkdir -p` for
        /// containment. Only meaningful when `--in` is a route.
        #[arg(long = "parents", short = 'p', requires = "in_target")]
        parents: bool,
        /// Where `-p` writes the nodes it creates. File placement only.
        #[arg(long, value_enum, default_value_t = LayoutArg::Nested, requires = "parents")]
        layout: LayoutArg,
        /// Attach a file prov *can* read as a document, shadowing it: prov links,
        /// moves and checksums it but never reads it — its title stays out of
        /// alias resolution and any `id` it shows stays out of the registry. For a
        /// specimen: an example document, a fixture, a captured export, whose
        /// metadata block is an exhibit rather than a claim about this workspace.
        /// `adopt` would instead write a link into that block, editing it.
        #[arg(long, conflicts_with = "all")]
        opaque: bool,
        /// Attach every loose file under the workspace — each opaque file that
        /// has no sidecar yet — instead of a single payload. Bounded to the
        /// directories the workspace already reaches (an unlinked subtree, a
        /// nested workspace, is left alone); pass `--recursive` to sweep the whole
        /// tree. Mutually exclusive with a positional file.
        #[arg(long)]
        all: bool,
        /// With `--all`, descend into every directory, including ones nothing
        /// links to yet — the full recursive sweep rather than the reachability-
        /// bounded default.
        #[arg(long)]
        recursive: bool,
        /// Treat the positional as a *directory* and cover it with a manifest:
        /// one node and one list of every opaque file under it, instead of one
        /// sidecar per file. For an archive — ten thousand photographs — where a
        /// sidecar each is not a workspace anyone can read.
        #[arg(long, conflicts_with_all = ["all", "opaque"])]
        manifest: bool,
        /// With `--manifest`, list the files without checksumming them: an
        /// inventory rather than a fixity baseline. Hashing reads every file,
        /// now and at each refresh, which is a real cost over an archive.
        #[arg(long = "no-hash", requires = "manifest")]
        no_hash: bool,
    },
    /// Show, refresh or deeply verify the manifest covering a directory — the
    /// bulk attachment minted by `attach --manifest`.
    ///
    /// Bare, it reports what the manifest says and whether the directory still
    /// agrees with it, reading no covered file. `--update` rebuilds the list
    /// from the directory as it is now (and re-stamps the node that pins it);
    /// `--verify` re-reads every listed file and compares its checksum, which is
    /// the pass `check` deliberately leaves out because it costs a full read of
    /// the archive.
    Manifest {
        /// The covered directory, or the node/manifest document that describes
        /// it — whichever you have to hand.
        #[arg(value_name = "TARGET")]
        target: PathBuf,
        /// Rebuild the list from the directory: record files that appeared, drop
        /// rows whose file is gone, and re-checksum what is there. Accepts the
        /// directory as it stands, so a file you have *lost* is written out of
        /// the record — which is why it is never automatic.
        #[arg(long)]
        update: bool,
        /// Re-read every listed file and compare its checksum against the
        /// manifest — the deep integrity pass over the archive.
        #[arg(long)]
        verify: bool,
    },
    /// Move/rename a document, maintaining every affected link: every inbound
    /// reference across the workspace (parent entry, children's inverses,
    /// overlay links, body wikilinks) and the document's own relative links.
    ///
    /// Moves the *file* and preserves the document's place in the tree. To change
    /// its place in the tree instead, see `reparent` — or pass `--in`
    /// here to do both at once.
    Mv {
        /// The document to move: a path, a title route (`@Daily/2026/07`), or an
        /// id (`id:fpk38j`).
        #[arg(value_name = "TARGET")]
        from: String,
        /// New path.
        to: PathBuf,
        /// Also reparent under this document — the file moves *and* changes
        /// parent. A path, a title route (`@Daily/2026/08`), or an id.
        #[arg(long = "in", short = 'i', value_name = "TARGET")]
        in_target: Option<String>,
        /// Create missing route segments (when `--in` is a route), like `mkdir -p`.
        #[arg(long = "parents", short = 'p', requires = "in_target")]
        parents: bool,
        /// Where `--parents` writes the nodes it synthesizes. Placement only —
        /// never the graph.
        #[arg(long, value_enum, default_value_t = LayoutArg::Nested, requires = "parents")]
        layout: LayoutArg,
    },
    /// Change a document's parent in the containment tree, leaving the file where
    /// it is.
    ///
    /// The complement of `mv`: `mv` changes a document's path and preserves its
    /// place in the tree; `reparent` changes its place in the tree and preserves
    /// its path. Containment is link-shaped, not directory-shaped, so a node may
    /// live in any directory — moving the file is a separate decision (`mv`, or
    /// `mv --in` to do both).
    ///
    /// The old parent's entry is removed and the new one's added, so the document
    /// is never contained twice. An unparented document is accepted: there is
    /// nothing to remove, so this simply links it in.
    ///
    /// The two directions are judged separately, so a document that already
    /// claims this parent while the parent does not list it — the state most
    /// orphans are actually in — gets the missing entry written rather than a
    /// success message and no change. It says which of the three happened: it
    /// moved, it linked, or both directions already held and nothing was
    /// written. An old parent that is no longer on disk is not an error either;
    /// there is simply no entry to remove.
    Reparent {
        /// The document to reparent: a path, a title route (`@Daily/2026/07`), or
        /// an id (`id:fpk38j`).
        #[arg(value_name = "TARGET")]
        path: String,
        /// The new parent: a path (`daily.md`), a title route
        /// (`@Daily/2026/08`), or an id (`id:fpk38j`).
        #[arg(long = "in", short = 'i', value_name = "TARGET")]
        in_target: String,
        /// Create missing route segments (when `--in` is a route), like `mkdir -p`.
        #[arg(long = "parents", short = 'p', requires = "in_target")]
        parents: bool,
        /// Where `--parents` writes the nodes it synthesizes. Placement only —
        /// never the graph.
        #[arg(long, value_enum, default_value_t = LayoutArg::Nested, requires = "parents")]
        layout: LayoutArg,
        /// Show what the route resolves to without changing anything.
        #[arg(long, requires = "in_target")]
        dry_run: bool,
    },
    /// Delete a document, removing its parent's spanning entry. Refuses when
    /// the document has children unless --force. By default the document is moved
    /// to the workspace recycle bin (recoverable with `restore`); pass `--purge`
    /// for an immediate hard delete. The default is governed by the `recycle_bin`
    /// config axis (on unless opted out).
    Rm {
        /// The document to delete: a path, a title route (`@Daily/2026/07`), or an
        /// id (`id:fpk38j`).
        #[arg(value_name = "TARGET")]
        path: String,
        /// Delete even when the document still contains children (orphans them).
        #[arg(long)]
        force: bool,
        /// Hard-delete: destroy the document instead of moving it to the recycle
        /// bin. Irreversible.
        #[arg(long)]
        purge: bool,
    },
    /// Restore a document from the recycle bin to the path it was deleted from,
    /// re-linking it under its original parent.
    ///
    /// Refuses when something already occupies that path, or when the record's
    /// id has been claimed in the meantime — by another document, or by another
    /// id at that path. Ids travel in frontmatter, so a sync can hand one to a
    /// second document while this one sits in the bin; restoring over that would
    /// take the id from a document that still spells it, and only you can say
    /// which should keep it.
    Restore {
        /// The original path of a binned document (as listed in the recycle bin).
        #[arg(value_name = "PATH")]
        path: String,
    },
    /// Permanently purge every document in the recycle bin. Irreversible; the
    /// only hard delete of binned documents.
    EmptyBin,
    /// Convert a document along a config axis. Five axes are supported.
    /// Two restyle the document's own outbound path links: `notation` (how a
    /// target is wrapped — `markdown` `[Title](target)` or `bare` `target`) and
    /// `path_style` (how the path itself is written — `root` / `relative` /
    /// only the spelling changes; each link's destination, label,
    /// and wrapper are preserved, and id/external/alias targets are left untouched.
    /// Two rewrite the metadata block: `metadata.format` re-emits the
    /// frontmatter in a different language (`yaml` / `json` / `toml` / `fig`),
    /// keeping its embedding shape; `metadata.embed` re-emits it in a different
    /// shape (`delimited` / `code_block` / `html_script` / `html_code`), keeping its
    /// language — so a `delimited` block can become a code block that can then hold
    /// fig. Both preserve every value (comments do not survive a block rewrite).
    /// The fifth, `content_format` (`markdown` / `djot` / `html`), transcodes the
    /// body prose — and, because a body's grammar is declared by its file
    /// extension, renames the file (`notes.md` → `notes.dj`) and retargets every
    /// inbound link to it. Converting to or from `html` is lossy and needs `-f`.
    /// Per file by default (DESIGN §8) — a document's spelling is its own to
    /// declare; `-r` also converts this file's spanning subtree.
    Convert {
        /// The document to convert.
        #[arg(value_name = "TARGET")]
        file: String,
        /// The config axis to convert: `notation`, `path_style`, `metadata.format`,
        /// `metadata.embed`, or `content_format`.
        axis: String,
        /// The target value (e.g. `bare` for `notation`, `relative` for
        /// `path_style`, `json` for `metadata.format`, `code_block` for
        /// `metadata.embed`, `djot` for `content_format`).
        value: String,
        /// Also convert every document in this file's spanning subtree.
        #[arg(long, short)]
        recursive: bool,
        /// Allow a lossy conversion — converting body prose to or from `html`,
        /// where the authored markup does not survive the round trip.
        #[arg(long, short)]
        force: bool,
    },
    /// Duplicate a document as a fresh sibling under the same parent, linking the
    /// copy in both directions. The copy takes the next free `-copy` name and
    /// carries the source's title, body, and metadata — but never its stable ID
    /// (identity is per-document) nor its children (a shallow copy, so no child is
    /// left with two parents). A separated node's body file is copied too.
    #[command(alias = "dup")]
    Duplicate {
        /// The document to duplicate: a path, a title route (`@Daily/2026/07`), or
        /// an id (`id:fpk38j`).
        #[arg(value_name = "TARGET")]
        source: String,
    },
    /// Ensure a document has a stable ID and print its `prov:<id>` target.
    /// Registers it in the workspace's registry document (bootstrapping
    /// registry.yaml + the root's `registry` pointer on first use) — link that
    /// target from any document and it survives moves.
    ///
    /// With `--workspace`, names the *workspace* instead of a document: the
    /// `workspace_id` other archives reference this one by (`id:<name>/<id>`).
    Id {
        /// Path to a document.
        #[arg(value_name = "TARGET", required_unless_present = "workspace")]
        file: Option<String>,
        /// Name this workspace rather than a document, so another workspace can
        /// reference it (`id:<NAME>/<id>`). Pass a NAME to choose one; pass the
        /// flag bare to have prov mint an opaque global one. Either way it is
        /// written to config and printed.
        ///
        /// Idempotent and never destructive: if the workspace is already named,
        /// that name is printed and nothing is written — a name is what other
        /// archives have already written their references with, so *changing*
        /// one is a deliberate `prov config workspace_id <name>`, not a rerun of
        /// this.
        #[arg(
            long = "workspace",
            value_name = "NAME",
            num_args = 0..=1,
            conflicts_with = "file"
        )]
        workspace: Option<Option<String>>,
    },
    /// Resolve a stable ID (with or without the `prov:` prefix) to its
    /// current path.
    Resolve {
        /// The ID to resolve.
        id: String,
    },
    /// List the documents that link to a document (its backlinks), across the
    /// workspace, as `source<TAB>site<TAB>path|id`.
    Backlinks {
        /// The document whose backlinks to list.
        #[arg(value_name = "TARGET")]
        file: String,
    },
    /// Get or set workspace config (e.g. `link_format`, `identity`). With a
    /// value, writes it to the linked config document — creating and linking
    /// `prov.yaml` from the root on first use. With a key only, prints that
    /// value; with no key, prints the effective config.
    Config {
        /// The config key — dotted for nested axes (e.g. `references.notation`,
        /// `identity`). Omit to print the effective config.
        key: Option<String>,
        /// The value to set. Omit to read.
        value: Option<String>,
        /// Materialize the *full* effective config explicitly into the config
        /// document — every setting written out at its current (or default)
        /// value, so nothing relies on invisible defaults. Fills in the keys you
        /// have not set; existing settings and fields are preserved.
        #[arg(long, conflicts_with_all = ["key", "value", "home"])]
        setup: bool,
        /// Relocate the whole workspace policy to one home, preserving what is
        /// declared (no defaults baked in): `sidecar` moves it into `prov.yaml`
        /// and clears the root's `prov:` block ("unclutter my root"); `root`
        /// inlines it into the root's `prov:` block and removes the sidecar ("one
        /// less file"). Reading always spans both homes regardless of where policy
        /// lives.
        #[arg(long, value_name = "root|sidecar", conflicts_with_all = ["key", "value", "setup"])]
        home: Option<ConfigHome>,
    },
    /// Copy the entire workspace tree to another filesystem location, for
    /// redundancy against losing the workspace's own location (a dead disk, a
    /// deleted cloud folder). A plain, opaque, whole-tree copy — bytes
    /// verbatim, hidden files included — with no pointer relation, no
    /// manifest, no config axis: it deliberately depends on nothing living
    /// inside the workspace, which is the whole point of a backup. An
    /// imperative one-off, not a standing behavior.
    Backup {
        /// Where to write the backup: a directory to copy the tree into
        /// (created if missing; parent directories are created as needed; an
        /// *existing* directory here must be empty) — or, with `--zip`, the
        /// path of the zip file to create. Refused if it resolves inside the
        /// workspace root (the copy would recurse into itself).
        #[arg(long = "to", value_name = "PATH")]
        to: PathBuf,
        /// Archive into a single store-only (uncompressed) zip file instead of
        /// copying to a directory.
        #[arg(long)]
        zip: bool,
    },
    /// Regenerate `about.md` — the prose page that tells a reader with no prior
    /// knowledge how to read this directory — from the workspace's own
    /// configuration, and point the root at it.
    ///
    /// The page is the spec *specialized* against this workspace: every rule
    /// resolved to a concrete fact, every branch this workspace does not take
    /// left out. It is written for a person who has the directory and nothing
    /// else, and it is found by its filename rather than by following a link.
    ///
    /// Derived from configuration alone, never from the files, so it is rewritten
    /// when configuration changes and at no other time. A conflicted copy is
    /// always resolved by regenerating rather than merging — nothing in it is a
    /// fact about anything but the config.
    About {
        /// Exit non-zero if the page is missing or does not match what prov
        /// would generate, printing what differs. Writes nothing. For CI in a
        /// repository that wants the page guaranteed current.
        #[arg(long)]
        check: bool,
        /// Write the generated page to stdout, touching nothing.
        #[arg(long, conflicts_with = "check")]
        print: bool,
    },
    /// Capture the workspace into the history store: hash every reachable file,
    /// park any bytes not already stored, and write one immutable event
    /// document recording the complete file set at this moment.
    ///
    /// The safety net for damage an external sync transport does to the
    /// workspace's *structure* — a rename, move or delete touches several files
    /// at once, and a transport reconciling bytes has no idea about prov's
    /// graph. An event is a consistent cut across every file it captured
    /// together, so a later restore puts the whole set back rather than one
    /// file's bytes.
    ///
    /// Adds files only (plus the current month's rebuildable index), so two
    /// devices capturing concurrently never conflict. If nothing has changed
    /// since the newest event, nothing is written.
    ///
    /// Requires `history: manual` in the workspace config. Leave it off when
    /// the transport is git — git already keeps every pre-image.
    HistoryCapture {
        /// A short note recorded on the event and slugged into its id
        /// (`pre-sync`, `nightly`, `pre-migration`). Free-form.
        ///
        /// It lands in a filename that is never rewritten, so keep it short.
        /// The reason for the capture belongs in `--message`.
        #[arg(long, value_name = "TEXT")]
        label: Option<String>,
        /// Why this capture was taken, in as many words as it deserves —
        /// written into the event document's own prose.
        ///
        /// The event id is a digest of the manifest and the label, never of the
        /// body, so a message costs the id nothing and can be as long as you
        /// like. It is also why `--label` stays short: one is a filename, the
        /// other is a note to whoever reads this event later.
        ///
        /// A message cannot make an event on its own. A capture that finds the
        /// workspace unchanged writes nothing, whatever is said about it.
        #[arg(short = 'm', long, value_name = "TEXT")]
        message: Option<String>,
        /// List what a capture would record — and, separately, what it would
        /// not — without writing anything or hashing a byte.
        ///
        /// The second list is the point. A capture set is drawn from the
        /// *reachable* graph, so a file nothing links to is not captured and
        /// history will not bring it back. That omission is otherwise silent:
        /// a folder of notes nobody linked looks exactly like a folder of notes
        /// that are safe. Linking the file is the repair.
        ///
        /// Works under `history: off` too — it writes nothing, so the axis has
        /// nothing to refuse, and asking what the workspace fails to reach is a
        /// question about the workspace rather than about history.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show this device's fixity cache for the workspace: where it lives and how
    /// many files it remembers.
    ///
    /// The cache is what lets `history-capture` skip files whose timestamp and
    /// size say they have not changed, instead of reading and hashing the whole
    /// workspace every time. It is device-local, deliberately outside the
    /// workspace (it is not part of what the archive says about itself), and
    /// entirely disposable — losing it costs one slow capture and nothing else.
    ///
    /// It is never consulted by `check`. Bit-rot is a change to the bytes that
    /// leaves the timestamp alone, so a cache keyed on timestamps would vouch
    /// for exactly the file that rotted.
    Cache {
        /// Delete it. The next capture reads and hashes everything, and starts a
        /// new one.
        #[arg(long)]
        clear: bool,
    },
    /// List the captures in the history store, newest first: id, timestamp,
    /// label, and how many files changed since each event's parent.
    ///
    /// Works regardless of the `history` config axis — recovery must never be
    /// gated behind re-enabling a setting, least of all on the machine that just
    /// suffered the damage.
    HistoryList,
    /// Print one event: its metadata, and the complete manifest of the file set
    /// exactly as it stood at that capture — each row marked when the pre-image
    /// bytes it names are not in the store.
    ///
    /// There is nothing to reconstruct: a full manifest *is* the effective
    /// state, which is what this format buys over a delta log. A manifest and
    /// its blobs travel over a sync transport separately, so an event whose
    /// bytes have not all arrived is ordinary rather than broken — and legible
    /// here, before anyone asks a restore to act on it.
    ///
    /// Works regardless of the `history` config axis.
    HistoryShow {
        /// The event id, as `history-list` prints it — for example
        /// `2026-07-31-0915-pre-sync-4f2a9c1e`. Resolves to its document
        /// directly, with no index consulted.
        event: String,
    },
    /// Write one captured file's bytes to standard output — the pre-image
    /// exactly as it stood at that capture.
    ///
    /// A lookup, not a reconstruction: the manifest row names a
    /// content-addressed blob, so this costs one read however many captures have
    /// happened since. It is what makes the store work with tools that are not
    /// prov —
    ///
    ///     prov history-cat 2026-07-31-0915-4f2a9c1e notes.md | diff - notes.md
    ///
    /// Bytes are written verbatim and are not necessarily text: a capture set
    /// holds whatever the workspace holds. Redirect to a file for an attachment.
    ///
    /// Exits non-zero, writing nothing to stdout, when the event has no such row
    /// or its bytes are not in the store — so a pipeline fails rather than
    /// silently comparing against an empty file.
    ///
    /// Works regardless of the `history` config axis.
    HistoryCat {
        /// The event id, as `history-list` prints it.
        event: String,
        /// The document: a path, or `id:<id>` to follow an id directly.
        ///
        /// A path is resolved to its id when the workspace has one, which is
        /// what reaches a document that has been renamed since the capture. A
        /// path that no longer exists is matched against the manifest as
        /// written — which is how a *deleted* document's bytes come back.
        target: String,
    },
    /// Compare two captures: what changed, what moved, what arrived, what went.
    ///
    /// Both events hold full manifests, so this is a comparison rather than a
    /// fold — nothing between them is read, and the two need not be adjacent or
    /// even from the same device.
    ///
    /// With no arguments, compares the newest capture against its parent: what
    /// the last capture recorded. With one, does the same for that event. With
    /// two, compares them directly, oldest-first regardless of the order given.
    ///
    /// A move is reported as a move, not as a deletion beside a creation, when
    /// the pairing is unambiguous — one path left with exactly those bytes and
    /// one arrived with them. A directory rename is then one intention and not
    /// several hundred rows. The inference is the same one `history-log` uses
    /// and carries the same limit: two unrelated files with identical content
    /// look like a move, and identical content is common (every empty file
    /// shares a digest), so an ambiguous pairing is never claimed.
    ///
    /// Works regardless of the `history` config axis.
    HistoryDiff {
        /// The earlier event, or the only event when `b` is omitted (in which
        /// case its parent is the other side). Defaults to the newest capture.
        a: Option<String>,
        /// The later event. Omit to compare `a` against its parent.
        b: Option<String>,
        /// Show a unified diff of every **changed** text file, not just the
        /// summary rows.
        ///
        /// Only changed files: an added or removed file's whole content is
        /// `prov history-cat`'s job, and dumping it here would print an entire
        /// workspace for a first capture. A file whose captured bytes are not
        /// valid UTF-8, or whose pre-image is not in this store, is named and
        /// skipped rather than mangled.
        #[arg(long)]
        patch: bool,
        /// Limit the comparison to these paths — naming a directory covers the
        /// subtree. After `--`, as in `git diff`, since the positional
        /// arguments before it are event ids.
        #[arg(last = true)]
        paths: Vec<PathBuf>,
    },
    /// Print one document's lineage across every capture: the events where its
    /// bytes or its path changed, newest first.
    ///
    /// Following an id is rename-robust — a move shows as one document that
    /// changed path, where a path-keyed history shows two unrelated lineages
    /// that happen to abut. A path argument naming a registered document is
    /// therefore followed by its id. A path with no id (the config document,
    /// the registry, an attachment payload) is followed by path, which is the
    /// best there is for a document that carries no identity.
    ///
    /// A derived query over the manifests, not a stored per-document chain: it
    /// reads every event in the store and writes nothing.
    ///
    /// Works regardless of the `history` config axis.
    HistoryLog {
        /// The document: a path, or `id:<id>` to follow an id directly — which
        /// still works for a document that has since been deleted.
        target: String,
    },
    /// Write a capture out to a directory somewhere else, leaving the workspace
    /// untouched.
    ///
    /// The safe way to look at an old state. `history-restore` writes over the
    /// workspace and is the tool for undoing damage; this copies the captured
    /// bytes somewhere new and changes nothing you already have — so comparing,
    /// salvaging one paragraph, or just seeing what a vault looked like in March
    /// costs nothing and risks nothing.
    ///
    /// Bytes are written **verbatim**, exactly as the capture holds them. One
    /// consequence worth stating: a whole-event export is a workspace whose root
    /// still declares a `history` pointer, and the store is not copied, so that
    /// link dangles there. That is the honest result — these are the captured
    /// bytes, not a workspace prov has adjusted — and `prov check` in the export
    /// will say so.
    ///
    /// Works regardless of the `history` config axis.
    HistoryExport {
        /// The event id, as `history-list` prints it.
        event: String,
        /// Where to write it. Created if missing; refused if it already holds
        /// anything, since an export never merges into an existing tree.
        #[arg(long, value_name = "DIR")]
        to: PathBuf,
        /// Export only the row carrying this document id, wherever the capture
        /// found it — the way to reach a document whose path has since changed.
        #[arg(long, value_name = "ID")]
        id: Option<String>,
        /// Limit the export to these captured paths; naming a directory covers
        /// the subtree it held. After `--`, as in `history-diff`.
        #[arg(last = true)]
        paths: Vec<PathBuf>,
    },
    /// Write a captured state back over the workspace: additive by default,
    /// exact on request.
    ///
    /// An event is a *consistent cut*. If a bad merge corrupted a renamed file
    /// and its parent's child list, both were hashed in the same capture, so
    /// restoring the whole event puts the set back together — which is what
    /// actually undoes the damage. Restoring one file out of it does not:
    /// writing one file's old bytes back without the rest of the same
    /// corruption's footprint can reintroduce the inconsistency history exists
    /// to fix. Scope this to paths or an id when a sync clobbered one file's
    /// prose; leave it whole when the graph broke.
    ///
    /// The default writes every captured path and deletes nothing. That leaves
    /// a gap on purpose: bad-merge damage is characteristically additive (a
    /// `.sync-conflict` copy, a rename-vs-rename landing both names), and none
    /// of it goes away by writing captured bytes over the top. `--exact` is the
    /// honest "undo this merge entirely" tool — see its own help.
    ///
    /// Restore does not repair links or the registry. It runs `check` before and
    /// after and reports the difference in three buckets — fixed, introduced,
    /// pre-existing — because you are restoring precisely when something is
    /// already broken, and a bare list of findings afterwards cannot tell you
    /// which of them you just caused. A non-empty *introduced* bucket exits
    /// non-zero; `prov check --fix` is the explicit next step.
    ///
    /// The history store itself is never written or deleted, and the root's
    /// `history` pointer is never removed — a captured root predating the store
    /// must not strand it unreachable.
    ///
    /// Works regardless of the `history` config axis: recovery must never be
    /// gated behind re-enabling a setting, least of all on the machine that just
    /// suffered the damage.
    HistoryRestore {
        /// The event id, as `history-list` prints it — for example
        /// `2026-07-31-0915-pre-sync-4f2a9c1e`.
        event: String,
        /// Restore only these captured paths (a directory restores everything
        /// the capture held beneath it). Content recovery, not structural
        /// repair — see the command help. Omit to restore the whole capture.
        #[arg(value_name = "PATH")]
        paths: Vec<String>,
        /// Restore only the document the capture recorded under this id, wherever
        /// it lived at the time. Rename-robust where a path is not.
        #[arg(long, value_name = "ID", conflicts_with = "paths")]
        id: Option<String>,
        /// Also remove every reachable file the capture does not contain, so the
        /// tree *matches* the event rather than merely including it.
        ///
        /// This is what undoes an additive bad merge — and the same pass discards
        /// legitimate work done since the capture. It restores the whole event by
        /// definition, so it cannot be combined with a scope, and it lists what it
        /// would remove and asks first on a terminal.
        #[arg(long)]
        exact: bool,
        /// Proceed even though restoring would displace a registration: an id the
        /// registry now binds to a different document, or a path it now binds to a
        /// different id. Refused by default — two documents claiming one id is
        /// something only their author can arbitrate.
        #[arg(long)]
        force: bool,
        /// Print the plan — what would be created, overwritten, left alone, left
        /// unrecoverable for want of bytes, and removed — and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation `--exact` asks before removing files.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Drop the oldest captures and collect the bytes no surviving capture
    /// references. Manual, never automatic, and irreversible.
    ///
    /// With full manifests this is delete plus garbage collection and nothing
    /// else: every event is self-contained, so dropping one cannot make another
    /// unreadable. What it *can* do is destroy the only copy of some content —
    /// including content another device captured and this one never had live —
    /// so it lists what it would drop and asks first.
    ///
    /// Exactly one bound is required. There is no default: an operation that
    /// deletes bytes should not do so because a flag was forgotten.
    ///
    /// The blob sweep is the same one `check` reports as orphaned, taken against
    /// the survivors — so a prune also collects blobs that were already
    /// unreferenced, which is what that finding points here for.
    ///
    /// Works regardless of the `history` config axis: turning the feature off
    /// must not strand bytes you can no longer clean up.
    HistoryPrune {
        /// Keep the newest N captures and drop everything older.
        #[arg(long, value_name = "N")]
        keep: Option<usize>,
        /// Drop every capture taken strictly before this date (`2026-06-01`) or
        /// RFC 3339 instant. A capture *on* the named day is kept.
        #[arg(long, value_name = "DATE", conflicts_with = "keep")]
        before: Option<String>,
        /// Print what would be dropped and collected, and delete nothing.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Destroy one document's captured bytes, and record that it was deliberate.
    ///
    /// History extends retention of everything ever captured: if any event caught
    /// a document while it was live, its bytes are in the store, and neither
    /// `empty-bin` nor `rm --purge` touches them. This is the tool that makes that
    /// irreversible on purpose.
    ///
    /// Two limits, both load-bearing. It destroys **only bytes nothing else
    /// names** — a hash shared with another captured path survives, because
    /// content addressing means forgetting one document cannot reach into
    /// another's history. And it destroys **bytes, not the record**: event
    /// documents are immutable, so every manifest still names the path, the id and
    /// the hash. If what has to disappear is the name, this is not that tool.
    ///
    /// The forgotten hashes are recorded in `history/forgotten.<ext>` so `check`
    /// can tell deliberate destruction from loss, and so the read verbs can say
    /// "forgotten" rather than "missing".
    ///
    /// Works regardless of the `history` config axis.
    HistoryForget {
        /// The document: a path, or `id:<id>` to follow an id directly — which
        /// still works for a document that has since been deleted, and is the
        /// rename-robust key.
        target: String,
        /// Forget even though the document is still in the workspace. Refused by
        /// default, because the next capture would simply park its bytes again.
        #[arg(long)]
        force: bool,
        /// Skip the confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

/// Which home the `config --home` conversion relocates workspace policy to. The
/// two homes read identically (DESIGN §2, "two homes, one vocabulary"); this only
/// chooses *where the bytes live*.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ConfigHome {
    /// Inline in the root document's `prov:` block; the sidecar is removed.
    Root,
    /// In the dedicated `prov.yaml` config document; the root's block is cleared.
    Sidecar,
}

/// CLI spelling of the metadata formats prov compiles in. Variants track the
/// crate's format features: YAML is always available; JSON and the native fig
/// dialect appear only when their features are enabled, so `--format` never
/// offers a format whose parser is not in the binary.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum MetaFormat {
    Yaml,
    #[cfg(feature = "toml")]
    Toml,
    #[cfg(feature = "json")]
    Json,
    #[cfg(feature = "fig-lang")]
    Fig,
}

impl MetaFormat {
    /// The lowercase spelling for the `init` summary line.
    pub(crate) fn label(self) -> &'static str {
        match self {
            MetaFormat::Yaml => "yaml",
            #[cfg(feature = "toml")]
            MetaFormat::Toml => "toml",
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
pub(crate) enum ContentLang {
    Markdown,
    Djot,
    Html,
}

impl ContentLang {
    /// The root document's file extension for this grammar.
    pub(crate) fn ext(self) -> &'static str {
        match self {
            ContentLang::Markdown => "md",
            ContentLang::Djot => "dj",
            ContentLang::Html => "html",
        }
    }

    /// A title heading in this grammar — the seed body of the root document.
    pub(crate) fn heading(self, title: &str) -> String {
        match self {
            // Markdown and Djot share ATX heading syntax.
            ContentLang::Markdown | ContentLang::Djot => format!("# {title}\n"),
            ContentLang::Html => format!("<h1>{title}</h1>\n"),
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            ContentLang::Markdown => "markdown",
            ContentLang::Djot => "djot",
            ContentLang::Html => "html",
        }
    }

    /// The embed styles `init` offers for this grammar, in menu order (the first
    /// is the default). Markdown gets delimiters, a fenced block, or a separate
    /// sidecar; Djot drops delimiters (it has no idiomatic frontmatter, and a
    /// leading `---`/`+++` is body syntax) and offers a fenced block or separate;
    /// HTML offers the two data-island shapes; every grammar can keep metadata
    /// in a sibling file.
    pub(crate) fn embed_styles(self) -> &'static [EmbedStyle] {
        match self {
            ContentLang::Markdown => &[
                EmbedStyle::Delimited,
                EmbedStyle::CodeBlock,
                EmbedStyle::Separate,
            ],
            ContentLang::Djot => &[EmbedStyle::CodeBlock, EmbedStyle::Separate],
            ContentLang::Html => &[
                EmbedStyle::HtmlScript,
                EmbedStyle::HtmlCode,
                EmbedStyle::Separate,
            ],
        }
    }

    /// Whether `style` is a sensible embed for this content grammar — the
    /// validity check the `--embed` flag is held to (the interactive menu only
    /// ever offers valid styles).
    pub(crate) fn allows_embed(self, style: EmbedStyle) -> bool {
        self.embed_styles().contains(&style)
    }
}

/// A menu label + hint for an embed style — the `init` "Embed type" prompt and
/// the summary line's spelling.
pub(crate) fn embed_labels(style: EmbedStyle) -> (&'static str, &'static str) {
    match style {
        EmbedStyle::Delimited => ("Character delimiters", "--- yaml · +++ toml · ;;; json"),
        EmbedStyle::CodeBlock => ("Typed code block", "```yaml · ```toml · ```fig"),
        EmbedStyle::HtmlScript => ("Script tag", "<script type=\"application/…\">"),
        EmbedStyle::HtmlCode => ("Code tag", "<pre><code class=\"language-…\">"),
        EmbedStyle::Separate => ("Separate", "metadata in a sibling file"),
    }
}

/// The config languages `init` offers for `embed`, compiled-in only. YAML is
/// always present; TOML/JSON/fig follow their crate features. The fig dialect
/// has no character-delimiter form, so it is dropped for [`EmbedStyle::Delimited`].
pub(crate) fn config_languages(embed: EmbedStyle) -> Vec<(MetaFormat, &'static str)> {
    let _ = embed; // read below only under the `fig-lang` feature
    let mut opts = vec![(MetaFormat::Yaml, "YAML")];
    #[cfg(feature = "toml")]
    opts.push((MetaFormat::Toml, "TOML"));
    #[cfg(feature = "json")]
    opts.push((MetaFormat::Json, "JSON"));
    #[cfg(feature = "fig-lang")]
    if embed != EmbedStyle::Delimited {
        opts.push((MetaFormat::Fig, "fig"));
    }
    opts
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

/// CLI spelling of the metadata *embed type* ([`prov::EmbedStyle`]) — how
/// the metadata is carried in (or beside) the document, one level above the
/// config language. Which styles make sense depends on the content grammar (see
/// [`ContentLang::embed_styles`]); the `--embed` flag accepts any and is
/// validated against the chosen content.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum EmbedArg {
    /// Character-delimited frontmatter (`---`/`+++`/`;;;`). Markdown only.
    Delimited,
    /// A typed fenced code block (```` ```yaml ````, ```` ```fig ````, …).
    CodeBlock,
    /// An HTML `<script type="application/…">` data island. HTML only.
    HtmlScript,
    /// An HTML `<pre><code class="language-…">` block. HTML only.
    HtmlCode,
    /// Metadata in a sibling whole-file document, linked by `content`.
    Separate,
}

impl From<EmbedArg> for EmbedStyle {
    fn from(e: EmbedArg) -> Self {
        match e {
            EmbedArg::Delimited => EmbedStyle::Delimited,
            EmbedArg::CodeBlock => EmbedStyle::CodeBlock,
            EmbedArg::HtmlScript => EmbedStyle::HtmlScript,
            EmbedArg::HtmlCode => EmbedStyle::HtmlCode,
            EmbedArg::Separate => EmbedStyle::Separate,
        }
    }
}

/// CLI spelling of the workspace link styles ([`prov::LinkStyle`]).
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum LinkStyleArg {
    MarkdownRoot,
    MarkdownRelative,
    PlainRelative,
    PlainRoot,
}

impl From<LinkStyleArg> for LinkStyle {
    fn from(l: LinkStyleArg) -> Self {
        match l {
            LinkStyleArg::MarkdownRoot => LinkStyle::MarkdownRoot,
            LinkStyleArg::MarkdownRelative => LinkStyle::MarkdownRelative,
            LinkStyleArg::PlainRelative => LinkStyle::PlainRelative,
            LinkStyleArg::PlainRoot => LinkStyle::PlainRoot,
        }
    }
}

/// When a document earns a stable ID — the `identity` config key, one of the
/// two independent identity axes `init` asks about. `Off` is paths-only; `Lazy`
/// mints on a durable reference (link-by-id or publish); `Eager` mints every
/// document at creation. The spellings match the config value ([`registration_from_str`]).
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum IdentityArg {
    /// Paths only — no document ever earns a stable ID. `none` is accepted as a
    /// synonym, matching the canonical `identity: none` config spelling.
    #[value(alias = "none")]
    Off,
    Lazy,
    Eager,
}

impl IdentityArg {
    /// The registration trigger set this identity policy selects.
    pub(crate) fn registration(self) -> Registration {
        match self {
            IdentityArg::Off => Registration::OFF,
            IdentityArg::Lazy => Registration::LAZY,
            IdentityArg::Eager => Registration::EAGER,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            IdentityArg::Off => "off",
            IdentityArg::Lazy => "lazy",
            IdentityArg::Eager => "eager",
        }
    }
}

/// Where a document's stable ID is stored — the `id_storage` config key
/// ([`IdStorage`]). `Registry` is the current default (IDs only in the registry);
/// `Frontmatter` also stamps each document's own `id` field (a portable,
/// self-describing shadow, registry kept as a cache); `FrontmatterOnly` drops the
/// registry entirely (self-describing, but no tombstones). `init` offers the
/// first two; the third is deliberately flag-only.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum IdStorageArg {
    Registry,
    Frontmatter,
    FrontmatterOnly,
}

impl From<IdStorageArg> for IdStorage {
    fn from(s: IdStorageArg) -> Self {
        match s {
            IdStorageArg::Registry => IdStorage::Registry,
            IdStorageArg::Frontmatter => IdStorage::Frontmatter,
            IdStorageArg::FrontmatterOnly => IdStorage::FrontmatterOnly,
        }
    }
}

impl IdStorageArg {
    /// The lowercase spelling for the `init` summary line.
    pub(crate) fn label(self) -> &'static str {
        IdStorage::from(self).as_config_str()
    }
}

/// How `check --fix` decides what to repair.
///
/// The split is [`prov::Warrant`]: a repair that restates an authority
/// (regenerate the derived page, rebuild the derived index, spell a link the way
/// the file on disk is actually named) chooses nothing and can run unattended,
/// while one that picks among rival readings must be picked *by someone*.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum FixModeArg {
    /// Walk the findings and offer each one's repairs. The default.
    Ask,
    /// Apply only the repairs nothing is being chosen in, and prompt for
    /// nothing — the scriptable mode. Leaves everything else outstanding, and
    /// says so.
    Mechanical,
}

/// How far content-checksum (fixity) coverage extends — the `fixity` config key
/// ([`prov::Fixity`]). `Payloads` (the default) checksums attachment payloads
/// only — frictionless, since a payload is never edited; `Full` also checksums
/// document bodies (pair with `prov edit`); `Off` records nothing.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum FixityArg {
    Off,
    Payloads,
    Full,
}

impl From<FixityArg> for prov::Fixity {
    fn from(f: FixityArg) -> Self {
        match f {
            FixityArg::Off => prov::Fixity::Off,
            FixityArg::Payloads => prov::Fixity::Payloads,
            FixityArg::Full => prov::Fixity::Full,
        }
    }
}

impl FixityArg {
    /// The lowercase spelling for the `init` summary line.
    pub(crate) fn label(self) -> &'static str {
        prov::Fixity::from(self).as_config_str()
    }
}

/// What `init` does with content documents already present in the directory
/// (`docs/init-adoption.md`). `Flat` (Phase 1) links each loose file directly
/// under the new root; `Mirror` (Phase 2) folds the directory tree into the
/// containment tree — every directory becomes a node, synthesizing a folder-note
/// index where none exists; `None_` initializes but leaves them unlinked.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum AdoptArg {
    Flat,
    #[value(name = "none")]
    None_,
    Mirror,
}

/// The syntactic wrapper `init` authors references in — the *first* style axis
/// (`--wrapper`), chosen before the addressing (see `docs/reference-styles.md`,
/// "pick the wrapper first, then the substyle").
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum WrapperArg {
    /// The diaryx/CommonMark form: `[Title](target)` (or bare).
    Markdown,
    /// The Obsidian form: `[[target]]` / `[[target|Title]]`.
    Wikilink,
}

impl From<WrapperArg> for Wrapper {
    fn from(w: WrapperArg) -> Self {
        match w {
            WrapperArg::Markdown => Wrapper::Markdown,
            WrapperArg::Wikilink => Wrapper::Wikilink,
        }
    }
}

/// What the references `init` authors address their target *by* — the *second*
/// style axis (`--reference`), the addressing. `Path` is readable but rewritten
/// on move; `Id` is durable and registers its target (so it needs identity);
/// `Alias` is by title (readable, never move-safe, never registers); `Split`
/// sets *different* addressing for the two spanning directions (the diaryx up≠down
/// shape). The wrapper is chosen separately ([`WrapperArg`]).
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ReferenceArg {
    /// By path — rewritten when a file moves. Rendering follows `--link-style`.
    Path,
    /// By durable `id:<id>` handle — survives moves untouched, registers the target.
    Id,
    /// By the target's title — readable, but not move/rename-safe (implies wikilink).
    Alias,
    /// Readable *down*, durable *up*: `contents` by alias, `part_of` by id.
    Split,
}

impl ReferenceArg {
    /// Whether this addressing registers targets (link-by-id), so it needs
    /// identity to mint IDs. `Path` and `Alias` never register.
    pub(crate) fn needs_identity(self) -> bool {
        matches!(self, ReferenceArg::Id | ReferenceArg::Split)
    }

    /// Whether a by-path reference is (possibly) authored, so `init` asks the
    /// path-format question. Only `Path` addresses by path.
    pub(crate) fn uses_path(self) -> bool {
        self == ReferenceArg::Path
    }

    /// The lowercase spelling for the `init` summary line and `--reference` flag.
    pub(crate) fn label(self) -> &'static str {
        match self {
            ReferenceArg::Path => "path",
            ReferenceArg::Id => "id",
            ReferenceArg::Alias => "alias",
            ReferenceArg::Split => "split (alias down, id up)",
        }
    }

    /// The `--reference` flag value (kebab-case), for diagnostics.
    pub(crate) fn flag(self) -> &'static str {
        match self {
            ReferenceArg::Split => "split",
            other => other.label(),
        }
    }

    /// Write the addressing axis and per-relation overrides this reference choice
    /// encodes onto `config`. The workspace `notation`/`path_style` are already set
    /// (from the wrapper + path-format prompts); this only touches `target`,
    /// `label`, and the split relations — except `alias`, which forces wikilink.
    pub(crate) fn write_onto(self, config: &mut WorkspaceConfig) {
        // Author id links *labeled* — `[Title](id:…)` for markdown, `[[id:…|Title]]`
        // for wikilink — so a durable reference stays readable, and clickable with
        // graceful degradation (an `id:` scheme link resolves in tools that know it,
        // and says "unsupported scheme" in those that don't), rather than an opaque
        // bare id. The label is a maintained cache of the target's title.
        let id_label = true;
        match self {
            // Path addressing is the default; notation/path_style already carry it.
            ReferenceArg::Path => {}
            ReferenceArg::Id => {
                config.reference_target = Addressing::Id;
                config.reference_label = id_label;
            }
            ReferenceArg::Alias => {
                // Alias has no markdown/bare spelling; it always renders wikilink.
                config.notation = Notation::Wikilink;
                config.reference_target = Addressing::Alias;
            }
            // Durable id by default (overlay relations like `links` stay
            // move-stable), then the two spanning directions diverge: a readable
            // alias going down, an id link going up in the workspace notation.
            ReferenceArg::Split => {
                config.reference_target = Addressing::Id;
                config.reference_label = id_label;
                config.relation_styles.insert(
                    "contents".into(),
                    RelationStyleConfig {
                        notation: Some(Notation::Wikilink),
                        path_style: None,
                        target: Some(Addressing::Alias),
                        label: None,
                    },
                );
                config.relation_styles.insert(
                    "part_of".into(),
                    RelationStyleConfig {
                        notation: None, // inherit the workspace notation
                        path_style: None,
                        target: Some(Addressing::Id),
                        label: Some(id_label),
                    },
                );
            }
        }
    }
}

impl From<MetaFormat> for Format {
    fn from(f: MetaFormat) -> Format {
        match f {
            MetaFormat::Yaml => Format::Yaml,
            #[cfg(feature = "toml")]
            MetaFormat::Toml => Format::Toml,
            #[cfg(feature = "json")]
            MetaFormat::Json => Format::Json,
            #[cfg(feature = "fig-lang")]
            MetaFormat::Fig => Format::Fig,
        }
    }
}
