//! The C ABI `main.bend`'s effects call.
//!
//! Every function takes one byte string as a `(pointer, length)` pair —
//! newline-separated arguments — and returns `0` and a buffer in `out` on
//! success, or an errno-style code and a message in `out` on failure. Rust
//! owns every buffer it hands out: the caller returns it through
//! [`pb_free`] with the length it was given, and nothing else. No panic
//! crosses the boundary — `catch_unwind` turns one into `EIO`.
//!
//! Two questions, and no decisions. [`pb_dir`] says what one directory
//! holds. [`pb_load`] says what one document says: its metadata as a flat,
//! pre-order value tree, and the links prov's body scanner finds in its
//! prose. Which directories are listed, which documents are loaded, which
//! fields are links, how a target resolves and what any of it means are all
//! decided on the Bend side. The parse is the boundary because prov's
//! parsers are fig and twig, written in Zig, and a pure Bend definition
//! cannot call a host: everything a law is about starts after it.

use std::ffi::c_char;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::slice;

use prov_graph::document::Document;
use prov_graph::link::scan_body_links;

const EIO: i32 = 5;
const EINVAL: i32 = 22;

/// The first line of every document [`pb_load`] answers with, so a Bend
/// side built against another shape refuses it rather than misreading it.
pub const DOC_HEADER: &str = "prov-bend-doc 1";

/// What `root/rel` holds, one entry a line: its kind (`f` file, `d`
/// directory, `l` symbolic link — never followed —, `o` anything else), a
/// tab, and its name. In the order the directory gives them. A name that is
/// not UTF-8 is left out, as prov's own scans leave it out.
///
/// The argument is `root`, a newline, and `rel` (empty for the root itself).
///
/// # Safety
///
/// `arg` is `arg_len` readable bytes; `out` and `out_len` are writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_dir(
    arg: *const c_char,
    arg_len: usize,
    out: *mut *mut c_char,
    out_len: *mut usize,
) -> i32 {
    unsafe {
        answer(arg, arg_len, out, out_len, |arg| {
            two(arg).and_then(|(root, rel)| dir(root, rel))
        })
    }
}

/// What the document at `root/rel` says, in the shape `canon.bend` reads —
/// see [`render`] — or why it could not be read, in the words prov's own
/// `Unreadable` finding uses (`io error: …`, `metadata error: …`).
///
/// # Safety
///
/// As [`pb_dir`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_load(
    arg: *const c_char,
    arg_len: usize,
    out: *mut *mut c_char,
    out_len: *mut usize,
) -> i32 {
    unsafe {
        answer(arg, arg_len, out, out_len, |arg| {
            two(arg).and_then(|(root, rel)| load(root, rel))
        })
    }
}

/// [`pb_dir`] for many directories in one call: the root, then a directory
/// a line. Back, two pieces for each, separated by [`RS`]: a head — the
/// directory, `1` or `0` for whether it was listed, and the error when it was
/// not, tab-separated — and a body, its entries as [`pb_dir`] gives them. The
/// C adapter hands the pieces to Bend as a list, so each can be read on its
/// own lane. One crossing of the boundary for a round's worth of
/// questions: each crossing is a round trip through Bend's event loop to a
/// helper thread, and a workspace of thousands asked one at a time spent
/// most of its time in them.
///
/// # Safety
///
/// As [`pb_dir`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_dirs(
    arg: *const c_char,
    arg_len: usize,
    out: *mut *mut c_char,
    out_len: *mut usize,
) -> i32 {
    unsafe { answer(arg, arg_len, out, out_len, |arg| Ok(batch(arg, dir))) }
}

/// [`pb_load`] for many documents in one call, answered as [`pb_dirs`] is: a
/// head for each — the path, `1` and nothing, or `0` and the words of its
/// `Unreadable` finding — and a body, its records when it loaded.
///
/// # Safety
///
/// As [`pb_dir`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_loads(
    arg: *const c_char,
    arg_len: usize,
    out: *mut *mut c_char,
    out_len: *mut usize,
) -> i32 {
    unsafe { answer(arg, arg_len, out, out_len, |arg| Ok(batch(arg, load))) }
}

/// The separator between a batch's pieces, which [`escape`] keeps out of
/// every field.
pub const RS: char = '\u{1e}';

