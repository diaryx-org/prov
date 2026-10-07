//! Provenance — how a document came to exist, and who has confirmed it since.
//!
//! Two frontmatter families, read here and written by exactly one verb each:
//!
//! - **`generated: {by, at, how?}`** — how the document came to exist. Written
//!   once, by whatever created it, and never maintained; the mapping is a fact
//!   about an event. prov reads it only to say what kind of actor wrote the
//!   document. `how` is the act — `drafted`, `transcribed`, `imported` — that
//!   tells a note the actor composed from one it merely carried across, which
//!   is the distinction that decides how much of the content is the actor's.
//!   prov carries it and ships no terms for it; a workspace closes its
//!   vocabulary with a `fields: generated.how:` declaration, as it closes
//!   `status`. In PROV-O's terms `by` is `prov:wasAttributedTo`, `at` is
//!   `prov:generatedAtTime`, and `how` names the activity a `prov:wasGeneratedBy`
//!   would point at, so an exporter maps all three without a gloss.
//! - **`confirmed: [{by, at, of?}]`** — an append-only list of dated,
//!   attributed statements that someone read the document and found it
//!   correct. [`Workspace::confirm`](crate::Workspace::confirm) appends one;
//!   nothing else writes the list, and nothing rewrites or drops an entry
//!   beyond naming a declared person by their link (`actors: declared`, and
//!   never an entry carrying another tool's keys).
//!
//! This is the **keeper's own ledger**: a line inside the document, written by
//! whoever can edit it, and exactly as trustworthy as the `author` field beside
//! it — a claim, not evidence. A signed, second-person vouch (a reviewer's own
//! record of what they reviewed, which the document's keeper cannot edit) is a
//! different tool's subject and is not attempted here.
//!
//! # The distinction that keeps it honest
//!
//! `check` is the **attester**: every finding it produces is a claim about
//! *state*, computed now and thrown away. A confirmation is a stored claim
//! about *meaning*, made once and kept. Neither is evidence for the other, so
//! prov never writes a confirmation from a passing `check`, and a stored
//! confirmation never suppresses a finding — no finding kind consults
//! `confirmed` when deciding whether to fire.
//!
//! # What a confirmation is bound to
//!
//! A confirmation is **stale** when the document changed after it was made,
//! and the record of a change is the workspace's own `updated` stamp: an entry
//! whose `at` is older than the document's `updated` instant describes a
//! document that no longer exists. Both are the same clock in the same
//! fixed-width RFC 3339 spelling, so the comparison is one a reader makes by
//! eye. Where the document records a `content_hash` — an attachment sidecar,
//! a separated node, a manifest node — the entry also names that digest as
//! `of`, and is stale when the digest on record has moved.
//!
//! What that accepts, stated plainly: an edit made outside prov that never
//! bumps the stamp is invisible here. That is the hole `updated` already has,
//! and a confirmation inherits it rather than adding to it. A workspace that
//! keeps no `updated` field gets staleness on the digest-bearing shapes only.
//!
//! # Binding to content instead
//!
//! A workspace that sets `confirmations: content`
//! ([`ConfirmationBinding::Content`](crate::config::ConfirmationBinding))
//! closes that hole. Every confirmation on a document without a
//! `content_hash` names the document's [content digest](content_digest) as
//! `of`, and an entry that carries one stands **exactly** while its `of` is
//! still the content digest. The stamp is not consulted for it: the content
//! digest covers the stamp along with every other byte.
//!
//! The content digest is the digest of the document **without its `confirmed`
//! list**, because a digest of the whole file would move with every entry
//! appended to it, and no confirmation could ever name the thing it confirms.
//! Which bytes are bookkeeping is this module's knowledge, so the rule lives
//! here, as a function of a path and a text that an outside tool can apply to
//! the bytes a document had at a past revision.
//!
//! # Actors
//!
//! A bare actor is a person. A non-human carries a prefix — `agent:` for a
//! model acting with judgment, `process:` for a program acting without it. The
//! burden sits with the party that can bear it mechanically: tools are code,
//! and code does not forget a prefix once told, where a `human:` on every line
//! of a workspace full of a person's own work would be boilerplate forever.
//! What that accepts is that a tool which omits its prefix is counted as a
//! person — a bug in one tool, fixed once.
//!
//! The **tier** a document earns is derived from its live entries and never
//! stored: storing it would be storing a conclusion, and a conclusion goes
//! stale the moment an entry is appended or the document edited.

