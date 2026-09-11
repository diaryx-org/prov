//! Who is at the keyboard — the actor a confirmation is attributed to.
//!
//! A fact about a device, not about the archive: the same workspace is opened
//! from several machines by several people, and which of them is confirming is
//! known only where the command runs. So it is resolved the way the peer map
//! is, and never read from the workspace's config:
//!
//! 1. `--by <ACTOR>` on the command
//! 2. `PROV_ACTOR`
//! 3. an `actor` file beside the peer map — one line, the actor — under
//!    `XDG_CONFIG_HOME/prov/` or the platform default
//!
//! None of those is not an anonymous confirmation; it is no confirmation. An
//! unattributed entry is exactly the free-floating assurance the family exists
//! to replace, so the command refuses rather than guessing.

use std::path::PathBuf;

/// The file's name inside whichever directory holds the peer map.
const FILE: &str = "actor";

/// The device-local actor file, if this invocation has somewhere to keep one.
pub(crate) fn path() -> Option<PathBuf> {
    crate::peer::path().map(|peers| peers.with_file_name(FILE))
}

/// Resolve the actor for this invocation, or say where one could be set.
pub(crate) fn resolve(flag: Option<String>) -> Result<String, String> {
    if let Some(actor) = flag.map(|a| a.trim().to_string()).filter(|a| !a.is_empty()) {
        return Ok(actor);
    }
    if let Some(actor) = std::env::var("PROV_ACTOR")
        .ok()
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
    {
        return Ok(actor);
    }
    if let Some(file) = path()
        && let Ok(text) = std::fs::read_to_string(&file)
        && let Some(actor) = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
    {
        return Ok(actor.to_string());
    }
    let hint = match path() {
        Some(file) => format!(", or write it to {}", file.display()),
        None => String::new(),
    };
    Err(format!(
        "no actor to attribute this confirmation to — pass --by <ACTOR> or set PROV_ACTOR{hint}. \
         A bare name is a person; a tool prefixes itself `agent:` or `process:`"
    ))
}
