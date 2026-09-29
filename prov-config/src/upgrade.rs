//! Retired config, rewritten in the form this build reads.
//!
//! A config key prov stopped reading is a finding, and most such findings have
//! one right answer: the retired `views.<name>` entry has an exact replacement
//! ([`prov_views::translate`]), and a top-level `updated: modified` says
//! exactly what `fields.modified.stamp: edit` says. What makes these different
//! from a misspelled key is that the answer is not one key. A view that nested
//! becomes a view *and* a `filing:` entry; a stamp moves from the top level
//! into the field's own declaration, which may already exist and must keep
//! what it says. So the repair is a list of [`ConfigEdit`]s against one
//! surface, applied together.
//!
//! This module plans the list and writes nothing: the surface is a value, the
//! edits are values, and `prov` applies them to the document that carries the
//! surface as one change. Planning here rather than there is what lets
//! [`diagnose`](crate::diagnose) say, in the finding itself, when there is no
//! plan — the same function answers both questions, so the finding and its
//! repair cannot disagree.
//!
//! **When there is no plan.** A rewrite that would overwrite something the
//! author wrote is not planned: a `filing.<name>` entry that already says
//! something else, or a stamp another field already carries. Either is a
//! decision about the workspace, and the finding stands.

use prov_graph::meta::Value;

use crate::config::{ConfigIssue, ConfigIssueKind, Stamp};

/// One step of a config rewrite, addressed by key path from the surface's own
/// top — a config document's top level, or the inside of a root's `prov:`
/// block. A path is a list of keys rather than dotted text because a view's
/// name may itself hold a dot.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigEdit {
    /// Write `value` at `key`, creating any missing mapping along the way and
    /// replacing whatever was there whole.
    Set { key: Vec<String>, value: Value },
    /// Append `value` to the sequence at `key`.
    Append { key: Vec<String>, value: Value },
    /// Delete the entry at `key`.
    Remove { key: Vec<String> },
}

// Equality is structural, and every value a plan carries was read from a
// config surface or built from strings; a float in one compares as floats do,
// which can make two plans unequal but never makes one wrong. Implemented so a
// fix carrying edits can stay `Eq` like every other fix.
impl Eq for ConfigEdit {}

impl ConfigEdit {
    /// The key path this edit writes to.
    pub fn key(&self) -> &[String] {
        match self {
            ConfigEdit::Set { key, .. }
            | ConfigEdit::Append { key, .. }
            | ConfigEdit::Remove { key } => key,
        }
    }

    /// The same edit, addressed one block down: under `prefix` when the
    /// surface is a root's inline `prov:` block rather than a whole config
    /// document. `None` leaves it as it is.
    pub fn prefixed(mut self, prefix: Option<&str>) -> Self {
        if let Some(prefix) = prefix {
            let key = match &mut self {
                ConfigEdit::Set { key, .. }
                | ConfigEdit::Append { key, .. }
                | ConfigEdit::Remove { key } => key,
            };
            key.insert(0, prefix.to_string());
        }
        self
    }
}

/// The rewrite that brings a finding [`diagnose`](crate::diagnose) reported on
/// `surface` into the current form, or `None` when the finding is not one of
/// the retired forms, or when rewriting it would overwrite something else the
/// author wrote.
///
/// Answers for [`ConfigIssueKind::ViewRetired`] and
/// [`ConfigIssueKind::StampRetired`]; every other kind is `None`. The plan is
/// computed against `surface` as it is now, so a caller applying several
/// should plan each after the last has landed — two retired keys can name the
/// same field.
pub fn upgrade(surface: &Value, issue: &ConfigIssue) -> Option<Vec<ConfigEdit>> {
    match &issue.kind {
        ConfigIssueKind::ViewRetired { .. } => {
            let name = issue.key.strip_prefix("views.")?;
            upgrade_view(surface, name)
        }
        ConfigIssueKind::StampRetired { stamp, .. } => {
            upgrade_stamp(surface, Stamp::from_config_str(stamp)?)
        }
        _ => None,
    }
}

fn key(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_string()).collect()
}

/// `views.<name>` replaced by its translation, and the translation's filing
/// entry added beside it — unless `filing.<name>` already says something
/// else.
pub(crate) fn upgrade_view(surface: &Value, name: &str) -> Option<Vec<ConfigEdit>> {
    let old = surface.get("views")?.as_mapping()?.get(name)?;
    let translation = prov_views::translate(old)?;
    let mut edits = vec![ConfigEdit::Set {
        key: key(&["views", name]),
        value: Value::Mapping(translation.view),
    }];
    if let Some(filing) = translation.filing {
        let filing = Value::Mapping(filing);
        match surface.get("filing").map(|block| block.get(name)) {
            // A `filing:` that is not a mapping is its own finding; writing
            // into it would bury that.
            Some(_) if surface.get("filing").and_then(Value::as_mapping).is_none() => return None,
            Some(Some(existing)) if *existing == filing => {}
            Some(Some(_)) => return None,
            _ => edits.push(ConfigEdit::Set {
                key: key(&["filing", name]),
                value: filing,
            }),
        }
    }
    Some(edits)
}