use std::path::Path;

use crate::meta::Value;
use prov_graph::document::Document;
use prov_graph::error::Result;

/// The frontmatter key holding the confirmation list.
pub const CONFIRMED: &str = "confirmed";
/// The frontmatter key recording how the document came to exist.
pub const GENERATED: &str = "generated";

const AGENT_PREFIX: &str = "agent:";
const PROCESS_PREFIX: &str = "process:";

/// Who made a statement — a person, or one of the two kinds of non-human.
///
/// Read off the actor string's prefix and nothing else: the identifier after
/// the prefix is the user's (tier 3) and prov never reasons about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// A bare actor: `amh`, `Adam Harris`. What a person writes.
    Person(String),
    /// `agent:<id>` — a model, acting with judgment.
    Agent(String),
    /// `process:<id>` — a program, acting without it. prov's own stamps are
    /// this kind.
    Process(String),
}

impl Actor {
    /// Classify an actor string by its prefix.
    pub fn parse(raw: &str) -> Actor {
        if let Some(id) = raw.strip_prefix(AGENT_PREFIX) {
            Actor::Agent(id.to_string())
        } else if let Some(id) = raw.strip_prefix(PROCESS_PREFIX) {
            Actor::Process(id.to_string())
        } else {
            Actor::Person(raw.to_string())
        }
    }

    /// Whether this actor is a person — the one question the tier asks.
    pub fn is_person(&self) -> bool {
        matches!(self, Actor::Person(_))
    }

    /// The identifier after any prefix.
    pub fn id(&self) -> &str {
        match self {
            Actor::Person(id) | Actor::Agent(id) | Actor::Process(id) => id,
        }
    }
}

impl std::fmt::Display for Actor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Actor::Person(id) => f.write_str(id),
            Actor::Agent(id) => write!(f, "{AGENT_PREFIX}{id}"),
            Actor::Process(id) => write!(f, "{PROCESS_PREFIX}{id}"),
        }
    }
}

/// The fields that name an actor, as field paths: who generated a document,
/// and who confirmed it. Under `actors: declared` each value is either
/// prefixed (`agent:`, `process:`) or a link to a person document.
pub const ACTOR_FIELDS: &[&str] = &["generated.by", "confirmed[].by"];

/// The field a person document lists the strings that stood for that person
/// in: `handles: [amh, Adam Harris]`.
pub const HANDLES: &str = "handles";

/// The strings a document's `handles:` lists, trimmed, in order. A document
/// whose list is empty or absent is not a person document.
pub fn handles_of(meta: &Value) -> Vec<String> {
    prov_graph::field::strings_at(
        meta,
        &prov_graph::field::FieldPath::parse(&format!("{HANDLES}[]")),
    )
    .into_iter()
    .map(|(_, handle)| handle.trim().to_string())
    .filter(|handle| !handle.is_empty())
    .collect()
}

/// Every actor value a document states: the field path it sits at, the value,
/// and whether it is **sealed** — in a confirmation entry that carries keys
/// beside prov's own `by`, `at` and `of`, which another tool may have signed
/// over the entry as written.
pub fn actors_of(meta: &Value) -> Vec<(&'static str, String, bool)> {
    let mut out = Vec::new();
    if let Some(by) = meta
        .get(GENERATED)
        .and_then(|generated| generated.get("by"))
        .and_then(Value::as_str)
    {
        out.push((ACTOR_FIELDS[0], by.to_string(), false));
    }
    for entry in meta
        .get(CONFIRMED)
        .and_then(Value::as_sequence)
        .unwrap_or_default()
    {
        let Some(by) = entry.get("by").and_then(Value::as_str) else {
            continue;
        };
        let sealed = entry.as_mapping().is_some_and(|map| {
            map.keys()
                .any(|key| !matches!(key.as_str(), "by" | "at" | "of"))
        });
        out.push((ACTOR_FIELDS[1], by.to_string(), sealed));
    }
    out
}

