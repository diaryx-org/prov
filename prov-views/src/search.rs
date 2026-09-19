//! Finding every document that mentions a word.
//!
//! The question an archive is asked most is the plainest one — *every mention
//! of Ogden*, *the letter about the pen* — and a workspace whose prose is in
//! files answers it in principle: the words are all there. This is the view
//! that answers it in practice: plain words, matched as substrings, over the
//! **title**, the **metadata values** and the **body** of every document the
//! workspace reaches. A sidecar with no prose answers by its fields alone,
//! since a field is where whoever wrote it said what they know about the
//! file.
//!
//! # A view, and what it is over
//!
//! The census ([`documents`](crate::documents)) is every reached document
//! with its metadata, and [`Graph::body`] is each one's prose read from
//! wherever it lives — a combined document's is what is left once the block
//! is taken out, a separated node's is the file its `content` names. That is
//! the corpus, and this module is a view over it in the same sense a grouped
//! view is: a second way through the same documents, reading nothing the
//! census does not already carry. The body stays what DESIGN §2 tier 3 says
//! it is, carried and never reasoned about, because the index built here is
//! **derived and disposable**: a [`Corpus`] is built from the files, held in
//! memory by whoever asked, and thrown away. Nothing is written into the
//! workspace, and no file depends on it, so a workspace opened on a second
//! device searches after a rebuild, not after a sync.
//!
//! # What "word" means
//!
//! A query is whitespace-separated terms, and a document matches when every
//! term occurs somewhere in it — the terms need not share a site, so `ogden
//! pen` finds a letter titled *From Ogden* whose body mentions the pen.
//! Matching is case-insensitive and diacritic-insensitive ([`fold`]): an
//! archive spells its names several ways, and `Ålesund` typed as `alesund`
//! has to find the page. Plain substring match is the honest first cut;
//! anything fuzzier is a later edit with a use that asked for it.
//!
//! # Which keys are read
//!
//! A metadata value is searched unless the key carries structure rather than
//! what the author wrote. Every relation key the workspace declares is
//! structure — under the default relations that is the spanning pair
//! (`contents` names every child by its title, and `part_of` the parent, so
//! every child of *Ogden letters* would match `ogden` through it), `links`
//! and `link_of`, the succession and derivation pairs, and the root's
//! pointers to its registry, config and stores — and so are `id`, `content`,
//! `manifest` and `root`, which are identifiers and file names. A hit through
//! any of them would be a hit on some other document's name, or on a path.
//! The workspace's relation set is what says which keys those are, so a
//! workspace that spells containment `whole`/`part` is read correctly
//! without anyone naming the pair here; a consumer with structural keys of
//! its own passes them to [`corpus`] beside it (see [`Excluded`]).
//!
//! # Two steps, kept apart
//!
//! Matching is a pure scan over folded text and answers *which* documents,
//! ranked — no filesystem, so every ranking question is a unit test. A hit is
//! ranked by how much of the query its title holds, and its passage is cut
//! from the body when any of the words is there, because the title is on the
//! row already and a passage is for what the row does not show. The passage
//! under each hit needs the document's raw prose, and the corpus does not
//! keep it: folded text is one copy of the archive in memory, and the raw
//! text would be a second for the sake of the dozen documents a query shows.
//! So [`search`] reads a body back for each body hit it reports — inside one
//! read scope for the whole answer — and cuts the window around the match.

use std::path::{Path, PathBuf};

use prov_graph::document::MetaCarrier;
use prov_graph::fs::ReadStorage;
use prov_graph::graph::Graph;
use prov_graph::index::IdIndex;
use prov_graph::meta::Value;
use prov_graph::relation::RelationSet;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::error::Result;
use crate::select::documents;

/// Keys that are identifiers or file names whatever the relation set says:
/// the document's own id, the path of a separated body or a manifest, and a
/// node's name for its root.
const IDENTIFIER_KEYS: &[&str] = &["id", "content", "manifest", "root"];

/// How much of a body is shown either side of the match, in characters.
const PASSAGE_HALF: usize = 90;

