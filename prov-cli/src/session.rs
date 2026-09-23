//! The session layer: where the workspace is, and how a command opens it.
//!
//! Every workspace command begins the same way — discover the root from the
//! current directory ([`find_root`]), build the library's [`Workspace`] over
//! it with the config's policy knobs applied ([`workspace`]), and, once a
//! mutation has run, land whatever identity changes it made ([`persist`]).
//! This module is that beginning and end, kept in one place so the commands
//! cannot disagree about what a workspace *is* — which registry it reads,
//! which documents are machinery rather than content, how a CLI argument
//! names a document ([`resolve_target`]).
//!
//! Nothing here prints a result or parses a flag: it is the plumbing between
//! the argument grammar in [`crate::cli`] and the library, and each command
//! module reaches it by name.

use std::path::{Path, PathBuf};

use prov::{
    Document, FileIndex, Id, IdIndex, Layout, Minter, StdFs, Workspace, WorkspaceConfig,
    WorkspaceRoot, block_on, link,
};

use crate::AnyError;
use crate::about::refresh_about;

/// The discovered workspace context: where the root is, which document is the
/// root, and where the root says the registry lives.
pub(crate) struct Ctx {
    /// Absolute path of the workspace root directory.
    pub(crate) root_dir: PathBuf,
    /// The root document, relative to `root_dir`.
    pub(crate) root_doc: PathBuf,
    /// The registry document the root declares (relative to `root_dir`), if any.
    pub(crate) registry: Option<PathBuf>,
    /// The workspace node (`prov.yaml`) found by convention, relative to
    /// `root_dir`, if any — the one document a timestamp stamp must never land
    /// in, because its `updated` key is the *name* of the field and not a
    /// value of it.
    pub(crate) node: Option<PathBuf>,
    /// The effective workspace config (root frontmatter overlaid by the linked
    /// config document, over defaults).
    pub(crate) config: WorkspaceConfig,
}

impl Ctx {
    /// This workspace, as the library's writable-workspace calls read it.
    pub(crate) fn as_root(&self) -> WorkspaceRoot<'_> {
        WorkspaceRoot {
            root_dir: &self.root_dir,
            root_doc: &self.root_doc,
            registry: self.registry.as_deref(),
            config: &self.config,
        }
    }
}

impl From<prov::Discovered> for Ctx {
    fn from(d: prov::Discovered) -> Self {
        Ctx {
            root_dir: d.root_dir,
            root_doc: d.root_doc,
            registry: d.registry,
            node: d.node.node,
            config: d.config,
        }
    }
}

/// An open workspace: the discovered [`Ctx`] and the library engine built over
/// it, which is what every workspace command holds from its first line to its
/// last.
///
/// The two constructors are the two ways a command begins. [`Session::open`]
/// is a read, or a mutation that cannot mint an ID. [`Session::open_for_mutation`]
/// is a mutation that may — because the reference style registers, or the
/// identity policy is eager — and so makes sure a registry exists *before* the
/// workspace is built over it, since a route's synthesized nodes are `create`d
/// and mint on the same terms as the leaf. That precondition used to be a
/// comment at each call site; here it is one line, and a verb that opens the
/// wrong way is the only way to get it wrong.
///
/// [`Session::commit`] is the end: the identity changes a mutation made, landed
/// according to the workspace's storage mode.
pub(crate) struct Session {
    pub(crate) ctx: Ctx,
    pub(crate) ws: Workspace<StdFs, Minter, FileIndex>,
}

impl Session {
    /// The workspace around the current directory, for reading or for a
    /// mutation that mints nothing.
    pub(crate) fn open() -> Result<Self, AnyError> {
        Self::over(find_root()?)
    }

    /// The workspace around the current directory, for a mutation that may
    /// mint — a registry is bootstrapped first when the config says it does.
    pub(crate) fn open_for_mutation() -> Result<Self, AnyError> {
        Self::mutating(find_root()?)
    }

    /// [`Session::open`] over a context the caller already discovered.
    pub(crate) fn over(ctx: Ctx) -> Result<Self, AnyError> {
        let ws = workspace(&ctx)?;
        Ok(Self { ctx, ws })
    }

    /// [`Session::open_for_mutation`] over a context the caller already
    /// discovered.
    pub(crate) fn mutating(mut ctx: Ctx) -> Result<Self, AnyError> {
        if ctx.config.mints_on_mutation() {
            ensure_registry(&mut ctx)?;
        }
        Self::over(ctx)
    }