/// Whether an actor value names a person by a bare string rather than by a
/// link: not prefixed, and not written as a link (`[label](target)`,
/// `[[target]]`) or an id reference (`id:<id>`).
pub fn is_bare_person(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.is_empty() || !Actor::parse(raw).is_person() {
        return false;
    }
    let link = prov_graph::link::Link::parse(raw);
    link.label.is_none() && !link.wikilink && link.id_ref().is_none()
}

/// How a document came to exist — the `generated` mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated {
    /// Who or what created it.
    pub by: String,
    /// When, RFC 3339 UTC.
    pub at: String,
    /// What the actor was doing when the document came to be — `drafted`,
    /// `transcribed`, `imported`. A term of the workspace's own choosing
    /// (tier 3, like the identifier after an actor's prefix), absent on every
    /// document written before the key existed and on any that never says.
    pub how: Option<String>,
}

impl Generated {
    /// The `generated` mapping a document records, if it records a well-formed
    /// one — a mapping with string `by` and `at`. `how` is read when it is a
    /// string and is never required.
    pub fn read(meta: &Value) -> Option<Generated> {
        let map = meta.get(GENERATED)?.as_mapping()?;
        Some(Generated {
            by: map.get("by")?.as_str()?.to_string(),
            at: map.get("at")?.as_str()?.to_string(),
            how: map.get("how").and_then(Value::as_str).map(str::to_string),
        })
    }

    /// The kind of actor that created the document.
    pub fn actor(&self) -> Actor {
        Actor::parse(&self.by)
    }
}

/// One entry of the `confirmed` list: a dated, attributed statement that
/// someone read the document and found it correct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    /// Who confirmed — see [`Actor`].
    pub by: String,
    /// When, RFC 3339 UTC, fixed width — the same clock as the workspace's
    /// edit stamp, which it is compared with as an instant (or, against a
    /// `type: date` stamp, as a day).
    pub at: String,
    /// The `content_hash` the document recorded at the moment of confirming,
    /// for a document that records one. On any other document, the document's
    /// [content digest](content_digest) at that moment, in a workspace that
    /// binds confirmations to content; absent in one that does not.
    pub of: Option<String>,
}

impl Confirmation {
    /// The kind of actor that confirmed.
    pub fn actor(&self) -> Actor {
        Actor::parse(&self.by)
    }

    /// Every well-formed entry of a document's `confirmed` list, in order. An
    /// entry missing `by` or `at`, or the list being something other than a
    /// list, contributes nothing: the field is read for what it can say and
    /// never refused, so a workspace that used the key for something else is
    /// left alone rather than reported.
    pub fn read_all(meta: &Value) -> Vec<Confirmation> {
        let Some(entries) = meta.get(CONFIRMED).and_then(Value::as_sequence) else {
            return Vec::new();
        };
        entries
            .iter()
            .filter_map(|entry| {
                let map = entry.as_mapping()?;
                Some(Confirmation {
                    by: map.get("by")?.as_str()?.to_string(),
                    at: map.get("at")?.as_str()?.to_string(),
                    of: map.get("of").and_then(Value::as_str).map(str::to_string),
                })
            })
            .collect()
    }

    /// [`read_all`](Self::read_all), with each entry's keys beyond `by`, `at`
    /// and `of` beside it, in the order written: the keys another tool added
    /// when the entry was made ([`Workspace::confirm_with`]). prov reads none
    /// of them.
    ///
    /// [`Workspace::confirm_with`]: crate::Workspace::confirm_with
    pub fn read_extended(meta: &Value) -> Vec<(Confirmation, crate::meta::Mapping)> {
        let Some(entries) = meta.get(CONFIRMED).and_then(Value::as_sequence) else {
            return Vec::new();
        };
        entries
            .iter()
            .filter_map(|entry| {
                let map = entry.as_mapping()?;
                let confirmation = Confirmation {
                    by: map.get("by")?.as_str()?.to_string(),
                    at: map.get("at")?.as_str()?.to_string(),
                    of: map.get("of").and_then(Value::as_str).map(str::to_string),
                };
                let rest = map
                    .iter()
                    .filter(|(key, _)| !matches!(key.as_str(), "by" | "at" | "of"))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                Some((confirmation, rest))
            })
            .collect()
    }

