//! `config` — read and write the workspace's policy.
//!
//! Policy has two homes, the root's `prov:` block and the linked config
//! document, and reads always span both. This module is where the two are
//! written: a single key (`config <key> <value>`, validated by the same
//! diagnostic `check` runs), the whole effective config materialized
//! (`--setup`), or the declared policy moved from one home to the other
//! (`--home`). [`write_config_setting`] is the write half on its own, for a
//! command that sets one axis on the user's behalf.

use std::path::PathBuf;
use std::process::ExitCode;

use prov::{Document, Format, Mapping, StdFs, Value, Workspace, block_on, edit, meta};

use crate::about::refresh_about;
use crate::cli::{CONFIG_STEM, ConfigHome, sidecar_name};
use crate::session::{Ctx, find_root_quiet};
use crate::{AnyError, CmdResult};

/// Look up a dotted key (`references.notation`) in a nested config mapping,
/// descending one mapping per segment.
fn lookup_dotted<'a>(map: &'a Mapping, dotted: &str) -> Option<&'a Value> {
    let mut segments = dotted.split('.');
    let mut current = map.get(segments.next()?)?;
    for seg in segments {
        current = current.get(seg)?;
    }
    Some(current)
}

/// Build the nested probe a dotted `config <key> <value>` implies, so `diagnose`
/// validates `references.notation=wikilink` as the nested shape it understands
/// rather than reading `references.notation` as one unknown top-level key.
fn nest_probe(dotted: &str, value: Value) -> Value {
    let mut node = value;
    for key in dotted.rsplit('.') {
        let mut m = Mapping::new();
        m.insert(key.to_string(), node);
        node = Value::Mapping(m);
    }
    node
}