/// Whether the retired view `name` nests, and `filing.<name>` already holds a
/// different entry — the one case [`upgrade_view`] declines that a finding
/// should name.
pub(crate) fn filing_taken(surface: &Value, name: &str) -> bool {
    let Some(filing) = surface
        .get("views")
        .and_then(Value::as_mapping)
        .and_then(|views| views.get(name))
        .and_then(prov_views::translate)
        .and_then(|t| t.filing)
    else {
        return false;
    };
    surface
        .get("filing")
        .and_then(|block| block.get(name))
        .is_some_and(|existing| *existing != Value::Mapping(filing))
}

/// The retired top-level key for `stamp`.
fn retired_key(stamp: Stamp) -> &'static str {
    match stamp {
        Stamp::Create => "created",
        Stamp::Edit => "updated",
    }
}

/// The field other than `field` whose unscoped declaration on `surface`
/// already carries `stamp`, if one does — the claimant that keeps a retired
/// key from being moved onto `field`.
pub(crate) fn stamp_claimant(surface: &Value, stamp: Stamp, field: &str) -> Option<String> {
    let fields = surface.get("fields")?.as_mapping()?;
    fields
        .iter()
        .filter(|(name, _)| name.as_str() != field)
        .find(|(_, spec)| {
            unscoped(spec).is_some_and(|decl| {
                decl.get("stamp").and_then(Value::as_str) == Some(stamp.as_config_str())
            })
        })
        .map(|(name, _)| name.clone())
}

/// A field's unscoped declaration: the entry itself, or the first item of a
/// scoped list with no `under:`.
fn unscoped(spec: &Value) -> Option<&Value> {
    match spec {
        Value::Sequence(items) => items.iter().find(|i| i.get("under").is_none()),
        other => Some(other),
    }
}

/// The top-level `updated:`/`created:` key removed, and the stamp declared on
/// the field it named, merged into whatever that field already declares.
fn upgrade_stamp(surface: &Value, stamp: Stamp) -> Option<Vec<ConfigEdit>> {
    let retired = retired_key(stamp);
    let field = surface.get(retired)?.as_str()?.trim();
    let remove = ConfigEdit::Remove {
        key: key(&[retired]),
    };
    // The spelling of "stamping off": nothing to declare.
    if field.is_empty() {
        return Some(vec![remove]);
    }
    if stamp_claimant(surface, stamp, field).is_some() {
        return None;
    }
    // Both retired keys naming one field would give it two stamps, and a
    // field has one.
    let other = match stamp {
        Stamp::Create => Stamp::Edit,
        Stamp::Edit => Stamp::Create,
    };
    if surface
        .get(retired_key(other))
        .and_then(Value::as_str)
        .is_some_and(|f| f.trim() == field)
    {
        return None;
    }
    let spelled = Value::String(stamp.as_config_str().to_string());
    let declare = match surface.get("fields") {
        None => Some(ConfigEdit::Set {
            key: key(&["fields", field, "stamp"]),
            value: spelled.clone(),
        }),
        Some(Value::Mapping(fields)) => match fields.get(field) {
            None => Some(ConfigEdit::Set {
                key: key(&["fields", field, "stamp"]),
                value: spelled.clone(),
            }),
            Some(Value::Mapping(decl)) => stamp_edit(decl.get("stamp"), stamp, || {
                key(&["fields", field, "stamp"])
            })?,
            // Scoped declarations: the stamp goes on the unscoped one, which
            // is added when there is none — a stamp under a scope is not read.
            Some(Value::Sequence(items)) => {
                match items.iter().position(|i| i.get("under").is_none()) {
                    Some(i) => {
                        let index = i.to_string();
                        stamp_edit(items[i].get("stamp"), stamp, || {
                            key(&["fields", field, &index, "stamp"])
                        })?
                    }
                    None => {
                        let mut decl = prov_graph::meta::Mapping::new();
                        decl.insert("stamp".into(), spelled.clone());
                        Some(ConfigEdit::Append {
                            key: key(&["fields", field]),
                            value: Value::Mapping(decl),
                        })
                    }
                }
            }
            // A declaration that is not a mapping is its own finding.
            Some(_) => return None,
        },
        Some(_) => return None,
    };
    Some(declare.into_iter().chain([remove]).collect())
}