/// Case and diacritics removed, so a query and a text compare the way a
/// person reads them.
///
/// NFD decomposition, combining marks dropped, then lowercased: `Ålesund`
/// and `ålesund` and `Alesund` fold to one spelling. Byte offsets into the
/// result do not correspond to offsets into the input — the passage cutter
/// keeps its own map back where that matters.
pub fn fold(text: &str) -> String {
    text.nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// [`fold`], plus for every byte of the folded text the byte offset in
/// `text` of the character it came from — what lets a match found in folded
/// text be shown from the raw text.
fn fold_with_origins(text: &str) -> (String, Vec<usize>) {
    let mut folded = String::with_capacity(text.len());
    let mut origins = Vec::with_capacity(text.len());
    for (at, c) in text.char_indices() {
        let start = folded.len();
        for d in c.nfd().filter(|d| !is_combining_mark(*d)) {
            for l in d.to_lowercase() {
                folded.push(l);
            }
        }
        origins.extend(std::iter::repeat_n(at, folded.len() - start));
    }
    (folded, origins)
}

/// The metadata keys a corpus leaves unread.
///
/// Every relation key the workspace declares, its inverse included, plus the
/// identifier keys (`id`, `content`, `manifest`, `root`), plus whatever the
/// consumer adds. The relation keys come from the workspace's own
/// [`RelationSet`] rather than from a list here, so a workspace that renames
/// its spanning pair, or declares a relation of its own, has it excluded
/// without this module knowing the name — the rule is *a link's label is
/// another document's title*, and the relation set is what says which keys
/// hold links.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Excluded {
    keys: Vec<String>,
}

impl Excluded {
    /// The keys `relations` declares, the identifier keys, and `extra` — the
    /// consumer's own structural keys, for a pair it maintains beside prov's
    /// relations and would otherwise be searched as prose.
    pub fn from_relations(relations: &RelationSet, extra: &[&str]) -> Self {
        let mut keys: Vec<String> = IDENTIFIER_KEYS.iter().map(|k| (*k).to_string()).collect();
        for relation in relations.relations() {
            keys.push(relation.name.clone());
            if let Some(inverse) = &relation.inverse {
                keys.push(inverse.clone());
            }
        }
        keys.extend(extra.iter().map(|k| (*k).to_string()));
        keys.sort();
        keys.dedup();
        Self { keys }
    }

    /// Whether `key` is left unread.
    pub fn contains(&self, key: &str) -> bool {
        self.keys.binary_search_by(|k| k.as_str().cmp(key)).is_ok()
    }
}

/// The words being looked for, folded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    terms: Vec<String>,
}

impl Query {
    /// Split `text` on whitespace and fold each word. `None` when there is
    /// nothing to look for — a search for nothing is not a search for
    /// everything.
    pub fn parse(text: &str) -> Option<Self> {
        let terms: Vec<String> = text
            .split_whitespace()
            .map(fold)
            .filter(|t| !t.is_empty())
            .collect();
        (!terms.is_empty()).then_some(Self { terms })
    }

    /// The folded terms, in the order they were typed.
    pub fn terms(&self) -> &[String] {
        &self.terms
    }
}

/// Where in a document a query was found — which of the three places the
/// passage is cut from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Site {
    /// The title.
    Title,
    /// A metadata value, under this key.
    Field(String),
    /// The prose body.
    Body,
}

/// What [`IndexedDoc::matches`] found: where to cut the passage, and how
/// far the document is from a perfect answer.
struct Match {
    site: Site,
    /// Terms the title does not hold.
    missing_title: usize,
    /// Terms neither the title nor any field holds — found in the body.
    missing_fields: usize,
}

/// One metadata value, flattened to text.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Field {
    key: String,
    raw: String,
    folded: String,
}

/// One document as the search reads it: everything folded, nothing raw but
/// the title and the field values, which are short.
#[derive(Clone, Debug)]
pub struct IndexedDoc {
    path: PathBuf,
    title: String,
    folded_title: String,
    fields: Vec<Field>,
    /// `None` for a document with no prose to carry — an attachment sidecar,
    /// a whole-file node with no `content` — as distinct from an empty body.
    folded_body: Option<String>,
}

