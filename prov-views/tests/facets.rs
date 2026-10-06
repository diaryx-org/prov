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

use prov_filing::{FilingSpec, Grain, Nest, filing_from};
use prov_graph::meta::{Mapping, Value};
use prov_views::{Evaluator, Expression, KeyShape, Row, ViewSpec, translate, views_from};

/// The five lenses, as a workspace now declares them.
const DECLARED: &str = "\
views:
  daily:
    label: Daily
    icon: calendar
    where: under('[Daily](id:abc1234)')
    key: year(first(date_of_document, created, updated))
  people:
    label: People
    icon: person.2
    key: people
  places:
    label: Places
    icon: mappin.and.ellipse
    key: places
  tags:
    label: Tags
    icon: tag
    key: tags
  audience:
    label: Audience
    icon: eye
    key: audience
filing:
  daily:
    under: '[Daily](id:abc1234)'
    field: [date_of_document, created, updated]
    nest: year
";

/// The same five, as they were declared before views were queries.
const RETIRED: &str = "\
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

fn parse(yaml: &str) -> Mapping {
    prov_graph::meta::parse_mapping(yaml, prov_graph::Format::Yaml).expect("the fixture parses")
}

fn field(name: &str) -> KeyShape {
    KeyShape::Field(name.into())
}

#[test]
fn all_five_diaryx_facets_express_as_declared_views() {
    let config = parse(DECLARED);
    let views = views_from(&config);

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
        daily.key.key_shape(),
        KeyShape::Cut(
            Grain::Year,
            Box::new(KeyShape::First(vec![
                field("date_of_document"),
                field("created"),
                field("updated"),
            ]))
        )
    );
    // Where it files is a declaration of its own.
    let filing = filing_from(&config);
    assert_eq!(filing[0].under.as_deref(), Some("[Daily](id:abc1234)"));
    assert_eq!(filing[0].nest, Some(Nest::Grain(Grain::Year)));

    // The four that always were fields, and stay one line each.
    for (view, key) in views[1..]
        .iter()
        .zip(["people", "places", "tags", "audience"])
    {
        assert_eq!(view.key.key_shape(), field(key), "{key}");
        assert_eq!(
            view.filter, None,
            "{key} covers everything, as the app's did"
        );
    }
}

/// Nothing left over, checked rather than claimed: every declaration
/// round-trips, so no key of the fixture was quietly dropped on the way in.
#[test]
fn the_five_survive_a_round_trip_with_nothing_dropped() {
    let config = parse(DECLARED);
    for view in views_from(&config) {
        let back = ViewSpec::parse(&view.name, &Value::Mapping(view.to_mapping()))
            .expect("a declared view re-reads as one");
        assert_eq!(back, view, "{} lost something", view.name);
    }
    for entry in filing_from(&config) {
        let back = FilingSpec::parse(&entry.name, &Value::Mapping(entry.to_mapping()));
        assert_eq!(back, Some(entry));
    }
}

/// The retired declarations translate to the current ones, key for key.
#[test]
fn the_retired_declarations_translate_to_these() {
    let retired = parse(RETIRED);
    let current = parse(DECLARED);
    let views = current.get("views").and_then(Value::as_mapping).unwrap();
    for (name, old) in &retired {
        let translation = translate(old).expect("a retired view translates");
        let view = ViewSpec::parse(name, &Value::Mapping(translation.view)).expect("parses");
        let expected = ViewSpec::parse(name, views.get(name).unwrap()).unwrap();
        assert_eq!(view, expected, "{name}");
        if let Some(filing) = translation.filing {
            assert_eq!(
                FilingSpec::parse(name, &Value::Mapping(filing)),
                filing_from(&current).into_iter().find(|f| &f.name == name),
                "{name}"
            );
        }
    }
}

/// A key names a field and nothing behind it: `date` is a field *named*
/// `date`, which is a real thing a workspace may declare, and not a token
/// meaning "the chain this program knows".
#[test]
fn a_field_named_date_is_an_ordinary_field() {
    let mut meta = Mapping::new();
    meta.insert(
        "date_of_document".into(),
        Value::String("2026-07-24".into()),
    );
    let row = Row {
        path: "a.md".into(),
        id: None,
        ancestors: Vec::new(),
        meta: Value::Mapping(meta),
        references: Vec::new(),
    };
    let keys = Evaluator::new()
        .keys(&Expression::parse("date").unwrap(), &row)
        .unwrap();
    assert!(
        keys.is_empty(),
        "it reads the field it names, and no chain behind it"
    );
}