    /// Land the identity changes a mutation made — see [`persist`].
    pub(crate) fn commit(&mut self) -> Result<(), AnyError> {
        persist(&self.ctx, &mut self.ws)
    }
}

/// Resolve the workspace root and, on success, warn (once, to stderr) about any
/// config a command would otherwise run past silently — settings prov would
/// ignore, or a config `spec` newer than this build. Suppressed by
/// `PROV_QUIET`. Commands that already report config in full
/// (`check`, `config`) use [`find_root_quiet`] instead.
pub(crate) fn find_root() -> Result<Ctx, AnyError> {
    let ctx = find_root_quiet()?;
    warn_config(&ctx);
    Ok(ctx)
}

/// Warn about config that will not take effect — the proactive counterpart to
/// `check`'s [`prov::Finding::ConfigIssue`]. One stderr line summarizing
/// settings prov would silently ignore (a typo or unrecognized value across
/// either config surface), and one for a `spec` this build is too old to fully
/// read. Quiet when the config is clean, or when `PROV_QUIET` is set.
fn warn_config(ctx: &Ctx) {
    if std::env::var_os("PROV_QUIET").is_some() {
        return;
    }
    let mut issues = Vec::new();
    let mut spec_ahead = None;
    // The root's `prov:` block.
    if let Ok(text) = std::fs::read_to_string(ctx.root_dir.join(&ctx.root_doc))
        && let Ok(doc) = Document::parse(&ctx.root_doc, &text)
        && let Some(block) = doc.meta.get(prov::config::ROOT_CONFIG_KEY)
    {
        issues.extend(prov::diagnose(block));
        spec_ahead = spec_ahead.or_else(|| prov::spec_ahead(block));
    }
    // The dedicated config document.
    let probe: Workspace<StdFs> = Workspace::builder(StdFs).root(&ctx.root_dir).build();
    if let Ok(Some(config_doc)) = block_on(probe.config_path(&ctx.root_doc))
        && let Ok(text) = std::fs::read_to_string(ctx.root_dir.join(&config_doc))
        && let Ok(doc) = Document::parse(&config_doc, &text)
    {
        issues.extend(prov::diagnose(&doc.meta));
        spec_ahead = spec_ahead.or_else(|| prov::spec_ahead(&doc.meta));
    }
    if let Some(declared) = spec_ahead {
        eprintln!(
            "prov: config declares spec {declared} but this build understands spec {} — newer settings may be ignored (upgrade prov)",
            prov::config::SPEC_VERSION
        );
    }
    // "Not taking effect" rather than "ignored": most of these *are* keys prov
    // silently drops, but `views.<name>.nest` on a multi-valued field is read
    // and simply cannot be acted on. One summary line covers both; `check` says
    // which it is.
    if let Some(first) = issues.first() {
        eprintln!(
            "prov: {} config setting(s) will not take effect (e.g. `{}`) — run `prov check` for details",
            issues.len(),
            first.key
        );
    }
}

/// Find the workspace root by walking up from the current directory. The walk,
/// the root-candidate rule, and the tie-breaking all live in the library
/// ([`prov::discover`]); this only supplies the real current directory and
/// phrases the two failure modes as CLI diagnostics. Does not warn about config —
/// see [`find_root`].
pub(crate) fn find_root_quiet() -> Result<Ctx, AnyError> {
    let cwd = std::env::current_dir()?;
    find_root_quiet_at(&cwd)
}

/// [`find_root_quiet`], but discovering from `dir` rather than the process's
/// current directory — for a re-discovery after a write has changed the config
/// on disk, where the caller already knows the root.
pub(crate) fn find_root_quiet_at(dir: &Path) -> Result<Ctx, AnyError> {
    match block_on(prov::discover(&StdFs, dir))? {
        prov::Discovery::Found(d) => Ok(Ctx::from(d)),
        prov::Discovery::Ambiguous { dir, candidates } => Err(format!(
            "ambiguous workspace root in {}: {} (rename one, add part_of, or declare the workspace with a prov.yaml beside its root)",
            dir.display(),
            candidates.join(", ")
        )
        .into()),
        prov::Discovery::NotFound => Err(
            "no workspace root found: no ancestor directory has a document \
with metadata and no part_of\n\
\n\
  If this directory holds content already, run `prov init` here to adopt it\n\
  (use `prov init --adopt` to link existing files in non-interactively).\n\
  Otherwise `prov init` starts a fresh workspace."
                .into(),
        ),
    }
}

