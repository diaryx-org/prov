//! `prov backup` — copy the whole workspace tree to another filesystem
//! location, for redundancy against losing the workspace's own location (a
//! dead disk, a deleted cloud folder).
//!
//! This is deliberately **outside the graph**: a plain, opaque, whole-tree copy
//! with no pointer relation, no manifest, no config axis, no dedup, no
//! reachability rule. The entire point is that it must not depend on anything
//! living *inside* the workspace — an imperative one-off action, not a standing
//! behavior, so there is nothing about it in the config document (compare
//! `mutate::recycle`, which *is* a graph citizen: a pointer relation, a
//! reachable folder, a config axis). Every file and directory under the root is
//! copied bytes-verbatim, including hidden files and a transient
//! `.prov-journal` (correct to copy: a restored copy rolls forward to a
//! consistent state via the existing recovery path, `prov check`/`recover`).
//!
//! It lives in the CLI, not the library, for the same reason: [`prov::fs::Storage`]
//! is a port sized to the scan/traverse/mutate engine's needs (an async,
//! backend-agnostic seam), not a general recursive-copy API, and backup's
//! whole design intent is to need nothing the workspace graph provides. The CLI
//! is free to reach for `std::fs` directly here — no journal, no `ChangeSet`,
//! no async — because there is no partial-write hazard to guard: nothing in the
//! *source* workspace is ever modified, so a crash mid-backup just leaves an
//! incomplete copy at the destination, not a broken workspace.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::CmdResult;
use crate::zip::ZipWriter;

/// Tally of what a backup copied, for the one-line narration on stderr.
#[derive(Default)]
struct Stats {
    files: u64,
    dirs: u64,
    symlinks: u64,
    skipped_symlinks: u64,
}

/// `prov backup --to <path> [--zip]`.
pub(crate) fn cmd_backup(to: &Path, zip: bool) -> CmdResult {
    let ctx = crate::find_root()?;
    let root_dir = &ctx.root_dir;

    let root_canon = effective_canonical(root_dir)?;
    let dest_canon = effective_canonical(to)?;
    if dest_canon.starts_with(&root_canon) {
        return Err(format!(
            "backup destination {} resolves inside the workspace root {} — refusing (it would copy into itself)",
            to.display(),
            root_dir.display()
        )
        .into());
    }

    check_destination(to, zip)?;

    let dest_display = if to.is_absolute() {
        to.to_path_buf()
    } else {
        std::env::current_dir()?.join(to)
    };

    let mut stats = Stats::default();
    let mut warnings = Vec::new();

    if zip {
        if let Some(parent) = to.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(to)?;
        let mut zw = ZipWriter::new(io::BufWriter::new(file));
        let root_name = root_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "workspace".to_string());
        let prefix = format!("{root_name}/");
        zw.add_dir(&prefix, dos_datetime_of(root_dir))?;
        add_dir_to_zip(&mut zw, root_dir, &prefix, &mut stats, &mut warnings)?;
        zw.finish()?;
    } else {
        std::fs::create_dir_all(to)?;
        copy_dir_contents(root_dir, to, &mut stats, &mut warnings)?;
    }

    for warning in &warnings {
        eprintln!("prov: warning: {warning}");
    }
    eprintln!(
        "backed up {} file(s), {} director{} ({} symlink(s){}) to {}{}",
        stats.files,
        stats.dirs,
        if stats.dirs == 1 { "y" } else { "ies" },
        stats.symlinks,
        if stats.skipped_symlinks > 0 {
            format!(", {} skipped", stats.skipped_symlinks)
        } else {
            String::new()
        },
        dest_display.display(),
        if zip { " (zip)" } else { "" }
    );
    println!("{}", dest_display.display());
    Ok(ExitCode::SUCCESS)
}

/// Resolve `path` to an absolute, symlink-free form usable for a
/// "does A contain B" comparison — *without* requiring `path` to exist. The
/// longest existing ancestor is canonicalized (resolving any symlinks in the
/// real part of the path); any trailing components that don't exist yet
/// (because `--to` names a backup that hasn't been created) are appended
/// literally, since a nonexistent path component can't itself be a symlink.
pub(crate) fn effective_canonical(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut probe = absolute.clone();
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(&probe) {
            Ok(mut canon) => {
                for part in suffix.into_iter().rev() {
                    canon.push(part);
                }
                return Ok(canon);
            }
            Err(e) => {
                let Some(name) = probe.file_name().map(|n| n.to_os_string()) else {
                    return Err(e);
                };
                suffix.push(name);
                probe = match probe.parent() {
                    Some(p) => p.to_path_buf(),
                    None => return Err(e),
                };
            }
        }
    }
}