/// Materialize the full effective config explicitly into the config document:
/// every setting written at its current-or-default value, so a workspace never
/// relies on invisible defaults. Bootstraps `prov.yaml` if none is linked,
/// preserves the document's own fields (title/part_of and any user fields) and
/// every setting already present (those are already in the effective config),
/// and fills in the rest. Canonicalizes layout (comments in the config document
/// are not preserved).
fn cmd_config_setup(mut ctx: Ctx) -> CmdResult {
    let config_doc = ensure_config(&mut ctx)?;
    let full = ctx.root_dir.join(&config_doc);
    let text = std::fs::read_to_string(&full)?;
    let doc = Document::parse(&config_doc, &text)?;
    let policy = ctx.config.to_mapping();
    // Keep the document's non-policy fields (title, part_of, user fields) in
    // place, then write every effective policy key explicitly after them.
    let mut map = Mapping::new();
    if let Some(existing) = doc.meta.as_mapping() {
        for (k, v) in existing {
            if !policy.contains_key(k) {
                map.insert(k.clone(), v.clone());
            }
        }
    }
    let count = policy.len();
    for (k, v) in policy {
        map.insert(k, v);
    }
    std::fs::write(
        &full,
        meta::serialize_mapping(&map, ctx.config.default_embed_format)?,
    )?;
    println!(
        "wrote {count} explicit setting(s) to {}",
        config_doc.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// Relocate the workspace's declared policy to a single home (`config --home`).
/// A *move*, not a materialization: only the *recognized policy* keys declared
/// across the two surfaces travel — no defaults baked in, and user fields stay
/// put — so the effective config is unchanged, just consolidated. Reads span both
/// homes regardless (`Ctx`); this only chooses where the bytes live.
fn cmd_config_home(mut ctx: Ctx, home: ConfigHome) -> CmdResult {
    // The recognized policy vocabulary: the keys `WorkspaceConfig` round-trips.
    // Anything else in a surface (a user field, a stray note) is not policy and
    // must not travel — so it is what the move ignores and what the delete guards.
    let recognized: std::collections::HashSet<String> =
        ctx.config.to_mapping().keys().cloned().collect();
    let declared = collect_declared_policy(&ctx, &recognized)?;
    match home {
        ConfigHome::Sidecar => move_policy_to_sidecar(&mut ctx, &declared, &recognized),
        ConfigHome::Root => move_policy_to_root(&mut ctx, &declared, &recognized),
    }
}

type KeySet = std::collections::HashSet<String>;

/// The recognized policy declared across both homes — the root's inline `prov:`
/// block with the sidecar's policy overlaid (the effective precedence *config
/// document > root block*), filtered to `recognized` so only policy travels.
/// Deep-merged, so a nested block present in both (e.g. `references`) combines
/// key-by-key rather than one home's block wholesale replacing the other's —
/// matching how `WorkspaceConfig::apply` layers.
fn collect_declared_policy(ctx: &Ctx, recognized: &KeySet) -> Result<Mapping, AnyError> {
    let mut merged = Mapping::new();
    let root_full = ctx.root_dir.join(&ctx.root_doc);
    if let Ok(text) = std::fs::read_to_string(&root_full)
        && let Ok(doc) = Document::parse(&ctx.root_doc, &text)
        && let Some(Value::Mapping(block)) = doc.meta.get(prov::config::ROOT_CONFIG_KEY)
    {
        merged = filter_keys(block, recognized);
    }
    let probe: Workspace<StdFs> = Workspace::builder(StdFs).root(&ctx.root_dir).build();
    if let Some(config_doc) = block_on(probe.config_path(&ctx.root_doc))? {
        let full = ctx.root_dir.join(&config_doc);
        let text = std::fs::read_to_string(&full)?;
        let doc = Document::parse(&config_doc, &text)?;
        if let Some(map) = doc.meta.as_mapping() {
            deep_merge(&mut merged, &filter_keys(map, recognized));
        }
    }
    Ok(merged)
}

/// A copy of `map` keeping only top-level keys in `keys`.
fn filter_keys(map: &Mapping, keys: &KeySet) -> Mapping {
    map.iter()
        .filter(|(k, _)| keys.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// A copy of `map` dropping the top-level keys in `keys`.
fn drop_keys(map: &Mapping, keys: &KeySet) -> Mapping {
    map.iter()
        .filter(|(k, _)| !keys.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Recursively overlay `overlay` onto `base`: a mapping-valued key present in both
/// merges key-by-key; every other key is replaced. The deep counterpart of
/// `Mapping::extend`, so `references: { notation }` in one home and
/// `references: { target }` in the other combine rather than clobber.
fn deep_merge(base: &mut Mapping, overlay: &Mapping) {
    for (key, value) in overlay {
        match (base.get_mut(key), value) {
            (Some(Value::Mapping(base_inner)), Value::Mapping(overlay_inner)) => {
                deep_merge(base_inner, overlay_inner);
            }
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Rewrite the root's `prov:` block to `keep` its non-policy fields only (the
/// recognized policy having moved out): if nothing remains, remove the `prov:`
/// key entirely; otherwise set it to the remainder. The root's body and all other
/// fields are preserved.
fn strip_root_policy(ctx: &Ctx, recognized: &KeySet) -> Result<(), AnyError> {
    let root_full = ctx.root_dir.join(&ctx.root_doc);
    let text = std::fs::read_to_string(&root_full)?;
    let doc = Document::parse(&ctx.root_doc, &text)?;
    let Some(Value::Mapping(block)) = doc.meta.get(prov::config::ROOT_CONFIG_KEY) else {
        return Ok(());
    };
    let remainder = drop_keys(block, recognized);
    let updated = if remainder.is_empty() {
        edit::unset_in_text(&text, doc.carrier, prov::config::ROOT_CONFIG_KEY)?
    } else {
        edit::set_meta_in_text(
            &text,
            doc.carrier,
            prov::config::ROOT_CONFIG_KEY,
            &Value::Mapping(remainder),
        )?
    };
    std::fs::write(&root_full, updated)?;
    Ok(())
}

/// `config --home sidecar`: write the declared policy into `prov.yaml` (creating
/// and linking it if absent), preserving the sidecar's own `title`/`part_of` and
/// any non-policy fields, then strip the recognized policy from the root's `prov:`
/// block. Comments in the config document are not preserved (rebuilt canonically,
/// like `--setup`).
fn move_policy_to_sidecar(ctx: &mut Ctx, declared: &Mapping, recognized: &KeySet) -> CmdResult {
    let config_doc = ensure_config(ctx)?;
    let full = ctx.root_dir.join(&config_doc);
    let text = std::fs::read_to_string(&full)?;
    let doc = Document::parse(&config_doc, &text)?;
    // Keep every non-policy field the sidecar already has (title, part_of, and any
    // hand-added content), then write the policy after it.
    let mut map = doc
        .meta
        .as_mapping()
        .map(|m| drop_keys(m, recognized))
        .unwrap_or_default();
    for (k, v) in declared {
        map.insert(k.clone(), v.clone());
    }
    std::fs::write(
        &full,
        meta::serialize_mapping(&map, ctx.config.default_embed_format)?,
    )?;
    strip_root_policy(ctx, recognized)?;
    println!(
        "moved workspace policy to {} and cleared the root `prov:` block",
        config_doc.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// `config --home root`: merge the declared policy into the root's `prov:` block
/// (preserving any non-policy field already there), then retire the sidecar. If
/// stripping the policy leaves the sidecar with only its `title`/`part_of`, it is
/// deleted and its `config:` pointer removed; if it still carries hand-added
/// fields, it is kept (rewritten without the moved policy) so nothing is lost.
fn move_policy_to_root(ctx: &mut Ctx, declared: &Mapping, recognized: &KeySet) -> CmdResult {
    let root_full = ctx.root_dir.join(&ctx.root_doc);
    let text = std::fs::read_to_string(&root_full)?;
    let doc = Document::parse(&ctx.root_doc, &text)?;
    let mut block = match doc.meta.get(prov::config::ROOT_CONFIG_KEY) {
        Some(Value::Mapping(m)) => m.clone(),
        _ => Mapping::new(),
    };
    for (k, v) in declared {
        block.insert(k.clone(), v.clone());
    }
    let updated = edit::set_meta_in_text(
        &text,
        doc.carrier,
        prov::config::ROOT_CONFIG_KEY,
        &Value::Mapping(block),
    )?;
    std::fs::write(&root_full, updated)?;

    let probe: Workspace<StdFs> = Workspace::builder(StdFs).root(&ctx.root_dir).build();
    if let Some(config_doc) = block_on(probe.config_path(&ctx.root_doc))? {
        let sidecar_full = ctx.root_dir.join(&config_doc);
        let sidecar_text = std::fs::read_to_string(&sidecar_full)?;
        let sidecar = Document::parse(&config_doc, &sidecar_text)?;
        let remainder = sidecar
            .meta
            .as_mapping()
            .map(|m| drop_keys(m, recognized))
            .unwrap_or_default();
        let only_self_describing = remainder.keys().all(|k| k == "title" || k == "part_of");
        if only_self_describing {
            // The sidecar is now empty of meaning — remove its pointer and delete it.
            let text = std::fs::read_to_string(&root_full)?;
            let doc = Document::parse(&ctx.root_doc, &text)?;
            if doc.meta.get("config").is_some() {
                let updated = edit::unset_in_text(&text, doc.carrier, "config")?;
                std::fs::write(&root_full, updated)?;
            }
            std::fs::remove_file(&sidecar_full)?;
            println!(
                "moved workspace policy into the root `prov:` block and removed {}",
                config_doc.display()
            );
        } else {
            // Hand-added fields remain — keep the sidecar, just without the policy.
            std::fs::write(
                &sidecar_full,
                meta::serialize_mapping(&remainder, ctx.config.default_embed_format)?,
            )?;
            let kept: Vec<String> = remainder
                .keys()
                .filter(|k| k.as_str() != "title" && k.as_str() != "part_of")
                .cloned()
                .collect();
            println!(
                "moved workspace policy into the root `prov:` block; kept {} for its non-policy field(s): {}",
                config_doc.display(),
                kept.join(", ")
            );
        }
    } else {
        println!("moved workspace policy into the root `prov:` block");
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_config(
    key: Option<&str>,
    value: Option<&str>,
    setup: bool,
    home: Option<ConfigHome>,
) -> CmdResult {
    let ctx = find_root_quiet()?;
    // Both of these can change what the page says even though neither sets an
    // axis: `--home` moves policy between the two homes (and may delete or
    // create the sidecar the footer names), and `--setup` bootstraps a config
    // document where none was linked. The page names its config document, so
    // either one can leave it describing a file that is no longer there.
    if setup {
        let root_dir = ctx.root_dir.clone();
        let code = cmd_config_setup(ctx)?;
        refresh_about(&root_dir)?;
        return Ok(code);
    }
    if let Some(home) = home {
        let root_dir = ctx.root_dir.clone();
        let code = cmd_config_home(ctx, home)?;
        refresh_about(&root_dir)?;
        return Ok(code);
    }
    match (key, value) {
        // No key: print the effective config (defaults + root + config document).
        (None, _) => {
            print!(
                "{}",
                meta::serialize_mapping(&ctx.config.to_mapping(), Format::Yaml)?
            );
        }
        // Key only: read that value from the *effective* config (defaults + root
        // frontmatter + config document), so it agrees with the no-key form
        // above. Reading the config document alone would report "not set" for a
        // value that comes from root frontmatter (the diaryx-compat path) or
        // stands at its default — a divergence between the two forms.
        (Some(key), None) => {
            let effective = ctx.config.to_mapping();
            // Dotted keys address nested axes (`references.notation`).
            match lookup_dotted(&effective, key) {
                Some(v) => match v.as_str() {
                    Some(s) => println!("{s}"),
                    None => println!("{}", meta::serialize_value(v, Format::Yaml)?.trim_end()),
                },
                None => {
                    eprintln!("prov: {key} is not set");
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        // Key + value: materialize/link the config document if needed, then set.
        (Some(key), Some(value)) => {
            let mut ctx = ctx;
            // The scalar the text implies (`true` → a bool, `12` → an int),
            // carried as prov's own value from here on — what `diagnose` reads
            // and what the write emits, so the setting that is judged is exactly
            // the setting that lands.
            //
            // `workspace_id` is exempt because it is a *name*, and a name that
            // happens to be spelled with digits is still a name. The mint's
            // alphabet includes the digits (`prov id --workspace` can hand back
            // `123456789012`), so inferring an int here would have prov refuse
            // to set a name it had just minted itself.
            let inferred: Value = if key == "workspace_id" {
                Value::String(value.to_string())
            } else if key == "out_of_scope" {
                // The one sequence-valued axis reachable from here, so the one
                // that needs a spelling a shell can produce: comma-separated,
                // because the alternative is asking a person to hand-edit YAML
                // for the axis whose whole purpose is to be set once by someone
                // who just noticed another tool's folder beside their notes.
                // An empty value clears the list rather than declaring `""`.
                Value::Sequence(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|dir| !dir.is_empty())
                        .map(|dir| Value::String(dir.to_string()))
                        .collect(),
                )
            } else {
                edit::infer_scalar(value).into()
            };
            // Refuse to write a setting prov would silently ignore — the same
            // conditions `check` flags (a key that resembles a real axis but
            // isn't, or a recognized axis with an unrecognized value). Running the
            // shared diagnostic over a one-key probe keeps set-time and check-time
            // judgments identical. A truly novel key (resembling no axis) is left
            // to pass — it may be a user field or a forward-compatible key.
            let probe = nest_probe(key, inferred.clone());
            if let Some(issue) = prov::diagnose(&probe).into_iter().next() {
                match issue.kind {
                    prov::ConfigIssueKind::UnknownKey { suggestion } => {
                        eprintln!(
                            "prov: unknown config key `{key}` — did you mean `{suggestion}`?"
                        );
                    }
                    prov::ConfigIssueKind::InvalidValue { value, expected } => {
                        eprintln!(
                            "prov: `{value}` is not a valid {key} (expected: {})",
                            expected.join(", ")
                        );
                    }
                    prov::ConfigIssueKind::SpanningNotSingleParent { inverse } => {
                        eprintln!(
                            "prov: spanning relation's inverse `{inverse}` must be `cardinality: one` to form a single-parent tree"
                        );
                    }
                    prov::ConfigIssueKind::MalformedWorkspaceId { value } => {
                        eprintln!(
                            "prov: `{value}` is not a valid workspace name — it cannot be empty or contain `/`, `:` or whitespace"
                        );
                    }
                    prov::ConfigIssueKind::MalformedRoot { value } => {
                        eprintln!(
                            "prov: `{value}` is not a valid root name — name the root as a bare file name in the node's own directory, with no `/`"
                        );
                    }
                    // Not reachable from a one-key probe (this needs a `fields`
                    // declaration alongside the view), but spelled out rather
                    // than wildcarded so a new issue kind arrives here as a
                    // compile error.
                    prov::ConfigIssueKind::NestNotSingleValued { field } => {
                        eprintln!(
                            "prov: cannot nest by `{field}` — it is declared `type: seq`, and a document with several values has several homes"
                        );
                    }
                }
                return Ok(ExitCode::FAILURE);
            }
            let config_doc = write_config_setting(&mut ctx, key, inferred)?;
            eprintln!("set {key} = {value} in {}", config_doc.display());
            refresh_about(&ctx.root_dir)?;
            // Echo the value now in effect, so `v=$(prov config set …)` round-trips
            // with `prov config <key>`.
            println!("{value}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Write one setting into the workspace's config document, bootstrapping and
/// linking it first if the workspace declares none. Returns the config
/// document's path relative to the root, for the caller to narrate.
///
/// The write half of `prov config <key> <value>`, factored out so anything that
/// sets a single axis on the user's behalf (`prov id --workspace`) lands in the
/// same file, through the same editor, as if they had set it by hand. The
/// *validation* half stays with `config`: this takes a [`Value`] already decided
/// on, so a caller that knows the exact scalar it wants (a minted workspace
/// name, which must stay a string) is not forced back through scalar inference.
pub(crate) fn write_config_setting(
    ctx: &mut Ctx,
    key: &str,
    value: Value,
) -> Result<PathBuf, AnyError> {
    let config_doc = ensure_config(ctx)?;
    let full = ctx.root_dir.join(&config_doc);
    let text = std::fs::read_to_string(&full)?;
    let doc = Document::parse(&config_doc, &text)?;
    let updated = edit::set_in_text(&text, doc.carrier, key, (&value).into())?;
    std::fs::write(&full, updated)?;
    Ok(config_doc)
}

/// Ensure the workspace *declares* a config document, bootstrapping one when it
/// does not: create `prov.<ext>` (in the workspace's metadata format) beside
/// the root (self-described with a title) and add the `config` pointer to the
/// root's metadata. Returns its path relative to the root. Mirrors
/// [`ensure_registry`](crate::session::ensure_registry), including its change set: a config document the root does
/// not point at is one nothing will ever read. Like the registry, it carries no
/// `part_of` — machinery is reached one-way through the root's pointer (DESIGN §5).
fn ensure_config(ctx: &mut Ctx) -> Result<PathBuf, AnyError> {
    let probe: Workspace<StdFs> = Workspace::builder(StdFs).root(&ctx.root_dir).build();
    if let Some(existing) = block_on(probe.config_path(&ctx.root_doc))? {
        return Ok(existing);
    }
    let format = ctx.config.default_embed_format;
    let config_rel = PathBuf::from(sidecar_name(CONFIG_STEM, format));

    let mut seed = Mapping::new();
    seed.insert("title".into(), Value::String("prov config".into()));

    let created =
        block_on(probe.link_sidecar(&ctx.root_doc, "config", &config_rel, &seed, format))?;
    if created {
        eprintln!(
            "initialized {} (linked from {})",
            config_rel.display(),
            ctx.root_doc.display()
        );
    }
    Ok(config_rel)
}