    /// Whether the document has changed since this confirmation was made,
    /// given the document's current `updated` instant and `content_hash`.
    ///
    /// Stale when the stamp is a later instant than `at`, or when the entry
    /// named a digest and the document no longer records that one. A document
    /// with no stamp has nothing saying it changed, and an entry that named no
    /// digest has nothing to compare.
    ///
    /// The two are compared as instants, not as text: prov writes both with
    /// and without fractional seconds (`…:00Z`, `…:00.000000Z`), and `Z` sorts
    /// after `.`, so a text comparison put a later edit first. A value that
    /// is not RFC 3339 — written by hand, or by another tool — falls back to
    /// the text comparison it always had.
    pub fn is_stale(&self, updated: Option<&str>, content_hash: Option<&str>) -> bool {
        self.is_stale_against(updated, content_hash, None)
    }

    /// [`is_stale`](Self::is_stale), in a workspace that binds confirmations
    /// to content: `content_digest` is the document's
    /// [content digest](content_digest) now, given when the workspace sets
    /// `confirmations: content` and `None` otherwise.
    ///
    /// On a document that records no `content_hash`, an entry naming an `of`
    /// is stale exactly when that is not `content_digest` — the stamp is not
    /// consulted, since the digest already covers it. Every other entry, and
    /// every entry on a document that records a `content_hash`, is judged by
    /// [`is_stale`](Self::is_stale)'s rule. With `content_digest` absent this
    /// *is* that rule.
    pub fn is_stale_against(
        &self,
        updated: Option<&str>,
        content_hash: Option<&str>,
        content_digest: Option<&str>,
    ) -> bool {
        if content_hash.is_none()
            && let (Some(of), Some(now)) = (&self.of, content_digest)
        {
            return of != now;
        }
        if let Some(updated) = updated
            && later(updated, &self.at)
        {
            return true;
        }
        match (&self.of, content_hash) {
            (Some(of), Some(hash)) => of != hash,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    /// This entry as frontmatter — the mapping `confirm` appends.
    pub fn to_value(&self) -> Value {
        let mut map = crate::meta::Mapping::new();
        map.insert("by".into(), Value::String(self.by.clone()));
        map.insert("at".into(), Value::String(self.at.clone()));
        if let Some(of) = &self.of {
            map.insert("of".into(), Value::String(of.clone()));
        }
        Value::Mapping(map)
    }
}

/// What a document's confirmations add up to right now — derived from the
/// list and the document's current stamp and digest, never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tier {
    /// No live confirmation.
    Unconfirmed,
    /// Live confirmations, every one by a prefixed (non-human) actor.
    MachineConfirmed,
    /// At least one live confirmation by a person.
    HumanConfirmed,
}

impl Tier {
    /// The stable lowercase spelling — for output that a script reads.
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Unconfirmed => "unconfirmed",
            Tier::MachineConfirmed => "machine-confirmed",
            Tier::HumanConfirmed => "human-confirmed",
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A document's confirmations, sorted into the ones that still stand and the
/// ones the document has moved out from under.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Confirmations {
    /// Entries the document has not changed since.
    pub live: Vec<Confirmation>,
    /// Entries made against a document that has since changed. Kept — they
    /// are history — and not counted.
    pub stale: Vec<Confirmation>,
}

impl Confirmations {
    /// Sort a document's list against its current `updated` instant and
    /// `content_hash`.
    pub fn read(meta: &Value, updated_field: Option<&str>) -> Confirmations {
        Self::read_against(meta, updated_field, None)
    }