fn batch(arg: &str, one: fn(&str, &str) -> Result<String, (i32, String)>) -> String {
    let mut lines = arg.split('\n');
    let root = lines.next().unwrap_or("");
    let mut pieces: Vec<String> = Vec::new();
    for rel in lines {
        let (head, body) = match one(root, rel) {
            Ok(text) => (format!("{}\t1\t", escape(rel)), text),
            Err((_, why)) => (
                format!("{}\t0\t{}", escape(rel), escape(&why)),
                String::new(),
            ),
        };
        pieces.push(head);
        pieces.push(body);
    }
    pieces.join(&RS.to_string())
}

/// Return a buffer this library handed out.
///
/// # Safety
///
/// `p` came from this library with this `len`, and is freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_free(p: *mut c_char, len: usize) {
    if p.is_null() {
        return;
    }
    // Allocated as `len + 1` bytes by `hand_out`, the terminator included.
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(p as *mut u8, len + 1)) });
}

unsafe fn answer(
    arg: *const c_char,
    arg_len: usize,
    out: *mut *mut c_char,
    out_len: *mut usize,
    f: impl FnOnce(&str) -> Result<String, (i32, String)>,
) -> i32 {
    let bytes: &[u8] = if arg.is_null() {
        &[]
    } else {
        unsafe { slice::from_raw_parts(arg as *const u8, arg_len) }
    };
    let result = match std::str::from_utf8(bytes) {
        Ok(text) => catch_unwind(AssertUnwindSafe(|| f(text)))
            .unwrap_or_else(|_| Err((EIO, "prov-bend-ffi: a host service panicked".to_owned()))),
        Err(_) => Err((
            EINVAL,
            "prov-bend-ffi: an argument that is not UTF-8".to_owned(),
        )),
    };
    let (code, text) = match result {
        Ok(text) => (0, text),
        Err((code, text)) => (code, text),
    };
    unsafe { hand_out(text, out, out_len) };
    code
}

unsafe fn hand_out(text: String, out: *mut *mut c_char, out_len: *mut usize) {
    let len = text.len();
    let mut bytes = text.into_bytes();
    bytes.push(0);
    let boxed = bytes.into_boxed_slice();
    unsafe {
        *out = Box::into_raw(boxed) as *mut c_char;
        *out_len = len;
    }
}

fn two(arg: &str) -> Result<(&str, &str), (i32, String)> {
    arg.split_once('\n').ok_or((
        EINVAL,
        "prov-bend-ffi: expected a root and a path".to_owned(),
    ))
}

fn joined(root: &str, rel: &str) -> PathBuf {
    if rel.is_empty() {
        PathBuf::from(root)
    } else {
        Path::new(root).join(rel)
    }
}

fn dir(root: &str, rel: &str) -> Result<String, (i32, String)> {
    let entries = std::fs::read_dir(joined(root, rel)).map_err(|e| (code(&e), e.to_string()))?;
    let mut out = String::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let kind = match entry.file_type() {
            Ok(t) if t.is_symlink() => 'l',
            Ok(t) if t.is_dir() => 'd',
            Ok(t) if t.is_file() => 'f',
            _ => 'o',
        };
        let _ = writeln!(out, "{kind}\t{}", escape(&name));
    }
    Ok(out)
}

fn load(root: &str, rel: &str) -> Result<String, (i32, String)> {
    // `Graph::load`'s own two steps and its own error words: the read, then
    // the parse. The escape check before them is the Bend side's.
    let text = std::fs::read_to_string(joined(root, rel))
        .map_err(|e| (code(&e), prov_graph::Error::from(e).to_string()))?;
    let path = Path::new(rel);
    let doc = Document::parse(path, &text).map_err(|e| (EINVAL, e.to_string()))?;
    Ok(render(path, &doc))
}

