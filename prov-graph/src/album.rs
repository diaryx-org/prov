//! Albums — a selection from a manifest, by hash.
//!
//! A [manifest](crate::manifest) claims a directory completely: every opaque
//! file under its `root` has a row, and no two manifests claim one file. That
//! is what lets it answer "did anything vanish", and it is also why a manifest
//! cannot be a *selection*. Forty photographs picked out of an archive of eighty
//! thousand, listed in a manifest of their own, would leave the other 79,960 as
//! drift, and would claim forty files the archive's manifest already claims.
//!
//! An **album** is the selection instead. It claims no file. It names the
//! manifest node it picks from, and the rows it picks by their hash:
//!
//! ```yaml
//! # reunion-2019.yaml
//! title: Family reunion, 2019
//! part_of: '[Photographs](/index.md)'
//! picks_from: '[Harris family photographs](id:abc1234)'
//! picks:
//! - sha256:bb2cb0280300ae0e9a87c49ea385083afd40c33c8aa1abdc88faa321f4a27e76
//! - hash: sha256:e6512e4c3757513df434c8d1e3565289c71dbe98a98fefae5c05795a0c5eda6b
//!   caption: Grandpa at the lake
//! ```
//!
//! - **`picks_from` is a link**, to the manifest *node*, and prov reads it as
//!   one wherever it is written: censused, reported broken, rewritten when the
//!   node moves, relabelled when it is retitled. It is a path-valued field the
//!   workspace need not declare ([spec §3](/docs/spec.md), *A field that holds
//!   links*).
//! - **A pick is a hash**, `sha256:<hex>` as the source's rows write it. A
//!   hash names the bytes, not where they are kept, so reorganising the
//!   archive's folders leaves every album as it was, and a device that holds
//!   the album and not the archive can still ask the archive's keeper for each
//!   photograph by the address it already uses. A pick is written bare, or as a
//!   mapping with `hash` and fields of its own (a caption *this album* gives
//!   the photograph), which are data, not links, as a manifest row's are.
//! - **The source must carry hashes.** An inventory manifest, with no digest
//!   on its rows, has nothing a pick could name.
//!
//! An album is an ordinary document otherwise: a YAML node, or a Markdown page
//! whose body says what the album is. It may not also declare `manifest`: a
//! node stands for a directory or picks from one, never both. Who may read it
//! is its own `audience`, not its source's, which is the point: the archive can
//! stay its keeper's alone while an album of it is given to a circle.
//!
//! `check` reports a pick its source no longer lists (`AlbumPickMissing`) and
//! an album that cannot be read as one (`AlbumMalformed`).

use crate::error::{Error, Result};
use crate::meta::{Mapping, Value};

/// The node key linking an album to the manifest node it picks from.
pub const PICKS_FROM_KEY: &str = "picks_from";

/// The node key holding an album's picks.
pub const PICKS_KEY: &str = "picks";

/// The per-pick key holding the digest, when a pick is written as a mapping.
pub const PICK_HASH_KEY: &str = "hash";

/// One photograph an album picks: the digest of its bytes, and whatever the
/// album says about it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Pick {
    /// `sha256:<64 lowercase hex>`, as the source manifest's row writes it.
    pub hash: String,
    /// Every other key on the pick, in the order written. Never `hash`.
    pub fields: Mapping,
}

impl Pick {
    /// A pick of `hash` with nothing more to say about it.
    pub fn new(hash: impl Into<String>) -> Self {
        Pick {
            hash: hash.into(),
            fields: Mapping::new(),
        }
    }

    /// The bare lowercase hex of the digest.
    pub fn hex(&self) -> &str {
        self.hash.strip_prefix("sha256:").unwrap_or(&self.hash)
    }

    fn to_value(&self) -> Value {
        if self.fields.is_empty() {
            return Value::String(self.hash.clone());
        }
        let mut map = Mapping::new();
        map.insert(PICK_HASH_KEY.into(), Value::String(self.hash.clone()));
        map.extend(self.fields.clone());
        Value::Mapping(map)
    }
}

/// An album as its node writes it: the link to its source, as written, and
/// its picks, in the order written.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Album {
    /// The `picks_from` value exactly as written: a link, in whichever style
    /// the workspace writes links. Resolved against the graph, not here.
    pub picks_from: String,
    /// The picks, in the order the album lists them. An album's order is its
    /// author's, so prov never sorts it.
    pub picks: Vec<Pick>,
}