    /// [`read`](Self::read), with the document's [content digest](content_digest)
    /// now, for a workspace that sets `confirmations: content` — see
    /// [`Confirmation::is_stale_against`] for the rule.
    pub fn read_against(
        meta: &Value,
        updated_field: Option<&str>,
        content_digest: Option<&str>,
    ) -> Confirmations {
        let updated = updated_field
            .and_then(|field| meta.get(field))
            .and_then(Value::as_str);
        let hash = meta.get("content_hash").and_then(Value::as_str);
        let mut out = Confirmations::default();
        for entry in Confirmation::read_all(meta) {
            if entry.is_stale_against(updated, hash, content_digest) {
                out.stale.push(entry);
            } else {
                out.live.push(entry);
            }
        }
        out
    }

    /// The tier the live entries support.
    pub fn tier(&self) -> Tier {
        if self.live.is_empty() {
            Tier::Unconfirmed
        } else if self.live.iter().any(|c| c.actor().is_person()) {
            Tier::HumanConfirmed
        } else {
            Tier::MachineConfirmed
        }
    }
}

/// The **content digest** of the document at `path` whose text is `text`: the
/// digest, in [`fixity`](crate::fixity)'s `sha256:<hex>` form, of the text as
/// it would be with the `confirmed` key removed from its metadata.
///
/// The key is removed with the same comment-preserving edit
/// [`Workspace::confirm`](crate::Workspace::confirm) appends with, so every
/// other byte — comments, key order, the body, the `updated` stamp — is what
/// the file says. A document whose metadata has no `confirmed` key digests as
/// its text, unchanged. So appending a confirmation never moves it, and any
/// other edit does.
///
/// One case is closed by hand: a fenced block left holding nothing once the
/// key is gone is left out too, because confirming a document that had no
/// metadata block is what put it there. (So the first confirmation of a
/// document whose block was already empty, `---` over `---`, is the one that
/// moves its content digest — once, and the entry still names the digest the
/// file has after it.)
///
/// `path` says how the text is read, as it does for
/// [`Document::parse`]: a `.yaml`, `.json` or `.toml` path is a document whose
/// whole file is metadata, and anything else carries a fenced block whose
/// format the text itself declares (`---` YAML, `+++` TOML, `;;;` JSON). It is
/// a pure function of the two, so an outside tool computes the content digest
/// of the bytes a document had at any past revision with the path it had
/// then.
pub fn content_digest(path: impl AsRef<Path>, text: &str) -> Result<String> {
    content_digest_of(text, &Document::parse(path.as_ref(), text)?)
}

/// [`content_digest`] over a text already parsed to `doc`.
pub(crate) fn content_digest_of(text: &str, doc: &Document) -> Result<String> {
    Ok(crate::fixity::digest(content_text(text, doc)?.as_bytes()))
}

/// The text the content digest is taken of: `text` without its `confirmed`
/// key, and without a fenced block that held nothing else — confirming a
/// document that had no metadata block is what created it.
fn content_text(text: &str, doc: &Document) -> Result<String> {
    let Some(carrier) = doc.carrier else {
        return Ok(text.to_string());
    };
    if doc.meta.get(CONFIRMED).is_none() {
        return Ok(text.to_string());
    }
    let stripped = prov_store::edit::unset_in_text(text, Some(carrier), CONFIRMED)?;
    if let prov_graph::document::MetaCarrier::Fenced(kind) = carrier {
        let found = fig::Embed::extract(&stripped, kind)?;
        let left = crate::meta::parse_value(found.content(), kind.inner_format())?;
        let empty = match &left {
            Value::Null => true,
            Value::Mapping(map) => map.is_empty(),
            _ => false,
        };
        if empty {
            return Ok(format!("{}{}", found.host_before(), found.host_after()));
        }
    }
    Ok(stripped)
}

/// Whether the edit stamp `a` is later than the confirmation instant `b`.
///
/// Both RFC 3339: compared as instants. A stamp that is a bare calendar date —
/// a `type: date` edit stamp — says only which day the edit fell on, so it is
/// later when it names a later day than the one `b` was written on, in `b`'s
/// own offset. An edit the same day as the confirmation cannot be ordered
/// against it, and does not make it stale: the day is all a date stamp
/// declares, and a document that must be ordered within one carries an
/// instant stamp or a `content_hash`. Anything else is compared as text.
fn later(a: &str, b: &str) -> bool {
    use crate::config::now::{calendar_days, instant_seconds};
    match (instant_seconds(a), instant_seconds(b)) {
        (Some(a), Some(b)) => a > b,
        (None, Some(_)) if a.trim().len() == 10 => match (calendar_days(a), calendar_days(b)) {
            (Some(a), Some(b)) => a > b,
            _ => a.trim() > b.trim(),
        },
        _ => a.trim() > b.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(yaml: &str) -> Value {
        Value::Mapping(crate::meta::parse_mapping(yaml, fig::Format::Yaml).unwrap())
    }

    #[test]
    fn stamps_compare_as_instants_whatever_their_spelling() {
        // Fractional seconds sort before `Z` as text; as instants the later
        // edit is later.
        assert!(later("2026-09-02T00:00:00.5Z", "2026-09-02T00:00:00Z"));
        assert!(!later(
            "2026-09-02T00:00:00Z",
            "2026-09-02T00:00:00.000000Z"
        ));
        // An offset is applied: 16:00+02:00 is 14:00Z.
        assert!(!later("2026-09-02T16:00:00+02:00", "2026-09-02T14:00:00Z"));
        assert!(later("2026-09-02T16:00:01+02:00", "2026-09-02T14:00:00Z"));
        assert_eq!(
            crate::config::now::instant_seconds("1970-01-01T00:00:00Z"),
            Some((0, 0))
        );
        assert_eq!(
            crate::config::now::instant_seconds("2000-03-01T00:00:00Z"),
            Some((951_868_800, 0))
        );
        // Not RFC 3339: the old text comparison, not a panic.
        assert_eq!(crate::config::now::instant_seconds("yesterday"), None);
        assert!(later("b", "a"));
    }

    #[test]
    fn a_bare_actor_is_a_person_and_a_prefix_says_otherwise() {
        assert!(Actor::parse("amh").is_person());
        assert!(Actor::parse("Adam Harris").is_person());
        assert_eq!(
            Actor::parse("agent:claude-opus-5"),
            Actor::Agent("claude-opus-5".into())
        );
        assert_eq!(Actor::parse("process:prov"), Actor::Process("prov".into()));
        // A mistyped prefix is a person — the trade the design accepts.
        assert!(Actor::parse("agnet:x").is_person());
        assert_eq!(Actor::parse("agent:x").to_string(), "agent:x");
    }

    #[test]
    fn the_list_is_read_for_what_it_can_say() {
        let m = meta(
            "confirmed:\n- by: amh\n  at: 2026-09-11T09:20:00.000000Z\n- by: nobody\n- not: a confirmation\n- 3\n",
        );
        let all = Confirmation::read_all(&m);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].by, "amh");
        assert!(all[0].of.is_none());
        // Something that is not a list is nothing, not an error.
        assert!(Confirmation::read_all(&meta("confirmed: yes\n")).is_empty());
        assert!(Confirmation::read_all(&meta("title: x\n")).is_empty());
    }

