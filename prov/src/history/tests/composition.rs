//! What the history store is composed *with*, which only exists here.
//!
//! Each of these turns on a fact `prov-history` is defined not to know: where
//! the recycle bin parks its items, which directories a workspace treats as
//! byte-parking interiors rather than documents, and who owns the fixity cache
//! a capture reads through. The store reaches all three through its host
//! traits, so what is under test is the answer this crate supplies — not the
//! use history makes of it, which is tested in `prov-history`.

use std::path::{Path, PathBuf};

use super::support::*;
use prov_graph::exec::block_on;

/// `history_exclusions` in the direction it was written for: a capture must not
/// park bytes the user has consigned to the bin. (It emphatically does *not*
/// make a purge final for content captured while it was live — that is
/// documented, not tested here, because it is a non-guarantee.)
#[test]
fn binned_bytes_are_not_newly_retained_by_a_routine_capture() {
    let dir = seed("capture-bin");
    write(
        &dir,
        "recyclebin/index.yaml",
        "title: Recycle Bin\ndeleted: []\n",
    );
    write(&dir, "recyclebin/items/notes/old.md", "binned bytes\n");
    write(
        &dir,
        "index.md",
        "---\ntitle: Home\ncontents:\n- notes/a.md\n- notes/photo.jpg.yaml\n\
         recycle_bin: recyclebin/index.yaml\n---\nroot\n",
    );
    let set = block_on(ws(&dir).history_capture_set(Path::new("index.md"))).unwrap();
    assert!(
        set.iter().all(|p| !p.starts_with("recyclebin/items")),
        "binned bytes must not be captured: {set:?}"
    );
    // The bin *index* is captured, though — that is what makes a restore put
    // a live document back as live.
    assert!(
        set.contains(&PathBuf::from("recyclebin/index.yaml")),
        "the bin index is ordinary structural state: {set:?}"
    );
}

/// A shard index is titled `"{Month} {Year}"`, which in a journal is an
/// entirely ordinary thing for a person to have called a note. Before the
/// stores were excluded, `[[January 2026]]` resolved `Unique` into
/// `history/events/2026/01/index.md` — a document the reader cannot see in
/// the tree and never meant to link to.
#[test]
fn a_shard_index_never_answers_to_a_name_the_author_might_use() {
    let dir = seed("titles-history");
    capture(&dir, "2026-01-15T09:15:22.000000Z", Some("first"));
    let w = ws(&dir);

    let titles = block_on(w.title_index_scoped(Path::new("index.md"))).unwrap();
    assert!(
        matches!(titles.resolve("January 2026"), crate::TitleMatch::Unknown),
        "a history shard answered to a month-and-year name"
    );
    // The store's own index is the deliberate exception, and the boundary is
    // worth pinning: the root points at it, `check` validates it, and a
    // reader can open it and learn what the store holds — so it is a
    // document of the workspace and keeps a name like any other. What is
    // excluded is its *interior*.
    assert!(
        matches!(titles.resolve("History"), crate::TitleMatch::Unique(_)),
        "the store index is part of the workspace and should still resolve"
    );

    // The author's *own* documents still resolve — the exclusion is about
    // prov's bookkeeping, not about narrowing the workspace.
    assert!(
        matches!(titles.resolve("A"), crate::TitleMatch::Unique(_)),
        "an ordinary note stopped resolving"
    );
}

/// The same exclusion, in the direction the bin makes vivid: a recycled
/// document keeps the title it had, so indexing `items/` means `[[A]]` can
/// resolve to the copy of a note the author deleted — while the live note is
/// still sitting there under the same name.
#[test]
fn a_recycled_document_stops_answering_to_the_name_it_had() {
    let dir = seed("titles-bin");
    let mut w = ws(&dir);
    block_on(w.recycle(Path::new("notes/a.md"), false, Some("2026-01-15T09:15:22Z"))).unwrap();

    let titles = block_on(ws(&dir).title_index_scoped(Path::new("index.md"))).unwrap();
    assert!(
        matches!(titles.resolve("A"), crate::TitleMatch::Unknown),
        "a binned document still answered to its title"
    );
}

/// The workspace owns the cache a capture reads through, so it is the workspace
/// that has to refuse a foreign one. `prov-history` tests what capture *does*
/// with a warm cache; this is the half above it — that the digests offered are
/// this workspace's own. The failure would be silent and wrong rather than loud
/// and wrong, so it is worth its own test even though `decode` is unit-tested.
#[test]
fn a_cache_from_another_workspace_is_refused() {
    let one = seed("cache-foreign-one");
    let two = seed("cache-foreign-two");
    let mut first = ws(&one);
    first.set_fixity_cache(Some(crate::FixityCache::new(&one)));
    block_on(first.history_capture(
        Path::new("index.md"),
        "2026-01-01T00:00:00Z",
        crate::CaptureNote::default(),
    ))
    .unwrap();
    let bytes = first.take_fixity_cache().unwrap().encode();

    assert!(
        crate::FixityCache::decode(&bytes, &two).is_none(),
        "one workspace's digests were offered to another"
    );
    assert!(crate::FixityCache::decode(&bytes, &one).is_some());
}
