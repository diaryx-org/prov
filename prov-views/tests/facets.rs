//! The acceptance test the view design named: diaryx's five hardcoded lenses —
//! `FacetId { date, people, places, tags, audience }`, a Swift enum with
//! hardcoded frontmatter keys — must express as declarations with nothing left
//! over. If they do not, the format is wrong, and no amount of it being nicer
//! to read makes up for a lens it cannot say.
//!
//! Four of the five were always the easy case: a facet whose groups *are* one
//! field's values. `date` is the one that mattered, because it was never a
//! field — it was a rule, reading a chain of three field names the app knew and
//! the vault had never agreed to. It expresses here as the same shape as the
//! other four, which is the whole result.
//!
//! What is *not* here is as much the point. Swift's `Facet` also carries
//! `emptyLabel` ("Undated", "Untagged") and `isControlled`. Neither is missing
//! from the format: `emptyLabel` is derived in the frontend and never declared,
//! and whether a field is controlled is `fields.<name>.values`, which prov has
//! carried since before views existed. A view has no business restating it.

use prov_graph::meta::{Mapping, Value};
use prov_views::{Grain, Grouping, ViewSpec, views_from};

/// The five lenses, as a workspace would now declare them.
const DECLARED: &str = "\
daily:
  label: Daily
  icon: calendar
  group: [date_of_document, created, updated]
  by: year
  under: '[Daily](id:abc1234)'
  nest: year
people:
  label: People
  icon: person.2
  group: people
places:
  label: Places
  icon: mappin.and.ellipse
  group: places
tags:
  label: Tags
  icon: tag
  group: tags
audience:
  label: Audience
  icon: eye
  group: audience
";

fn parse_block(yaml: &str) -> Value {
    let mut config = Mapping::new();
    config.insert(
        "views".into(),
        prov_graph::meta::parse_value(yaml, prov_graph::Format::Yaml).expect("the fixture parses"),
    );
    Value::Mapping(config)
}

#[test]
fn all_five_diaryx_facets_express_as_declared_views() {
    let config = parse_block(DECLARED);
    let views = views_from(config.as_mapping().unwrap());

    assert_eq!(
        views.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
        ["daily", "people", "places", "tags", "audience"],
        "every facet expresses, in declaration order"
    );

    // The one that was a *rule* rather than a field: the chain the app used to
    // hold, now written down by the workspace, and cut by the grain the app
    // used to hold separately.
    let daily = &views[0];
    assert_eq!(
        daily.group,
        Grouping {
            keys: vec![
                "date_of_document".into(),
                "created".into(),
                "updated".into()
            ],
            by: Some(Grain::Year),
        }
    );
    assert_eq!(daily.under.as_deref(), Some("[Daily](id:abc1234)"));
    assert_eq!(daily.nest, Some(Grain::Year));

    // The four that always were fields, and stay one line each.
    for (view, key) in views[1..]
        .iter()
        .zip(["people", "places", "tags", "audience"])
    {
        assert_eq!(view.group, Grouping::field(key), "{key}");
        assert_eq!(view.group.by, None, "{key} does not cut");
        assert_eq!(view.under, None, "{key} is unscoped, as the app's was");
    }
}

/// Nothing left over, checked rather than claimed: every declaration
/// round-trips, so no key of the fixture was quietly dropped on the way in.
#[test]
fn the_five_survive_a_round_trip_with_nothing_dropped() {
    let config = parse_block(DECLARED);
    let views = views_from(config.as_mapping().unwrap());

    for view in &views {
        let back = ViewSpec::parse(&view.name, &Value::Mapping(view.to_mapping()))
            .expect("a declared view re-reads as one");
        assert_eq!(&back, view, "{} lost something", view.name);
    }
}

/// The old spelling is gone, not aliased. `group: date` is now a view grouped
/// on a field *named* `date` — which is a real thing a workspace may declare,
/// and no longer a token meaning "the chain this program knows".
#[test]
fn the_retired_date_token_is_now_an_ordinary_field_name() {
    let config = parse_block("legacy:\n  group: date\n");
    let views = views_from(config.as_mapping().unwrap());
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].group, Grouping::field("date"));

    let mut doc = Mapping::new();
    doc.insert(
        "date_of_document".into(),
        Value::String("2026-07-24".into()),
    );
    assert!(
        views[0].group.keys_of(&Value::Mapping(doc)).is_empty(),
        "it reads the field it names, and no chain behind it"
    );
}