/// The shape `canon.bend` reads. One record a line, fields separated by
/// tabs, each field escaped (`\\`, `\t`, `\n`):
///
/// - `prov-bend-doc 1` — [`DOC_HEADER`];
/// - `H` and `1` when the document has metadata, `0` when it has none;
/// - `V`, depth, key, tag, text — the metadata tree in pre-order. The key is
///   a mapping key, or an item's position in its sequence; the tag is `s`
///   string, `m` mapping, `q` sequence, `n` null, `b` boolean, `i` integer,
///   `f` float, `x` anything else (a TOML datetime, say), and the text is
///   the scalar spelled as fig spells it;
/// - `L`, start, end, image (`1`/`0`), label present (`1`/`0`), label,
///   target — each link the body scanner finds, byte span and all.
pub fn render(path: &Path, doc: &Document) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{DOC_HEADER}");
    let _ = writeln!(out, "H\t{}", if doc.has_meta() { 1 } else { 0 });
    let meta = fig::Value::from(&doc.meta);
    if let fig::Value::Map(entries) = &meta {
        for (key, value) in entries {
            tree(&mut out, 0, &key_text(key), value);
        }
    }
    for link in scan_body_links(path, &doc.body) {
        let _ = writeln!(
            out,
            "L\t{}\t{}\t{}\t{}\t{}\t{}",
            link.span.start,
            link.span.end,
            if link.image { 1 } else { 0 },
            if link.link.label.is_some() { 1 } else { 0 },
            escape(link.link.label.as_deref().unwrap_or("")),
            escape(&link.link.target),
        );
    }
    out
}

fn tree(out: &mut String, depth: usize, key: &str, value: &fig::Value) {
    let (tag, text) = match value {
        fig::Value::Str(s) => ('s', s.clone()),
        fig::Value::Map(_) => ('m', String::new()),
        fig::Value::Seq(_) => ('q', String::new()),
        fig::Value::Null => ('n', String::new()),
        fig::Value::Bool(b) => ('b', b.to_string()),
        fig::Value::Int(i) => ('i', i.to_string()),
        fig::Value::Uint(u) => ('i', u.to_string()),
        fig::Value::Float(f) => ('f', f.to_string()),
        other => ('x', format!("{other:?}")),
    };
    let _ = writeln!(out, "V\t{depth}\t{}\t{tag}\t{}", escape(key), escape(&text));
    match value {
        fig::Value::Map(entries) => {
            for (k, v) in entries {
                tree(out, depth + 1, &key_text(k), v);
            }
        }
        fig::Value::Seq(items) => {
            for (i, v) in items.iter().enumerate() {
                tree(out, depth + 1, &i.to_string(), v);
            }
        }
        _ => {}
    }
}

fn key_text(key: &fig::Value) -> String {
    match key {
        fig::Value::Str(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            RS => out.push_str("\\e"),
            c => out.push(c),
        }
    }
    out
}

fn code(e: &std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(EIO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_relations_and_body_links() {
        let text = "---\ntitle: A\ncontents:\n- '[B](b.md)'\n- 3\npart_of: id:x/y\n---\n\nSee [c](c.md).\n";
        let doc = Document::parse(Path::new("a.md"), text).unwrap();
        let out = render(Path::new("a.md"), &doc);
        assert_eq!(
            out,
            "prov-bend-doc 1\nH\t1\nV\t0\ttitle\ts\tA\nV\t0\tcontents\tq\t\nV\t1\t0\ts\t[B](b.md)\n\
V\t1\t1\ti\t3\nV\t0\tpart_of\ts\tid:x/y\nL\t5\t14\t0\t1\tc\tc.md\n"
        );
    }

    #[test]
    fn batches_answer_each_question_in_turn() {
        let dir = std::env::temp_dir().join("prov-bend-ffi-batch");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.md"), "---\ntitle: A\n---\n").unwrap();
        let root = dir.to_str().unwrap();
        let out = batch(&format!("{root}\na.md\nmissing.md"), load);
        let pieces: Vec<&str> = out.split(RS).collect();
        assert_eq!(pieces.len(), 4);
        assert_eq!(pieces[0], "a.md\t1\t");
        assert_eq!(pieces[1], "prov-bend-doc 1\nH\t1\nV\t0\ttitle\ts\tA\n");
        assert!(pieces[2].starts_with("missing.md\t0\tio error: "));
        assert_eq!(pieces[3], "");
    }

    #[test]
    fn escapes_what_would_break_a_record() {
        assert_eq!(escape("a\tb\nc\\\u{1e}"), "a\\tb\\nc\\\\\\e");
    }
}
