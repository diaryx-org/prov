//! Talking to the terminal: a pager, an editor, a question.
//!
//! The three places the CLI hands control to something interactive, kept
//! together so each reads its environment the same way — `$PAGER` and
//! `$EDITOR`/`$VISUAL` with the conventional fallbacks, and a prompt that
//! reads EOF as an empty answer so a non-interactive run declines rather than
//! hangs.

use std::path::Path;

use crate::AnyError;

/// Page `text` through `$PAGER` (default `less`), falling back to a plain print
/// when no pager can be spawned.
pub(crate) fn page_text(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".to_string());
    let mut parts = pager.split_whitespace();
    let Some(program) = parts.next() else {
        print!("{text}");
        return Ok(());
    };
    let spawned = std::process::Command::new(program)
        .args(parts)
        .stdin(std::process::Stdio::piped())
        .spawn();
    match spawned {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            child.wait()?;
        }
        Err(_) => print!("{text}"),
    }
    Ok(())
}

/// Open `path` in `$EDITOR`/`$VISUAL` (default `vi`), inheriting the terminal.
pub(crate) fn edit_file(path: &Path) -> std::io::Result<()> {
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    std::process::Command::new(program)
        .args(parts)
        .arg(path)
        .status()?;
    Ok(())
}

/// Prompt on stderr, read a trimmed, lowercased line from stdin (EOF → empty).
pub(crate) fn prompt(message: &str) -> Result<String, AnyError> {
    use std::io::Write;
    eprint!("{message}");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_lowercase())
}
