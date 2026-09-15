//! `prov` — command-line companion for the prov library.
//!
//! A thin adapter: parse arguments, call into the library, render the result.
//! The workspace *semantics* — discovery, bootstrap, the mutation engine — live
//! in `prov`; this crate is argument parsing, session plumbing, and
//! presentation.
//!
//! The crate is one module per concern, so that each stays legible:
//!
//! - [`cli`] — the `clap` argument grammar and the enums that mirror the
//!   library's config axes (the CLI *spelling* of each concept).
//! - [`session`] — how a command finds the workspace and opens the library's
//!   [`prov::StdFs`]-backed engine over it, through the dependency-free
//!   [`prov::block_on`] executor; and how a CLI argument names a document.
//! - one module per verb or family of verbs — [`doc`] for the
//!   single-document commands, [`structure`] for the mutations of the
//!   containment tree, [`check`], [`config`], [`init`], and so on — each
//!   opening with a note on what it is and where its boundary lies.
//! - the device-local state the library deliberately does not own:
//!   [`peer`] (where the other workspaces are) and [`actor`] (who is at the
//!   keyboard).
//! - `main` (here) — the dispatcher: one `match` from a parsed [`Command`] to
//!   the function that runs it, and nothing else.
//!
//! Single-document commands (`show`, `links`, `meta`, `get`, `body`, `set`,
//! `unset`) operate on the pure layers and need no workspace; workspace commands
//! (`tree`, `check`, `new`, `mv`, `rm`, …) discover a root first.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

mod about;
mod actor;
mod backup;
mod check;
mod cli;
mod clock;
mod config;
mod convert;
mod doc;
mod explore;
mod identity;
mod ignore;
mod init;
mod json;
mod peer;
mod session;
mod stamp;
mod structure;
mod term;
mod tree;
mod views;
mod zip;

use cli::{Cli, Command};

/// What every command returns: the process exit code, or an error `main`
/// prints as `prov: <err>` and turns into a failure.
type CmdResult = Result<ExitCode, Box<dyn std::error::Error>>;

/// The error type the CLI's own functions use — boxed, because a command's
/// failure is reported as a line of text and never matched on.
type AnyError = Box<dyn std::error::Error>;

fn main() -> ExitCode {
    let cli = Cli::parse();
    // `-C <dir>` / `--root <dir>` (or `PROV_ROOT`, which it overrides) runs prov
    // as if it had started in that directory: chdir once, up front, so every
    // downstream `current_dir()`-based root discovery and every relative path
    // argument resolves there — the `git -C` model, in one place.
    if let Some(dir) = cli
        .root
        .clone()
        .or_else(|| std::env::var_os("PROV_ROOT").map(PathBuf::from))
        && let Err(e) = std::env::set_current_dir(&dir)
    {
        eprintln!("prov: could not use root directory {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    // Resolved once, before any command runs, for the same reason `-C` is: it is
    // a property of the invocation, not of any one verb.
    peer::init(cli.peers.clone());
    let result = match cli.command {
        Command::Peer { action } => peer::cmd_peer(action),
        Command::Show { file } => session::resolve_target(&file).and_then(|f| doc::cmd_show(&f)),
        Command::Links { file, relation } => {
            session::resolve_target(&file).and_then(|f| doc::cmd_links(&f, relation.as_deref()))
        }
        Command::Meta { file, format } => {
            session::resolve_target(&file).and_then(|f| doc::cmd_meta(&f, format))
        }
        Command::Get { file, key } => {
            session::resolve_target(&file).and_then(|f| doc::cmd_get(&f, &key))
        }
        Command::Body { file } => session::resolve_target(&file).and_then(|f| doc::cmd_body(&f)),
        Command::Render { file } => {
            session::resolve_target(&file).and_then(|f| doc::cmd_render(&f))
        }
        Command::Init(args) => init::cmd_init(args),
        Command::Edit { file } => session::resolve_target(&file).and_then(|f| doc::cmd_edit(&f)),
        Command::Set { file, key, value } => {
            session::resolve_target(&file).and_then(|f| doc::cmd_set(&f, &key, &value))
        }
        Command::Unset { file, key } => {
            session::resolve_target(&file).and_then(|f| doc::cmd_unset(&f, &key))
        }
        Command::Views { name, json } => views::cmd_views(name.as_deref(), json),
        Command::Docs { json } => views::cmd_docs(json),
        Command::Exports { name } => views::cmd_exports(name.as_deref()),
        Command::Presets { dir, write } => views::cmd_presets(dir.as_deref(), write),
        Command::Tree {
            root,
            follow,
            unverified,
        } => root
            .map(|r| session::resolve_target(&r))
            .transpose()
            .and_then(|r| tree::cmd_tree(r.as_deref(), follow, unverified)),
        Command::Explore { file, unverified } => explore::cmd_explore(file.as_deref(), unverified),
        Command::Check(args) => check::cmd_check(args),
        Command::Confirm { target, by, show } => {
            session::resolve_target(&target).and_then(|t| stamp::cmd_confirm(&t, by, show))
        }
        Command::Stamp {
            target,
            all,
            no_timestamp,
            dry_run,
        } => target
            .map(|t| session::resolve_target(&t))
            .transpose()
            .and_then(|t| stamp::cmd_stamp(t.as_deref(), all, no_timestamp, dry_run)),
        Command::New(args) => structure::cmd_new(args),
        Command::Attach(args) => structure::cmd_attach(args),
        Command::Manifest {
            target,
            update,
            verify,
        } => check::cmd_manifest(&target, update, verify),
        Command::Mv {
            from,
            to,
            in_target,
            parents,
            layout,
        } => structure::cmd_mv(&from, &to, in_target.as_deref(), parents, layout.into()),
        Command::Reparent {
            path,
            in_target,
            parents,
            layout,
            dry_run,
        } => structure::cmd_reparent(&path, &in_target, parents, layout.into(), dry_run),
        Command::Rm { path, force } => structure::cmd_rm(&path, force),
        Command::Restore { path } => structure::cmd_restore(&path),
        Command::ClearDeletions => structure::cmd_clear_deletions(),
        Command::Duplicate { source } => structure::cmd_duplicate(&source),
        Command::Convert {
            file,
            axis,
            value,
            recursive,
            force,
        } => session::resolve_target(&file)
            .and_then(|f| convert::cmd_convert(&f, &axis, &value, recursive, force)),
        Command::Id { file, workspace } => match workspace {
            Some(name) => identity::cmd_id_workspace(name.as_deref()),
            // `required_unless_present` makes this unreachable without the flag.
            None => session::resolve_target(&file.unwrap_or_default())
                .and_then(|f| identity::cmd_id(&f)),
        },
        Command::Resolve { id } => identity::cmd_resolve(&id),
        Command::Backlinks { file } => {
            session::resolve_target(&file).and_then(|f| identity::cmd_backlinks(&f))
        }
        Command::Config {
            key,
            value,
            setup,
            home,
        } => config::cmd_config(key.as_deref(), value.as_deref(), setup, home),
        Command::Backup { to, zip } => backup::cmd_backup(&to, zip),
        Command::About { check, print } => about::cmd_about(check, print),
        Command::Ignore { why, json } => ignore::cmd_ignore(why, json),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("prov: {err}");
            ExitCode::FAILURE
        }
    }
}