/// The workspace around `dir`, if there is one — for a command that works on
/// a *file* and keeps working outside any workspace (`set`, `unset`), but does
/// the workspace's bookkeeping when it finds itself inside one. Every answer
/// other than a clean find is `None`: an ambiguous root is a fault `check` and
/// `edit` report, and a single-field edit is not the place to refuse over it.
pub(crate) fn workspace_around(dir: &Path) -> Option<Ctx> {
    match block_on(prov::discover(&StdFs, dir)) {
        Ok(prov::Discovery::Found(d)) => Some(Ctx::from(d)),
        _ => None,
    }
}

/// The documents that are the workspace's own record-keeping rather than its
/// content — the node, the registry, the deletion log, and each flat `fields`
/// vocabulary — as workspace-relative paths.
///
/// These are exactly the whole-file stores `check` holds to the whole-file
/// rule, plus the node, and they are enumerated here for the opposite reason:
/// a timestamp is a claim about *content*, and stamping one into a store
/// corrupts it. The node's `updated:` key names the field, so a stamp there
/// overwrites the name with a value; the registry is a record store prov
/// re-lays-out from its own shape. A reified vocabulary is content — an index
/// node with term documents under it — and is not listed.
pub(crate) fn machinery(
    ctx: &Ctx,
    ws: &Workspace<StdFs, Minter, FileIndex>,
) -> Result<Vec<PathBuf>, AnyError> {
    let mut stores: Vec<PathBuf> = ctx.node.iter().cloned().collect();
    stores.extend(ctx.registry.iter().cloned());
    stores.extend(block_on(ws.deletions_path(&ctx.root_doc))?);
    for (_, spec) in ctx.config.field_declarations() {
        if spec.reify {
            continue;
        }
        if let Some(pointer) = &spec.vocabulary
            && let Some(p) = ws.vocabulary_path(&ctx.root_doc, pointer)
        {
            stores.push(p);
        }
    }
    Ok(stores)
}

/// The timestamp half of a content change to `rel`: the workspace's `updated`
/// field and the instant `now`, or `None` when the workspace keeps no such
/// field or `rel` is [`machinery`] rather than content. Shared by
/// every verb that stamps — `edit`, `set`, `unset`, `stamp` — so they cannot
/// disagree about which documents a timestamp may land in.
pub(crate) fn updated_stamp<'a>(
    ctx: &'a Ctx,
    machinery: &[PathBuf],
    rel: &Path,
    now: &'a str,
) -> Option<(&'a str, &'a str)> {
    if ctx.config.updated.is_empty() || machinery.iter().any(|m| m == rel) {
        return None;
    }
    Some((ctx.config.updated.as_str(), now))
}

/// The workspace the multi-document commands drive: rooted at the discovered
/// root, the configured identity policy, and the id index the workspace keeps
/// — [`Workspace::open_for_writing`], which is where that assembly lives so
/// that every writer, and not only this one, builds it the same way.
pub(crate) fn workspace(ctx: &Ctx) -> Result<Workspace<StdFs, Minter, FileIndex>, AnyError> {
    Ok(block_on(Workspace::open_for_writing(
        StdFs,
        ctx.as_root(),
        entropy_seed(),
    ))?)
}

/// Make sure the workspace *declares* a registry, bootstrapping one when it
/// does not — [`prov::ensure_registry`], which writes `registry.<ext>` beside
/// the root and the root's pointer to it as one change set. What is the CLI's
/// is saying so, recording the registry in `ctx`, and refreshing `about.md`,
/// which lists the machinery the root points at.
pub(crate) fn ensure_registry(ctx: &mut Ctx) -> Result<(), AnyError> {
    let Some(boot) = block_on(prov::ensure_registry(StdFs, ctx.as_root()))? else {
        return Ok(());
    };
    if boot.created {
        eprintln!(
            "initialized {} (linked from {})",
            boot.path.display(),
            ctx.root_doc.display()
        );
        // A new machinery file the root now points at, which `about.md` lists
        // among the files the spine will never reach. Bootstrapping one is a
        // change to the workspace's declared structure, not to its contents, so
        // it is squarely inside what the page describes.
        refresh_about(&ctx.root_dir)?;
    }
    ctx.registry = Some(boot.path);
    Ok(())
}

/// Persist a mutation's identity changes according to the workspace's
/// [`IdStorage`](prov::IdStorage) mode — [`Workspace::persist_identity`].
fn persist(ctx: &Ctx, ws: &mut Workspace<StdFs, Minter, FileIndex>) -> Result<(), AnyError> {
    Ok(block_on(ws.persist_identity(ctx.as_root()))?)
}

