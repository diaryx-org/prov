//! `ignore` — what a tool copying or syncing this folder should leave alone.
//!
//! The difference between what is on disk and what the graph reaches, spelled
//! as ignore-file rules. [`reason_word`] is shared with the JSON printer so
//! `--why` and `--json` never drift into different vocabularies for the same
//! fact.

use std::process::ExitCode;

use prov::block_on;

use crate::CmdResult;
use crate::json;
use crate::session::{find_root, workspace};

/// List what a tool copying, syncing or recording this folder should leave
/// alone: the difference between what is on disk and what the graph reaches.
///
/// Gated on nothing. Asking what the workspace fails to reach is a question
/// about the workspace, and the command writes nothing — the tool that
/// consumes the list owns every decision after this one.
///
/// The rules go to stdout (they are an ignore file's content, for `>` and
/// `diff` and friends); the count said *about* them goes to stderr.
pub(crate) fn cmd_ignore(why: bool, as_json: bool) -> CmdResult {
    let ctx = find_root()?;
    let ws = workspace(&ctx)?;
    let list = block_on(ws.ignore_list(&ctx.root_doc))?;

    if as_json {
        print!(
            "{}",
            json::J::Arr(list.rules.iter().map(json::ignore).collect()).render()
        );
        return Ok(ExitCode::SUCCESS);
    }
    // With `--why` the rules are grouped under a comment naming their reason,
    // rather than annotated one by one: gitignore reads `#` only at the start
    // of a line, so a trailing note would become part of the pattern and the
    // file would stop meaning what it says.
    let mut ordered: Vec<_> = list.rules.iter().collect();
    if why {
        ordered.sort_by_key(|rule| (rule.reason, rule.path.clone()));
    }
    let mut said: Option<prov::Reason> = None;
    for rule in ordered {
        if why && said != Some(rule.reason) {
            if said.is_some() {
                println!();
            }
            println!("# {}", reason_word(rule.reason));
            said = Some(rule.reason);
        }
        println!("{rule}");
    }
    // The count is narration for a person reading a terminal; `--json` is the
    // mode where nobody is, and it has already returned.
    match list.is_empty() {
        true => eprintln!("nothing to ignore — the graph reaches everything on disk"),
        false => eprintln!("{} rule(s)", list.rules.len()),
    }
    Ok(ExitCode::SUCCESS)
}

/// The one-word spelling of a reason, shared by `--why` and `--json` so the
/// two never drift into different vocabularies for the same fact.
pub(crate) fn reason_word(reason: prov::Reason) -> &'static str {
    match reason {
        prov::Reason::Bookkeeping => "bookkeeping",
        prov::Reason::Claimed => "claimed by a manifest",
        prov::Reason::Declared => "declared out of scope",
        prov::Reason::Hidden => "hidden",
        prov::Reason::Unreached => "unreached",
    }
}