    #[test]
    fn a_confirmation_is_stale_once_the_stamp_is_newer() {
        let c = Confirmation {
            by: "amh".into(),
            at: "2026-09-11T09:20:00.000000Z".into(),
            of: None,
        };
        assert!(!c.is_stale(None, None));
        assert!(!c.is_stale(Some("2026-09-11T09:20:00.000000Z"), None));
        assert!(!c.is_stale(Some("2026-09-11T09:19:59.999999Z"), None));
        assert!(c.is_stale(Some("2026-09-11T09:20:00.000001Z"), None));
        // Bound to a digest as well: the digest moving is enough on its own.
        let bound = Confirmation {
            of: Some("sha256:aa".into()),
            ..c.clone()
        };
        assert!(!bound.is_stale(None, Some("sha256:aa")));
        assert!(bound.is_stale(None, Some("sha256:bb")));
        assert!(bound.is_stale(None, None), "the digest it named is gone");
    }

    /// Bound to content, an entry naming an `of` on a document with no
    /// `content_hash` is judged by that alone; anything else keeps the rule it
    /// had.
    #[test]
    fn bound_to_content_the_digest_alone_decides() {
        let c = Confirmation {
            by: "amh".into(),
            at: "2026-09-11T09:20:00.000000Z".into(),
            of: Some("sha256:aa".into()),
        };
        let later = Some("2026-09-12T00:00:00.000000Z");
        assert!(!c.is_stale_against(later, None, Some("sha256:aa")));
        assert!(c.is_stale_against(None, None, Some("sha256:bb")));
        // A recorded `content_hash` is what an `of` names on that document.
        assert!(c.is_stale_against(None, Some("sha256:bb"), Some("sha256:aa")));
        assert!(c.is_stale_against(later, Some("sha256:aa"), Some("sha256:aa")));
        // No `of`: the stamp, as before.
        let bare = Confirmation {
            of: None,
            ..c.clone()
        };
        assert!(bare.is_stale_against(later, None, Some("sha256:aa")));
        assert!(!bare.is_stale_against(None, None, Some("sha256:aa")));
        // Not bound: exactly `is_stale`.
        assert_eq!(c.is_stale_against(None, None, None), c.is_stale(None, None));
    }