/// How a CLI argument names a document — the addressing mode carried by the
/// *value*, not by which flag it was passed to.
///
/// This mirrors the library's [`Addressing`](prov::Addressing) (`Path`/`Id`/
/// `Alias`) and its `Link::parse`, which have always disambiguated a target by its
/// own syntax. The CLI briefly did it with flag names instead (`--in-path` vs
/// `--in-title`), which cost a flag per mode per argument and could only ever be
/// afforded on *one* argument — the parent — leaving every subject path-only. A
/// grammar costs one flag total and works in every slot, including subjects.
///
/// The spellings are chosen so a bare path stays a bare path: `id:` is the
/// library's own [`ID_SCHEME`](prov::link::ID_SCHEME), and `@` is not legal at
/// the start of a *relative* path anyone writes by habit. A file genuinely named
/// `@foo.md` is still addressable as `./@foo.md`, which parses as a path.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TargetSpec<'a> {
    /// A filesystem path — the default, and the only mode that needs no workspace.
    Path(&'a str),
    /// `id:<id>` (or the legacy `prov:<id>`) — resolved through the registry.
    Id(&'a str),
    /// `@Daily/2026/08` — a route of titles walked from the workspace root. Bare
    /// `@` is the root document itself.
    Route(&'a str),
}

/// Classify a CLI target. Pure text: no filesystem, no workspace, no guessing —
/// the string says which mode it is or it is a path.
pub(crate) fn parse_target(s: &str) -> TargetSpec<'_> {
    if let Some(id) = link::strip_id_scheme(s) {
        return TargetSpec::Id(id);
    }
    match s.strip_prefix('@') {
        Some(route) => TargetSpec::Route(route),
        None => TargetSpec::Path(s),
    }
}

/// Resolve a target that names an *existing* document, to a path this process can
/// open (absolute for id/route, as-written for a path).
///
/// Root discovery is **lazy**: a plain path resolves without one, so `show`,
/// `meta`, `get`, `body`, `links`, `render`, `set`, and `unset` keep working on any
/// file anywhere — outside a workspace, in a tarball, wherever. Only `@` and `id:`
/// need a workspace, and only then is one discovered. That property is worth
/// keeping: those commands read a *file*, and only the other modes make the
/// argument mean a *node*.
pub(crate) fn resolve_target(s: &str) -> Result<PathBuf, AnyError> {
    match parse_target(s) {
        TargetSpec::Path(p) => Ok(PathBuf::from(p)),
        TargetSpec::Id(id) => {
            let Session { ctx, ws } = Session::open()?;
            let id = Id(id.to_string());
            match ws.index().resolve(&id) {
                Some(path) => Ok(ctx.root_dir.join(path)),
                None if ws.index().is_tombstoned(&id) => {
                    Err(format!("{id} is tombstoned — its document was deleted").into())
                }
                None => Err(format!("{id} is not in the registry").into()),
            }
        }
        TargetSpec::Route(route) => {
            let Session { ctx, ws } = Session::open()?;
            let terminal = resolve_route(&ctx, &ws, route)?;
            Ok(ctx.root_dir.join(terminal))
        }
    }
}

/// Walk a route of titles to an existing node, workspace-relative. Refuses to
/// create: a *subject* that does not exist is a mistake, never an instruction —
/// only a `--in` destination may be synthesized, and only with `-p`.
fn resolve_route(
    ctx: &Ctx,
    ws: &Workspace<StdFs, Minter, FileIndex>,
    route: &str,
) -> Result<PathBuf, AnyError> {
    let segments = Workspace::<StdFs>::route_segments(route);
    let plan = block_on(ws.plan_route(&ctx.root_doc, &segments, Layout::Nested))?;
    if !plan.is_complete() {
        let missing = &plan.synthesize[0];
        return Err(format!(
            "@{route} stops at {}: no child titled {:?}",
            missing.parent.display(),
            missing.title,
        )
        .into());
    }
    Ok(plan.terminal)
}

/// Re-anchor a (cwd-relative) CLI path to the discovered workspace root.
pub(crate) fn ws_rel(ctx: &Ctx, path: &Path) -> Result<PathBuf, AnyError> {
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
pub(crate) fn entropy_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::hash::RandomState::new().build_hasher().finish()
}

pub(crate) fn load(file: &Path) -> Result<(String, Document), Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(file)?;
    let doc = Document::parse(file, &text)?;
    Ok((text, doc))
}