/// Refuse an unsafe destination: an existing file (either mode would clobber
/// it), an existing non-empty directory (copy mode would merge into it,
/// silently mixing old and new contents), or — for `--zip` — an existing
/// directory at all (the zip archive needs a plain file path). An existing
/// *empty* directory is fine for the non-zip mode: `--to` can point at a
/// directory a caller already `mkdir`-ed.
fn check_destination(to: &Path, zip: bool) -> Result<(), String> {
    match std::fs::symlink_metadata(to) {
        Ok(meta) => {
            if meta.is_dir() {
                if zip {
                    Err(format!(
                        "{} already exists as a directory — --zip needs a file path",
                        to.display()
                    ))
                } else {
                    let non_empty = std::fs::read_dir(to)
                        .map_err(|e| format!("cannot read {}: {e}", to.display()))?
                        .next()
                        .is_some();
                    if non_empty {
                        Err(format!(
                            "{} already exists and is not empty — refusing to back up into it",
                            to.display()
                        ))
                    } else {
                        Ok(())
                    }
                }
            } else {
                Err(format!(
                    "{} already exists — refusing to overwrite it",
                    to.display()
                ))
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot check destination {}: {e}", to.display())),
    }
}

/// Directory entries in filename order, for a deterministic walk (stable
/// narration order, and a predictable ZIP entry order in tests).
fn read_sorted_dir(dir: &Path) -> io::Result<Vec<std::fs::DirEntry>> {
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(dir)?.collect::<io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    Ok(entries)
}

/// Recursively copy `src`'s contents into the already-created `dst` directory,
/// bytes verbatim. A symlink is recreated as a symlink (never followed) on
/// platforms that support it cheaply; elsewhere it is skipped with a warning
/// (see the module doc and `add_dir_to_zip` for the fuller symlink-policy
/// rationale).
fn copy_dir_contents(
    src: &Path,
    dst: &Path,
    stats: &mut Stats,
    warnings: &mut Vec<String>,
) -> io::Result<()> {
    for entry in read_sorted_dir(src)? {
        let name = entry.file_name();
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        let file_type = entry.file_type()?;

        if file_type.is_symlink() {
            copy_symlink(&src_path, &dst_path, stats, warnings)?;
        } else if file_type.is_dir() {
            std::fs::create_dir_all(&dst_path)?;
            stats.dirs += 1;
            copy_dir_contents(&src_path, &dst_path, stats, warnings)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
            stats.files += 1;
        }
    }
    Ok(())
}

/// Symlink policy for the directory-copy mode: recreate the link itself (the
/// same target text, unresolved) rather than following it — a symlink pointing
/// outside the workspace, or at an ancestor of itself, is copied as a link and
/// never walked into. Cheap and exact on Unix ([`std::os::unix::fs::symlink`]);
/// elsewhere (no portable, privilege-free symlink call in `std`) the entry is
/// skipped with a warning rather than silently followed or copied as a
/// duplicate file.
#[cfg(unix)]
fn copy_symlink(
    src_path: &Path,
    dst_path: &Path,
    stats: &mut Stats,
    _warnings: &mut [String],
) -> io::Result<()> {
    let target = std::fs::read_link(src_path)?;
    std::os::unix::fs::symlink(&target, dst_path)?;
    stats.symlinks += 1;
    Ok(())
}

#[cfg(not(unix))]
fn copy_symlink(
    src_path: &Path,
    _dst_path: &Path,
    stats: &mut Stats,
    warnings: &mut Vec<String>,
) -> io::Result<()> {
    warnings.push(format!(
        "skipped symlink (not supported on this platform): {}",
        src_path.display()
    ));
    stats.skipped_symlinks += 1;
    Ok(())
}

/// Recursively add `src`'s contents to the archive under `prefix`. Symlinks are
/// always skipped (with a warning) here rather than recreated: representing one
/// in a ZIP entry requires per-platform Unix extra-field attributes, which
/// would bloat this hand-rolled, dependency-free writer well past "legitimately
/// simple" for a store-only format. Copy mode (above) preserves them instead;
/// an archive is the one place this backup is genuinely lossy.
fn add_dir_to_zip<W: io::Write>(
    zw: &mut ZipWriter<W>,
    src: &Path,
    prefix: &str,
    stats: &mut Stats,
    warnings: &mut Vec<String>,
) -> io::Result<()> {
    for entry in read_sorted_dir(src)? {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let src_path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_symlink() {
            warnings.push(format!(
                "skipped symlink (not supported in --zip archives): {}",
                src_path.display()
            ));
            stats.skipped_symlinks += 1;
            continue;
        }

        let at = dos_datetime_of(&src_path);
        if file_type.is_dir() {
            let entry_prefix = format!("{prefix}{name_str}/");
            zw.add_dir(&entry_prefix, at)?;
            stats.dirs += 1;
            add_dir_to_zip(zw, &src_path, &entry_prefix, stats, warnings)?;
        } else {
            let data = std::fs::read(&src_path)?;
            zw.add_file(&format!("{prefix}{name_str}"), &data, at)?;
            stats.files += 1;
        }
    }
    Ok(())
}

/// The MS-DOS date/time ZIP wants for `path`, from that file's own recorded
/// modification time — never the current time (this module reads no clock; the
/// one clock in the CLI, [`crate::now_rfc3339`], is unrelated). Unreadable
/// metadata, a missing modified-time (some exotic backend), or an instant
/// before the format's 1980 floor all fall back to the same fixed epoch
/// (1980-01-01T00:00:00Z, DOS's own zero value) — deterministic either way, so
/// backing up the same tree twice produces byte-identical archives whenever
/// mtimes are stable.
fn dos_datetime_of(path: &Path) -> (u16, u16) {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| io::Error::other(e.to_string()))
        })
        .map(|d| dos_datetime(d.as_secs()))
        .unwrap_or(DOS_EPOCH)
}