impl IndexedDoc {
    /// Index one document from its parts. `body` is `None` for a document
    /// that has no prose to read; `excluded` is what [`corpus`] derives from
    /// the workspace, offered here so a corpus can be assembled without one.
    pub fn new(
        path: PathBuf,
        title: String,
        meta: &Value,
        body: Option<&str>,
        excluded: &Excluded,
    ) -> Self {
        let mut fields = Vec::new();
        if let Some(map) = meta.as_mapping() {
            for (key, value) in map {
                if excluded.contains(key) {
                    continue;
                }
                flatten_value(key, value, &mut fields);
            }
        }
        Self {
            folded_title: fold(&title),
            fields,
            folded_body: body.map(fold),
            path,
            title,
        }
    }

    /// Whether every term occurs somewhere in this document, and if so how
    /// well it answers and where the passage should be cut from.
    ///
    /// The answer's quality is two counts: the terms the title does *not*
    /// hold, and the terms neither the title nor a field holds — fewer is
    /// better, in that order. The passage site is the opposite preference:
    /// the body if any term is in it, else a field, else the title — because
    /// the title is on the row already, and a passage is for showing what the
    /// row does not. Within a class, the earliest term decides.
    fn matches(&self, query: &Query) -> Option<Match> {
        let mut missing_title = 0;
        let mut missing_fields = 0;
        let mut in_body = false;
        let mut in_field: Option<&str> = None;
        for term in &query.terms {
            if self.folded_title.contains(term.as_str()) {
                continue;
            }
            missing_title += 1;
            if let Some(field) = self
                .fields
                .iter()
                .find(|f| f.folded.contains(term.as_str()))
            {
                if in_field.is_none() {
                    in_field = Some(&field.key);
                }
                continue;
            }
            missing_fields += 1;
            if self
                .folded_body
                .as_deref()
                .is_some_and(|b| b.contains(term.as_str()))
            {
                in_body = true;
            } else {
                return None;
            }
        }
        let site = if in_body {
            Site::Body
        } else if let Some(key) = in_field {
            Site::Field(key.to_string())
        } else {
            Site::Title
        };
        Some(Match {
            site,
            missing_title,
            missing_fields,
        })
    }

    /// The field named `key`, raw, for a passage.
    fn field(&self, key: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.key == key)
    }
}

/// A scalar's text, or every scalar's under a sequence or mapping, each as
/// its own field under the top-level key — so `people: [Ada, Bob]` is two
/// values under `people`, and `written: {on: 1943, by: Ada}` is two under
/// `written`. A nested key is not reported: what the passage shows is the
/// value and the author's own top-level name for it.
fn flatten_value(key: &str, value: &Value, out: &mut Vec<Field>) {
    match value {
        Value::Null => {}
        Value::Bool(b) => push_field(key, b.to_string(), out),
        Value::Int(i) => push_field(key, i.to_string(), out),
        Value::Float(f) => push_field(key, f.to_string(), out),
        Value::String(s) => push_field(key, s.clone(), out),
        Value::Sequence(items) => {
            for item in items {
                flatten_value(key, item, out);
            }
        }
        Value::Mapping(map) => {
            for (_, inner) in map {
                flatten_value(key, inner, out);
            }
        }
    }
}

fn push_field(key: &str, raw: String, out: &mut Vec<Field>) {
    if raw.trim().is_empty() {
        return;
    }
    out.push(Field {
        key: key.to_string(),
        folded: fold(&raw),
        raw,
    });
}

/// Every document the workspace reaches, folded — what a query scans.
///
/// Built by [`corpus`], held by whoever asked, and good until the workspace
/// is written to. Nothing here knows when that was; the holder decides when
/// to build another.
#[derive(Clone, Debug, Default)]
pub struct Corpus {
    docs: Vec<IndexedDoc>,
}

impl Corpus {
    /// A corpus from documents already indexed — the test seam, and what
    /// [`corpus`] assembles.
    pub fn from_docs(docs: Vec<IndexedDoc>) -> Self {
        Self { docs }
    }