/// Whether `hash` is spelled as a manifest row's digest is:
/// `sha256:` and 64 lowercase hex digits.
pub fn is_sha256(hash: &str) -> bool {
    hash.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

impl Album {
    /// Read an album out of a node's metadata, or `None` when the node is not
    /// one (it carries neither key).
    ///
    /// Strict about what a reader must trust: a `picks_from` that is a string,
    /// and picks that are each a well-formed digest. A pick that is not is
    /// refused rather than skipped, for a manifest row's reason: a dropped pick
    /// reads as "never picked", and would turn a damaged album into a clean
    /// report. A digest written twice is refused too, since the second says
    /// nothing the first did not and an edit to one would leave the other.
    pub fn from_meta(meta: &Value) -> Result<Option<Self>> {
        let from = meta.get(PICKS_FROM_KEY);
        let picks = meta.get(PICKS_KEY);
        if from.is_none() && picks.is_none() {
            return Ok(None);
        }
        let picks_from = from
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|from| !from.is_empty())
            .ok_or_else(|| {
                Error::Structure(format!(
                    "an album's `{PICKS_FROM_KEY}` must link to the manifest node it picks from"
                ))
            })?
            .to_string();
        let rows = match picks {
            None | Some(Value::Null) => &[][..],
            Some(value) => value.as_sequence().ok_or_else(|| {
                Error::Structure(format!("an album's `{PICKS_KEY}` must be a list"))
            })?,
        };
        let mut seen = std::collections::HashSet::with_capacity(rows.len());
        let mut out = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            let pick = match row {
                Value::String(hash) => Pick::new(hash.trim()),
                Value::Mapping(map) => {
                    let hash = map
                        .get(PICK_HASH_KEY)
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            Error::Structure(format!("pick {i} has no `{PICK_HASH_KEY}`"))
                        })?
                        .trim()
                        .to_string();
                    let fields = map
                        .iter()
                        .filter(|(key, _)| key.as_str() != PICK_HASH_KEY)
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect();
                    Pick { hash, fields }
                }
                _ => {
                    return Err(Error::Structure(format!(
                        "pick {i} must be a digest or a mapping with `{PICK_HASH_KEY}`"
                    )));
                }
            };
            if !is_sha256(&pick.hash) {
                return Err(Error::Structure(format!(
                    "pick {i} (`{}`) is not `sha256:` and 64 lowercase hex digits",
                    pick.hash
                )));
            }
            if !seen.insert(pick.hash.clone()) {
                return Err(Error::Structure(format!(
                    "pick {i} (`{}`) is picked twice",
                    pick.hash
                )));
            }
            out.push(pick);
        }
        Ok(Some(Album {
            picks_from,
            picks: out,
        }))
    }

    /// The picks as a node writes them: a bare digest where the pick says
    /// nothing more, a mapping where it does.
    pub fn picks_value(&self) -> Value {
        Value::Sequence(self.picks.iter().map(Pick::to_value).collect())
    }

    /// Whether the album picks `hash` (`sha256:<hex>`).
    pub fn picks(&self, hash: &str) -> bool {
        self.picks.iter().any(|pick| pick.hash == hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "sha256:bb2cb0280300ae0e9a87c49ea385083afd40c33c8aa1abdc88faa321f4a27e76";
    const B: &str = "sha256:e6512e4c3757513df434c8d1e3565289c71dbe98a98fefae5c05795a0c5eda6b";

    fn meta(yaml: &str) -> Value {
        crate::meta::parse_value(yaml, crate::MetaFormat::Yaml).unwrap()
    }

    #[test]
    fn a_node_with_neither_key_is_not_an_album() {
        assert_eq!(Album::from_meta(&meta("title: x\n")).unwrap(), None);
    }

    #[test]
    fn picks_are_read_bare_or_with_fields_and_kept_in_order() {
        let album = Album::from_meta(&meta(&format!(
            "picks_from: '[Photo](photo.yaml)'\npicks:\n- {B}\n- hash: {A}\n  caption: Grandpa\n"
        )))
        .unwrap()
        .unwrap();
        assert_eq!(album.picks_from, "[Photo](photo.yaml)");
        assert_eq!(album.picks[0], Pick::new(B));
        assert_eq!(album.picks[1].hash, A);
        assert_eq!(
            album.picks[1].fields.get("caption").and_then(Value::as_str),
            Some("Grandpa")
        );
        assert!(album.picks(A) && album.picks(B));
        assert_eq!(album.picks[1].hex(), &A[7..]);
    }

    #[test]
    fn an_album_with_no_picks_yet_is_an_album() {
        let album = Album::from_meta(&meta("picks_from: photo.yaml\n"))
            .unwrap()
            .unwrap();
        assert!(album.picks.is_empty());
    }

    #[test]
    fn what_a_reader_cannot_trust_is_refused() {
        for bad in [
            format!("picks:\n- {A}\n"),
            format!("picks_from: ''\npicks:\n- {A}\n"),
            "picks_from: p.yaml\npicks: {}\n".to_string(),
            "picks_from: p.yaml\npicks:\n- sha256:00\n".to_string(),
            format!("picks_from: p.yaml\npicks:\n- {}\n", A.to_uppercase()),
            format!("picks_from: p.yaml\npicks:\n- caption: {A}\n"),
            format!("picks_from: p.yaml\npicks:\n- {A}\n- hash: {A}\n"),
            "picks_from: p.yaml\npicks:\n- 3\n".to_string(),
        ] {
            assert!(Album::from_meta(&meta(&bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn picks_write_back_as_they_were_read() {
        let yaml = format!("picks_from: p.yaml\npicks:\n- {B}\n- hash: {A}\n  caption: Grandpa\n");
        let album = Album::from_meta(&meta(&yaml)).unwrap().unwrap();
        let mut map = Mapping::new();
        map.insert(
            PICKS_FROM_KEY.into(),
            Value::String(album.picks_from.clone()),
        );
        map.insert(PICKS_KEY.into(), album.picks_value());
        assert_eq!(
            Album::from_meta(&Value::Mapping(map)).unwrap().unwrap(),
            album
        );
    }
}
