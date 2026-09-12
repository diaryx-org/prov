//! `convert` — rewrite a document (or a subtree) from one spelling of an axis
//! to another: link notation, path style, metadata format and embedding,
//! content format.
//!
//! Each axis composes with the workspace's current *other* axis, so the
//! command names one value and the library derives the full style. The
//! changed paths go to stdout, one per line, for `| git add` and friends.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use prov::{EmbedStyle, LinkStyle, Notation, PathStyle, block_on};

use crate::CmdResult;
use crate::session::{find_root, persist, workspace, ws_rel};

/// Report a convert sweep: the changed document paths to stdout (one per line,
/// for `| git add` and friends), the human count to stderr.
fn report_converted(changed: &[PathBuf], target: &str) {
    for path in changed {
        println!("{}", path.display());
    }
    eprintln!("converted {} document(s) to {target}", changed.len());
}

pub(crate) fn cmd_convert(
    file: &Path,
    axis: &str,
    value: &str,
    recursive: bool,
    force: bool,
) -> CmdResult {
    let ctx = find_root()?;
    let mut ws = workspace(&ctx)?;
    // Convert authors path links in a target [`LinkStyle`], which fuses the
    // notation (bracketed/bare) and path resolution. Each axis composes with the
    // workspace's current *other* axis; `wikilink` has no path rendering to
    // convert, so it is rejected here.
    match axis {
        "path_style" | "path-style" => {
            let ps = PathStyle::from_config_str(value)
                .ok_or_else(|| format!("unknown path_style `{value}` (expected root|relative)"))?;
            let style = LinkStyle::from_axes(ctx.config.notation, ps);
            let changed = block_on(ws.convert_link_style(&ws_rel(&ctx, file)?, style, recursive))?;
            persist(&ctx, &mut ws)?;
            report_converted(&changed, &format!("{value} path resolution"));
        }
        "notation" => {
            let nt = Notation::from_config_str(value)
                .ok_or_else(|| format!("unknown notation `{value}` (expected markdown|bare)"))?;
            if nt == Notation::Wikilink {
                return Err("convert: `wikilink` has no path rendering to convert".into());
            }
            let style = LinkStyle::from_axes(nt, ctx.config.path_style);
            let changed = block_on(ws.convert_link_style(&ws_rel(&ctx, file)?, style, recursive))?;
            persist(&ctx, &mut ws)?;
            report_converted(&changed, &format!("{value} notation"));
        }
        "metadata.format" | "metadata_format" | "format" => {
            let fmt = prov::metadata_format_from_str(value).ok_or_else(|| {
                format!("unknown metadata.format `{value}` (expected yaml|json|toml|fig)")
            })?;
            let changed = block_on(ws.convert_meta_format(&ws_rel(&ctx, file)?, fmt, recursive))?;
            persist(&ctx, &mut ws)?;
            report_converted(&changed, &format!("{value} frontmatter"));
        }
        "metadata.embed" | "metadata_embed" | "embed" => {
            let style = EmbedStyle::from_config_str(value).ok_or_else(|| {
                format!(
                    "unknown metadata.embed `{value}` \
                     (expected delimited|code_block|html_script|html_code)"
                )
            })?;
            let changed = block_on(ws.convert_meta_embed(&ws_rel(&ctx, file)?, style, recursive))?;
            persist(&ctx, &mut ws)?;
            report_converted(&changed, &format!("{value} embedding"));
        }
        "content_format" | "content-format" | "content" => {
            let fmt = prov::ContentFormat::from_config_str(value).ok_or_else(|| {
                format!("unknown content_format `{value}` (expected markdown|djot|html)")
            })?;
            let changed =
                block_on(ws.convert_content_format(&ws_rel(&ctx, file)?, fmt, recursive, force))?;
            persist(&ctx, &mut ws)?;
            // Unlike the other axes, this one moves the files it converts — the
            // paths reported are where each document now *is*, not where it was.
            report_converted(&changed, &format!("{value} prose"));
        }
        other => {
            return Err(format!(
                "convert: axis `{other}` is not supported (only `notation`, `path_style`, \
                 `metadata.format`, `metadata.embed`, and `content_format`)"
            )
            .into());
        }
    }
    Ok(ExitCode::SUCCESS)
}