    /// How many documents the corpus holds.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// The documents matching `query`, best first, at most `limit` of them.
    ///
    /// Ranking is by how many of the terms the title holds, most first, then
    /// by how many the title and the fields hold together, and then by the
    /// corpus's own order, which is the census's path order. Pure: no
    /// filesystem, so a ranking claim is a unit test. Crate-private so the
    /// rule can change without the crate's surface changing.
    pub(crate) fn matches(&self, query: &Query, limit: usize) -> Vec<Candidate<'_>> {
        let mut found: Vec<(usize, usize, Candidate<'_>)> = self
            .docs
            .iter()
            .enumerate()
            .filter_map(|(order, doc)| {
                let m = doc.matches(query)?;
                Some((
                    m.missing_title,
                    m.missing_fields,
                    Candidate {
                        doc,
                        site: m.site,
                        order,
                    },
                ))
            })
            .collect();
        found.sort_by_key(|(title, fields, c)| (*title, *fields, c.order));
        found.into_iter().take(limit).map(|(_, _, c)| c).collect()
    }
}

/// A document [`Corpus::matches`] found, and where.
#[derive(Clone, Debug)]
pub(crate) struct Candidate<'a> {
    pub(crate) doc: &'a IndexedDoc,
    pub(crate) site: Site,
    /// Position in the corpus — the tie-breaker, and stable across queries.
    pub(crate) order: usize,
}

/// A passage with the match marked: the text before it, the match itself as
/// it is spelled in the document, and the text after.
///
/// Three strings rather than one and a range, so that whoever draws it need
/// not know whether the range counts bytes, scalars or UTF-16 units — a
/// question with a different answer on each side of a language boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Passage {
    /// What precedes the match, cut at a word and marked with an ellipsis
    /// where it was cut.
    pub before: String,
    /// The match as the document spells it.
    pub matched: String,
    /// What follows the match, cut the same way.
    pub after: String,
}

/// The window of `text` around the first occurrence of any of `query`'s terms,
/// or `None` when no term occurs in it.
///
/// Whitespace is collapsed on either side so a match in the middle of a
/// paragraph reads as one line, and an ellipsis marks each cut. The match is
/// found in folded text and shown from the raw, so `Ålesund` is what the
/// passage says however the query spelled it.
fn passage(text: &str, query: &Query) -> Option<Passage> {
    let (folded, origins) = fold_with_origins(text);
    let (at, len) = query
        .terms
        .iter()
        .filter_map(|term| folded.find(term.as_str()).map(|at| (at, term.len())))
        .min_by_key(|(at, _)| *at)?;
    let start = origins[at];
    let end = origins
        .get(at + len)
        .copied()
        .unwrap_or(text.len())
        .max(start + text[start..].chars().next().map_or(0, char::len_utf8));

    // The document's own edges are not part of the passage: a body that
    // opens with a blank line would otherwise open the passage with a space.
    let before = tail(collapse(&text[..start]).trim_start(), PASSAGE_HALF);
    let after = head(collapse(&text[end..]).trim_end(), PASSAGE_HALF);
    Some(Passage {
        before,
        matched: text[start..end].to_string(),
        after,
    })
}

/// Whitespace runs to one space, and the outer edges kept so the join with
/// the match reads as the text did — `"a  b"` is `"a b"`, `" a"` stays `" a"`.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

/// The last `chars` characters of `text`, cut at a word and marked.
fn tail(text: &str, chars: usize) -> String {
    let count = text.chars().count();
    if count <= chars {
        return text.to_string();
    }
    let skip = count - chars;
    let cut: String = text.chars().skip(skip).collect();
    // Cut at the first space so the window does not open on half a word.
    let at_word = cut.find(' ').map(|i| i + 1).unwrap_or(0);
    format!("…{}", &cut[at_word..])
}

/// The first `chars` characters of `text`, cut at a word and marked.
fn head(text: &str, chars: usize) -> String {
    let count = text.chars().count();
    if count <= chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(chars).collect();
    let at_word = cut.rfind(' ').unwrap_or(cut.len());
    format!("{}…", &cut[..at_word])
}

/// One document the search reports: the document, where the words were
/// found, and the passage that shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// Workspace-relative, normalized path, as the census lists it.
    pub path: PathBuf,
    /// The document's title, or its file name's stem when it declares none.
    pub title: String,
    /// Where the passage was cut from.
    pub site: Site,
    /// The passage, with the match marked.
    pub passage: Passage,
}