/// The edit that puts `stamp` on a declaration whose `stamp:` is `current`:
/// none when it already says so, a set when it says nothing, and `None` — no
/// plan — when it says the other stamp or something unreadable.
fn stamp_edit(
    current: Option<&Value>,
    stamp: Stamp,
    at: impl FnOnce() -> Vec<String>,
) -> Option<Option<ConfigEdit>> {
    match current {
        None => Some(Some(ConfigEdit::Set {
            key: at(),
            value: Value::String(stamp.as_config_str().to_string()),
        })),
        Some(v) if v.as_str() == Some(stamp.as_config_str()) => Some(None),
        Some(_) => None,
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::config::diagnose;

    fn yaml(text: &str) -> Value {
        prov_graph::meta::parse_value(text, prov_graph::Format::Yaml).expect("yaml")
    }

    /// Apply a plan to a surface, as the editor would.
    fn applied(surface: &Value, edits: &[ConfigEdit]) -> Value {
        let mut out = surface.clone();
        for edit in edits {
            let (last, parents) = edit.key().split_last().unwrap();
            let mut at = &mut out;
            for part in parents {
                if at.get(part).is_none()
                    && let Value::Mapping(m) = at
                {
                    m.insert(part.clone(), Value::Mapping(Default::default()));
                }
                at = match at {
                    Value::Mapping(m) => m.get_mut(part).unwrap(),
                    Value::Sequence(s) => &mut s[part.parse::<usize>().unwrap()],
                    _ => panic!("no container at {part}"),
                };
            }
            match edit {
                ConfigEdit::Set { value, .. } => {
                    let Value::Mapping(m) = at else { panic!() };
                    m.insert(last.clone(), value.clone());
                }
                ConfigEdit::Append { value, .. } => {
                    let Value::Mapping(m) = at else { panic!() };
                    let Some(Value::Sequence(s)) = m.get_mut(last) else {
                        panic!()
                    };
                    s.push(value.clone());
                }
                ConfigEdit::Remove { .. } => {
                    let Value::Mapping(m) = at else { panic!() };
                    m.shift_remove(last);
                }
            }
        }
        out
    }

    fn only_issue(surface: &Value) -> ConfigIssue {
        let issues = diagnose(surface);
        assert_eq!(issues.len(), 1, "{issues:#?}");
        issues.into_iter().next().unwrap()
    }

    #[test]
    fn a_retired_view_becomes_a_view_and_a_filing_entry() {
        let surface = yaml(
            "views:\n  daily:\n    label: Daily\n    group: created\n    by: month\n    under: '[[Daily]]'\n    nest: month\n",
        );
        let issue = only_issue(&surface);
        let edits = upgrade(&surface, &issue).expect("a plan");
        let after = applied(&surface, &edits);
        assert!(diagnose(&after).is_empty(), "{:#?}", diagnose(&after));
        assert_eq!(
            after
                .get("filing")
                .and_then(|f| f.get("daily"))
                .and_then(|d| d.get("nest")),
            Some(&Value::String("month".into()))
        );
    }

    #[test]
    fn a_filing_entry_that_says_something_else_is_not_overwritten() {
        let surface = yaml(
            "views:\n  daily:\n    group: created\n    nest: year\nfiling:\n  daily:\n    field: date\n    nest: month\n",
        );
        let issue = only_issue(&surface);
        assert!(matches!(
            issue.kind,
            ConfigIssueKind::ViewRetired {
                filing_taken: true,
                ..
            }
        ));
        assert!(upgrade(&surface, &issue).is_none());
    }

    #[test]
    fn a_retired_stamp_moves_onto_its_field_and_keeps_its_type() {
        let surface = yaml("updated: modified\nfields:\n  modified:\n    type: datetime\n");
        let issue = only_issue(&surface);
        assert_eq!(issue.key, "updated");
        let after = applied(&surface, &upgrade(&surface, &issue).unwrap());
        assert_eq!(
            after,
            yaml("fields:\n  modified:\n    type: datetime\n    stamp: edit\n")
        );
    }

    #[test]
    fn a_retired_stamp_lands_on_the_unscoped_declaration() {
        let scoped = yaml(
            "created: made\nfields:\n  made:\n    - under: '[[Tasks]]'\n      default: x\n    - type: date\n",
        );
        let after = applied(&scoped, &upgrade(&scoped, &only_issue(&scoped)).unwrap());
        assert!(diagnose(&after).is_empty(), "{:#?}", diagnose(&after));
        assert_eq!(
            after.get("fields").and_then(|f| f.get("made")),
            Some(&yaml(
                "- under: '[[Tasks]]'\n  default: x\n- type: date\n  stamp: create\n"
            ))
        );

        let only_scoped =
            yaml("created: made\nfields:\n  made:\n    - under: '[[Tasks]]'\n      default: x\n");
        let after = applied(
            &only_scoped,
            &upgrade(&only_scoped, &only_issue(&only_scoped)).unwrap(),
        );
        assert!(diagnose(&after).is_empty(), "{:#?}", diagnose(&after));
    }

    #[test]
    fn an_empty_stamp_is_removed_and_a_claimed_one_stands() {
        let off = yaml("updated: ''\n");
        let after = applied(&off, &upgrade(&off, &only_issue(&off)).unwrap());
        assert_eq!(after, yaml("{}\n"));

        let claimed = yaml("updated: modified\nfields:\n  lastmod:\n    stamp: edit\n");
        let issue = only_issue(&claimed);
        assert!(matches!(
            &issue.kind,
            ConfigIssueKind::StampRetired { claimed_by: Some(by), .. } if by == "lastmod"
        ));
        assert!(upgrade(&claimed, &issue).is_none());

        let twice = yaml("updated: when\ncreated: when\n");
        for issue in diagnose(&twice) {
            assert!(upgrade(&twice, &issue).is_none(), "{issue:?}");
        }
    }
}