/// DOS's own zero timestamp: 1980-01-01, 00:00:00 — the floor of the format's
/// date range, and the deterministic fallback above.
const DOS_EPOCH: (u16, u16) = (0, 0x0021);

/// Seconds-since-Unix-epoch as ZIP's native (time, date) pair, each a packed
/// bitfield (PKWARE APPNOTE.TXT §4.4.6). Clamped at the 1980-01-01 floor DOS
/// timestamps cannot represent below.
fn dos_datetime(secs: u64) -> (u16, u16) {
    const DOS_FLOOR_SECS: u64 = 315_532_800; // 1980-01-01T00:00:00Z
    let secs = secs.max(DOS_FLOOR_SECS);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = crate::civil_from_days(days);
    let dos_year = (year - 1980).clamp(0, i64::from(u16::MAX >> 9)) as u16;
    let dos_date = (dos_year << 9) | ((month as u16) << 5) | (day as u16);
    let dos_time = ((hour as u16) << 11) | ((min as u16) << 5) | ((sec as u16) / 2);
    (dos_time, dos_date)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-backup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dos_datetime_floors_at_1980() {
        assert_eq!(dos_datetime(0), (0, 0x0021));
    }

    #[test]
    fn dos_datetime_matches_a_known_instant() {
        // 2020-02-29T13:07:36Z (a leap day, to exercise the calendar path).
        let (time, date) = dos_datetime(1_582_981_656);
        let year = 1980 + (date >> 9);
        let month = (date >> 5) & 0x0F;
        let day = date & 0x1F;
        assert_eq!((year, month, day), (2020, 2, 29));
        let hour = time >> 11;
        let min = (time >> 5) & 0x3F;
        let sec2 = time & 0x1F; // seconds / 2
        assert_eq!((hour, min, sec2 * 2), (13, 7, 36));
    }

    #[test]
    fn effective_canonical_resolves_a_nonexistent_suffix_against_a_real_prefix() {
        let dir = tempdir("canon-suffix");
        let got = effective_canonical(&dir.join("does-not-exist-yet")).unwrap();
        let want = std::fs::canonicalize(&dir)
            .unwrap()
            .join("does-not-exist-yet");
        assert_eq!(got, want);
    }

    #[test]
    fn check_destination_accepts_an_empty_existing_directory() {
        let dir = tempdir("empty-ok");
        let target = dir.join("dest");
        std::fs::create_dir_all(&target).unwrap();
        assert!(check_destination(&target, false).is_ok());
    }

    #[test]
    fn check_destination_refuses_a_nonempty_existing_directory() {
        let dir = tempdir("nonempty-refused");
        let target = dir.join("dest");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("stray.txt"), "x").unwrap();
        assert!(check_destination(&target, false).is_err());
    }

    #[test]
    fn check_destination_refuses_an_existing_file() {
        let dir = tempdir("file-refused");
        let target = dir.join("dest");
        std::fs::write(&target, "x").unwrap();
        assert!(check_destination(&target, false).is_err());
        assert!(check_destination(&target, true).is_err());
    }

    #[test]
    fn check_destination_refuses_a_directory_in_zip_mode() {
        let dir = tempdir("zip-dir-refused");
        let target = dir.join("dest");
        std::fs::create_dir_all(&target).unwrap();
        assert!(check_destination(&target, true).is_err());
    }

    #[test]
    fn copy_dir_contents_round_trips_nested_dirs_and_hidden_files() {
        let dir = tempdir("copy-roundtrip");
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("a/b")).unwrap();
        std::fs::write(src.join("top.md"), "top").unwrap();
        std::fs::write(src.join(".hidden"), "shh").unwrap();
        std::fs::write(src.join("a/b/leaf.md"), "leaf").unwrap();

        let dst = dir.join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        let mut stats = Stats::default();
        let mut warnings = Vec::new();
        copy_dir_contents(&src, &dst, &mut stats, &mut warnings).unwrap();

        assert_eq!(std::fs::read_to_string(dst.join("top.md")).unwrap(), "top");
        assert_eq!(std::fs::read_to_string(dst.join(".hidden")).unwrap(), "shh");
        assert_eq!(
            std::fs::read_to_string(dst.join("a/b/leaf.md")).unwrap(),
            "leaf"
        );
        assert_eq!(stats.files, 3);
        assert_eq!(stats.dirs, 2);
        assert!(warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn copy_dir_contents_recreates_symlinks_without_following_them() {
        let dir = tempdir("copy-symlink");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("real.md"), "real").unwrap();
        std::os::unix::fs::symlink("real.md", src.join("link.md")).unwrap();
        // A symlink to somewhere outside the tree entirely — must not be
        // followed or dereferenced, just recreated as the same link text.
        std::os::unix::fs::symlink("/nonexistent-outside-target", src.join("dangling.md")).unwrap();

        let dst = dir.join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        let mut stats = Stats::default();
        let mut warnings = Vec::new();
        copy_dir_contents(&src, &dst, &mut stats, &mut warnings).unwrap();

        assert_eq!(stats.symlinks, 2);
        assert_eq!(
            std::fs::read_link(dst.join("link.md")).unwrap(),
            PathBuf::from("real.md")
        );
        assert_eq!(
            std::fs::read_link(dst.join("dangling.md")).unwrap(),
            PathBuf::from("/nonexistent-outside-target")
        );
    }

    #[test]
    fn add_dir_to_zip_produces_the_expected_entries() {
        let dir = tempdir("zip-entries");
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.md"), "hello").unwrap();
        std::fs::write(src.join("sub/b.md"), "world").unwrap();

        let mut zw = ZipWriter::new(Vec::new());
        zw.add_dir("root/", DOS_EPOCH).unwrap();
        let mut stats = Stats::default();
        let mut warnings = Vec::new();
        add_dir_to_zip(&mut zw, &src, "root/", &mut stats, &mut warnings).unwrap();
        let bytes = zw.finish().unwrap();

        // Structural check: every expected path appears as a UTF-8 substring of
        // the archive (each ZIP entry's name is stored as literal bytes right
        // after its 30-byte local header, so a raw substring search is exactly as
        // trustworthy here as parsing the central directory — see zip::tests for
        // the full-parse version).
        let text = String::from_utf8_lossy(&bytes);
        for expect in ["root/", "root/a.md", "root/sub/", "root/sub/b.md"] {
            assert!(text.contains(expect), "missing entry {expect}");
        }
        assert_eq!(stats.files, 2);
        assert_eq!(stats.dirs, 1);
        assert!(warnings.is_empty());
    }
}
