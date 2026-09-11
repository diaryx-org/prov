//! Provenance — how a document came to exist, and who has confirmed it since.
//!
//! Two frontmatter families, read here and written by exactly one verb each:
//!
//! - **`generated: {by, at}`** — how the document came to exist. Written once,
//!   by whatever created it, and never maintained; the pair is a fact about an
//!   event. prov reads it only to say what kind of actor wrote the document.
//! - **`confirmed: [{by, at, of?}]`** — an append-only list of dated,
//!   attributed statements that someone read the document and found it
//!   correct. [`Workspace::confirm`](crate::Workspace::confirm) appends one;
//!   nothing else writes the list, and nothing ever rewrites or drops an entry.
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

use crate::meta::Value;

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

/// How a document came to exist — the `generated` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated {
    /// Who or what created it.
    pub by: String,
    /// When, RFC 3339 UTC.
    pub at: String,
}

impl Generated {
    /// The `generated` pair a document records, if it records a well-formed
    /// one — a mapping with string `by` and `at`.
    pub fn read(meta: &Value) -> Option<Generated> {
        let map = meta.get(GENERATED)?.as_mapping()?;
        Some(Generated {
            by: map.get("by")?.as_str()?.to_string(),
            at: map.get("at")?.as_str()?.to_string(),
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
    /// When, RFC 3339 UTC, fixed width — the same clock and spelling as the
    /// workspace's `updated` stamp, so the two compare as strings.
    pub at: String,
    /// The `content_hash` the document recorded at the moment of confirming,
    /// for a document that records one. Absent on a combined document, which
    /// has no digest to name.
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

    /// Whether the document has changed since this confirmation was made,
    /// given the document's current `updated` instant and `content_hash`.
    ///
    /// Stale when the stamp is newer than `at` — a plain string comparison,
    /// sound because both are fixed-width RFC 3339 UTC — or when the entry
    /// named a digest and the document no longer records that one. A document
    /// with no stamp has nothing saying it changed, and an entry that named no
    /// digest has nothing to compare.
    pub fn is_stale(&self, updated: Option<&str>, content_hash: Option<&str>) -> bool {
        if let Some(updated) = updated
            && updated > self.at.as_str()
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
        let updated = updated_field
            .and_then(|field| meta.get(field))
            .and_then(Value::as_str);
        let hash = meta.get("content_hash").and_then(Value::as_str);
        let mut out = Confirmations::default();
        for entry in Confirmation::read_all(meta) {
            if entry.is_stale(updated, hash) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(yaml: &str) -> Value {
        Value::Mapping(crate::meta::parse_mapping(yaml, fig::Format::Yaml).unwrap())
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
        assert!(Generated::read(&meta("generated: prov\n")).is_none());
    }
}