    /// A `type: date` edit stamp orders by day: an edit on a later day than
    /// the confirmation makes it stale, one on the same day cannot be told
    /// apart from it and does not.
    #[test]
    fn a_date_stamp_is_later_only_on_a_later_day() {
        let c = Confirmation {
            by: "amh".into(),
            at: "2026-09-11T09:20:00.000000Z".into(),
            of: None,
        };
        assert!(!c.is_stale(Some("2026-09-10"), None));
        assert!(!c.is_stale(Some("2026-09-11"), None));
        assert!(c.is_stale(Some("2026-09-12"), None));
    }

    #[test]
    fn the_tier_is_derived_from_live_entries_only() {
        let m = meta(
            "updated: 2026-09-11T10:00:00.000000Z\n\
             confirmed:\n\
             - by: amh\n  at: 2026-09-11T09:00:00.000000Z\n\
             - by: agent:claude-opus-5\n  at: 2026-09-11T11:00:00.000000Z\n",
        );
        let c = Confirmations::read(&m, Some("updated"));
        assert_eq!(c.stale.len(), 1);
        assert_eq!(c.live.len(), 1);
        assert_eq!(c.tier(), Tier::MachineConfirmed);
        // With no stamp field configured, nothing says the document changed.
        let c = Confirmations::read(&m, None);
        assert_eq!(c.stale.len(), 0);
        assert_eq!(c.tier(), Tier::HumanConfirmed);
        assert_eq!(
            Confirmations::read(&meta("title: x\n"), Some("updated")).tier(),
            Tier::Unconfirmed
        );
    }

    #[test]
    fn generated_is_read_when_well_formed() {
        let m = meta("generated:\n  by: process:prov\n  at: 2026-09-11T09:00:00.000000Z\n");
        let g = Generated::read(&m).unwrap();
        assert_eq!(g.actor(), Actor::Process("prov".into()));
        assert_eq!(g.how, None);
        assert!(Generated::read(&meta("generated: prov\n")).is_none());
        // The act is a third key, optional, and read only as a string.
        let m = meta(
            "generated:\n  by: agent:claude-opus-5\n  at: 2026-09-11T09:00:00.000000Z\n  how: transcribed\n",
        );
        assert_eq!(
            Generated::read(&m).unwrap().how.as_deref(),
            Some("transcribed")
        );
        let m = meta("generated:\n  by: amh\n  at: 2026-09-11T09:00:00.000000Z\n  how: [a, b]\n");
        assert_eq!(Generated::read(&m).unwrap().how, None);
    }
}