/// The name a document shows when it declares none: the file's name with its
/// last extension off, so `photo.jpg.yaml` — a sidecar — is `photo.jpg`.
fn stem_title(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Every document the workspace reaches from `root_doc`, folded for
/// searching.
///
/// One read scope over the census and the bodies, so each document is parsed
/// once. A document with no prose — an attachment sidecar, a whole-file node
/// with no `content` — is indexed by its title and fields alone. A `content`
/// that names a missing file is an error here as it is for `prov docs
/// --body`: that document claims a body it has not got, which is a `check`
/// finding and not a document without one.
///
/// `exclude` is the consumer's own structural keys, left unread beside the
/// workspace's relation keys — see [`Excluded`]. Pass `&[]` when there are
/// none.
pub async fn corpus<FS: ReadStorage, Ix: IdIndex>(
    graph: &Graph<FS, Ix>,
    root_doc: impl AsRef<Path>,
    exclude: &[&str],
) -> Result<Corpus> {
    let _scope = graph.read_scope();
    let excluded = Excluded::from_relations(graph.relations(), exclude);
    let rows = documents(graph, root_doc).await?;
    let mut docs = Vec::with_capacity(rows.len());
    for row in rows {
        let doc = graph.document(&row.path).await?;
        let has_body = !doc.is_attachment()
            && !(matches!(doc.carrier, Some(MetaCarrier::WholeFile(_)))
                && doc.content_attr().is_none());
        let body = if has_body {
            Some(graph.body(&row.path).await?.text)
        } else {
            None
        };
        let title = row
            .title()
            .map(str::to_owned)
            .unwrap_or_else(|| stem_title(&row.path));
        docs.push(IndexedDoc::new(
            row.path,
            title,
            &row.meta,
            body.as_deref(),
            &excluded,
        ));
    }
    Ok(Corpus { docs })
}

/// The documents in `corpus` that match `query`, best first, each with the
/// passage that shows why — at most `limit` of them.
///
/// A hit found in the title or a field carries that text; one found in the
/// body has the body read back for its window, inside one read scope for the
/// whole answer. A body that cannot be read any more — deleted between the
/// corpus and the query — is a hit with no passage, and is dropped: the
/// corpus is a memory, and a memory of a document that has gone is not a
/// result. That is the only thing that can go wrong here, and it is not an
/// error, so this cannot fail.
pub async fn search<FS: ReadStorage, Ix: IdIndex>(
    graph: &Graph<FS, Ix>,
    corpus: &Corpus,
    query: &Query,
    limit: usize,
) -> Vec<Hit> {
    let _scope = graph.read_scope();
    let mut hits = Vec::new();
    for candidate in corpus.matches(query, limit) {
        let doc = candidate.doc;
        let passage = match &candidate.site {
            Site::Title => passage(&doc.title, query),
            Site::Field(key) => doc
                .field(key)
                .and_then(|f| passage(&f.raw, query))
                .map(|p| Passage {
                    before: format!("{key}: {}", p.before),
                    ..p
                }),
            Site::Body => match graph.body(&doc.path).await {
                Ok(body) => passage(&body.text, query),
                Err(_) => None,
            },
        };
        let Some(passage) = passage else { continue };
        hits.push(Hit {
            path: doc.path.clone(),
            title: doc.title.clone(),
            site: candidate.site,
            passage,
        });
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use prov_graph::meta::Mapping;

    fn meta(pairs: &[(&str, Value)]) -> Value {
        let mut map = Mapping::new();
        for (k, v) in pairs {
            map.insert((*k).to_string(), v.clone());
        }
        Value::Mapping(map)
    }

    fn s(text: &str) -> Value {
        Value::String(text.to_string())
    }

    fn defaults() -> Excluded {
        Excluded::from_relations(&RelationSet::diaryx(), &[])
    }

    fn doc(path: &str, title: &str, meta: &Value, body: Option<&str>) -> IndexedDoc {
        IndexedDoc::new(
            PathBuf::from(path),
            title.to_string(),
            meta,
            body,
            &defaults(),
        )
    }

    #[test]
    fn folding_drops_case_and_diacritics() {
        assert_eq!(fold("Ålesund"), "alesund");
        assert_eq!(fold("CAFÉ"), "cafe");
        assert_eq!(fold("Straße"), "straße");
    }

    #[test]
    fn a_query_is_its_words_folded_and_nothing_is_no_query() {
        assert_eq!(
            Query::parse("  Ogden   Pen ").unwrap().terms(),
            ["ogden", "pen"]
        );
        assert!(Query::parse("   ").is_none());
        assert!(Query::parse("").is_none());
    }

    #[test]
    fn every_term_must_occur_but_not_in_one_place() {
        let letter = doc(
            "letters/ogden.md",
            "From Ogden",
            &meta(&[("people", s("Ada"))]),
            Some("She wrote about the pen she lost."),
        );
        let other = doc("notes.md", "Notes", &meta(&[]), Some("A pen and paper."));
        let corpus = Corpus::from_docs(vec![letter, other]);

        let q = Query::parse("ogden pen").unwrap();
        let found = corpus.matches(&q, 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].doc.path, Path::new("letters/ogden.md"));
        // The title is on the row already; the passage shows the body.
        assert_eq!(found[0].site, Site::Body);

        let q = Query::parse("pen").unwrap();
        assert_eq!(corpus.matches(&q, 10).len(), 2);
    }

    #[test]
    fn a_title_hit_outranks_a_field_hit_outranks_a_body_hit() {
        let body = doc("c.md", "Third", &meta(&[]), Some("Ogden again"));
        let field = doc("b.md", "Second", &meta(&[("places", s("Ogden"))]), Some(""));
        let title = doc("a.md", "Ogden", &meta(&[]), Some("nothing"));
        let corpus = Corpus::from_docs(vec![body, field, title]);
        let q = Query::parse("ogden").unwrap();
        let found: Vec<_> = corpus
            .matches(&q, 10)
            .into_iter()
            .map(|c| (c.doc.path.to_string_lossy().into_owned(), c.site))
            .collect();
        assert_eq!(
            found,
            vec![
                ("a.md".to_string(), Site::Title),
                ("b.md".to_string(), Site::Field("places".to_string())),
                ("c.md".to_string(), Site::Body),
            ]
        );
    }

    #[test]
    fn a_title_holding_more_of_the_words_comes_first() {
        let half = doc("half.md", "Ogden", &meta(&[]), Some("a letter"));
        let whole = doc("whole.md", "Ogden letter", &meta(&[]), Some(""));
        let corpus = Corpus::from_docs(vec![half, whole]);
        let q = Query::parse("ogden letter").unwrap();
        let found = corpus.matches(&q, 10);
        assert_eq!(found[0].doc.path, Path::new("whole.md"));
        assert_eq!(found[1].doc.path, Path::new("half.md"));
    }

    #[test]
    fn the_limit_is_honoured() {
        let docs: Vec<_> = (0..5)
            .map(|i| doc(&format!("{i}.md"), "x", &meta(&[]), Some("pen")))
            .collect();
        let corpus = Corpus::from_docs(docs);
        assert_eq!(corpus.matches(&Query::parse("pen").unwrap(), 2).len(), 2);
    }

    #[test]
    fn structural_fields_are_not_read_and_sequences_and_numbers_are() {
        let m = meta(&[
            ("part_of", s("[Ogden letters](../ogden.md)")),
            ("contents", Value::Sequence(vec![s("[Ogden](ogden.md)")])),
            ("people", Value::Sequence(vec![s("Ada"), s("Bob")])),
            ("year", Value::Int(1943)),
            ("provenance", s("Found in the attic")),
        ]);
        let d = doc("x.md", "Untitled", &m, Some(""));
        let corpus = Corpus::from_docs(vec![d]);
        assert!(
            corpus
                .matches(&Query::parse("ogden").unwrap(), 10)
                .is_empty()
        );
        assert_eq!(
            corpus.matches(&Query::parse("bob").unwrap(), 10)[0].site,
            Site::Field("people".to_string())
        );
        assert_eq!(
            corpus.matches(&Query::parse("1943").unwrap(), 10)[0].site,
            Site::Field("year".to_string())
        );
        assert_eq!(
            corpus.matches(&Query::parse("attic").unwrap(), 10)[0].site,
            Site::Field("provenance".to_string())
        );
    }

    /// The spanning pair is not the only relation: a `links` entry names the
    /// linked document by its title just as `part_of` names the parent, and
    /// the relation set is what says so.
    #[test]
    fn every_relation_key_is_left_unread_not_only_the_spanning_pair() {
        let m = meta(&[
            (
                "links",
                Value::Sequence(vec![s("[Ogden letters](../ogden.md)")]),
            ),
            ("derived_from", s("[Ogden scan](../scan.jpg.yaml)")),
            ("config", s("prov.yaml")),
            ("subject", s("the pen")),
        ]);
        let d = doc("x.md", "Untitled", &m, Some(""));
        let corpus = Corpus::from_docs(vec![d]);
        assert!(
            corpus
                .matches(&Query::parse("ogden").unwrap(), 10)
                .is_empty()
        );
        assert!(
            corpus
                .matches(&Query::parse("prov").unwrap(), 10)
                .is_empty()
        );
        assert_eq!(
            corpus.matches(&Query::parse("pen").unwrap(), 10)[0].site,
            Site::Field("subject".to_string())
        );

        // A workspace that spells the pair differently is read by its own
        // relation set, not by the names above.
        let whole_part = RelationSet::new()
            .with(prov_graph::relation::Relation::many("whole").inverse("part"))
            .with(prov_graph::relation::Relation::one("part").inverse("whole"))
            .spanning("whole");
        let excluded = Excluded::from_relations(&whole_part, &[]);
        assert!(excluded.contains("whole") && excluded.contains("part"));
        assert!(!excluded.contains("links"));
        assert!(excluded.contains("id") && excluded.contains("content"));
    }

    /// A consumer that keeps a pair of its own beside prov's relations names
    /// it, and it is left unread as the relations are.
    #[test]
    fn the_callers_extra_keys_are_left_unread() {
        let excluded = Excluded::from_relations(
            &RelationSet::diaryx(),
            &["transcription", "transcription_of"],
        );
        let m = meta(&[
            ("transcription_of", s("[Ogden scan](../scan.jpg.yaml)")),
            ("people", s("Ogden")),
        ]);
        let d = IndexedDoc::new(
            PathBuf::from("x.md"),
            "Untitled".to_string(),
            &m,
            Some(""),
            &excluded,
        );
        let corpus = Corpus::from_docs(vec![d]);
        let found = corpus.matches(&Query::parse("ogden").unwrap(), 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].site, Site::Field("people".to_string()));

        let found = corpus.matches(&Query::parse("scan").unwrap(), 10);
        assert!(found.is_empty());
    }

    #[test]
    fn a_document_without_prose_matches_by_its_title_and_fields_alone() {
        let sidecar = doc(
            "scans/letter.jpg.yaml",
            "letter.jpg",
            &meta(&[("provenance", s("the pen letter"))]),
            None,
        );
        let corpus = Corpus::from_docs(vec![sidecar]);
        assert_eq!(corpus.matches(&Query::parse("pen").unwrap(), 10).len(), 1);
        assert!(
            corpus
                .matches(&Query::parse("prose").unwrap(), 10)
                .is_empty()
        );
    }

    #[test]
    fn the_passage_shows_the_raw_spelling_around_the_match() {
        let q = Query::parse("alesund").unwrap();
        let p = passage("We sailed  to\nÅlesund in May.", &q).unwrap();
        assert_eq!(p.before, "We sailed to ");
        assert_eq!(p.matched, "Ålesund");
        assert_eq!(p.after, " in May.");
    }

    #[test]
    fn the_passage_is_a_window_cut_at_words() {
        let long = format!("{} pen {}", "word ".repeat(60), "after ".repeat(60));
        let q = Query::parse("pen").unwrap();
        let p = passage(&long, &q).unwrap();
        assert!(p.before.starts_with('…'));
        assert!(p.before.ends_with("word "));
        assert!(p.before.chars().count() <= PASSAGE_HALF + 1);
        assert!(p.after.ends_with('…'));
        assert!(p.after.starts_with(" after"));
        assert!(p.after.chars().count() <= PASSAGE_HALF + 1);
        assert_eq!(p.matched, "pen");
    }

    #[test]
    fn the_passage_is_around_the_earliest_of_the_terms() {
        let q = Query::parse("pen ink").unwrap();
        let p = passage("ink first, then the pen", &q).unwrap();
        assert_eq!(p.matched, "ink");
        assert!(passage("neither", &q).is_none());
    }

    #[test]
    fn a_match_at_the_very_end_of_the_text_is_whole() {
        let q = Query::parse("café").unwrap();
        let p = passage("At the CAFÉ", &q).unwrap();
        assert_eq!(p.matched, "CAFÉ");
        assert_eq!(p.after, "");
    }

    /// The whole thing over a workspace: the census, the bodies, a sidecar
    /// with no prose, and the passage read back for a body hit.
    #[test]
    fn a_workspace_is_searched_by_title_field_and_body_with_the_passage_read_back() {
        use prov_graph::exec::block_on;
        use prov_graph::fs::StdFs;
        use prov_graph::graph::ReadSettings;
        use prov_graph::index::NoIndex;
        use prov_testkit::write;

        let dir = prov_testkit::scratch("search", "workspace");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n  - '[Letter](letters/ogden.md)'\n  - '[Scan](scans/letter.jpg.yaml)'\n---\n# Home\n",
        );
        write(
            &dir,
            "letters/ogden.md",
            "---\ntitle: From Ogden\npart_of: '[Home](../index.md)'\npeople: [Ada]\n---\nShe wrote that the pen had been her mother's, and\nthat she had lost it at \u{c5}lesund.\n",
        );
        write(
            &dir,
            "scans/letter.jpg.yaml",
            "content: letter.jpg\npart_of: '[Home](../index.md)'\nprovenance: The pen letter, photographed in 2019\n",
        );
        write(&dir, "scans/letter.jpg", b"\xff\xd8");

        let graph = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        block_on(async {
            let corpus = corpus(&graph, "index.md", &[]).await.unwrap();
            assert_eq!(corpus.len(), 3, "the root, the letter and the sidecar");

            // A body hit, with the passage read back from the file and the
            // raw spelling kept.
            let hits = search(&graph, &corpus, &Query::parse("alesund").unwrap(), 10).await;
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].path, Path::new("letters/ogden.md"));
            assert_eq!(hits[0].title, "From Ogden");
            assert_eq!(hits[0].site, Site::Body);
            assert_eq!(hits[0].passage.matched, "\u{c5}lesund");
            assert!(hits[0].passage.before.ends_with("lost it at "));

            // The sidecar has no prose; it answers by its field, and the
            // passage names the field.
            let hits = search(&graph, &corpus, &Query::parse("photographed").unwrap(), 10).await;
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].path, Path::new("scans/letter.jpg.yaml"));
            assert_eq!(hits[0].title, "letter.jpg");
            assert_eq!(hits[0].site, Site::Field("provenance".to_string()));
            assert!(hits[0].passage.before.starts_with("provenance: "));

            // Both mention the pen; neither title does, so the field hit
            // outranks the body hit.
            let hits = search(&graph, &corpus, &Query::parse("pen").unwrap(), 10).await;
            let paths: Vec<_> = hits.iter().map(|h| h.path.clone()).collect();
            assert_eq!(
                paths,
                vec![
                    PathBuf::from("scans/letter.jpg.yaml"),
                    PathBuf::from("letters/ogden.md")
                ],
                "a field hit outranks a body hit"
            );

            // The parent's name reaches the child only through `part_of`,
            // which is not read.
            let hits = search(&graph, &corpus, &Query::parse("home").unwrap(), 10).await;
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].path, Path::new("index.md"));

            // The body is read back from the file at query time, so a body
            // that has gone since the corpus was built is not a result.
            std::fs::remove_file(dir.join("letters/ogden.md")).unwrap();
            let hits = search(&graph, &corpus, &Query::parse("alesund").unwrap(), 10).await;
            assert!(hits.is_empty());
        });
    }
}
