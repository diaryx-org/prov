//! The expressions a view is written in: [CEL], with prov's own functions.
//!
//! A view's `where:` and `key:` are CEL expressions. CEL is not prov's: its
//! grammar, its operators and its standard functions are fixed by [its
//! specification][CEL] and implemented by the `cel` crate. What prov adds is
//! the environment an expression sees — each document's fields, and the
//! document itself as `doc` — and a small closed set of functions that carry
//! decisions prov has already made.
//!
//! # Why CEL
//!
//! A view is a way of *reading*, and reading has no invariant: a wrong view
//! shows the wrong rows and you edit the file. So nothing here needs the static
//! guarantees filing does (those are `prov-filing`'s), and a view can be as
//! expressive as a query language — provided the language cannot write and
//! always finishes. CEL is designed for exactly that: it is deliberately not
//! Turing complete, an expression has no side effects, and it is what
//! Kubernetes and Envoy embed for expressions inside configuration. The
//! reasoning is in the proposal `views-as-queries`.
//!
//! # The environment
//!
//! - Each **metadata field** is a variable of its own name: `status`,
//!   `created`, `people`. A field the document does not declare is `null`, so
//!   `status == 'done'` is false on a document without one rather than an
//!   error — and, the one place prov departs from plain CEL, `null` on the
//!   right of `in` or as the list `exists`/`all`/`map`/`filter` walk is an
//!   empty list, so `'Ada' in people` is false on a document with no `people`
//!   rather than an error on every such document.
//! - **`doc`** is the document: `doc.path`, `doc.title`, `doc.id`, `doc.meta`
//!   (the whole block, for a key that is not an identifier — `doc.meta['date
//!   of birth']`), and `doc.ancestors`, every document above this one in the
//!   spine from the root down, each `{path, title, id}`. `doc` wins over a
//!   field of that name, which is still `doc.meta.doc`.
//!
//! # prov's functions
//!
//! | function | gives |
//! |---|---|
//! | `present(x)` | whether `x` carries a non-empty value |
//! | `first(a, b, …)` | the first argument that is `present` |
//! | `field('a.b')` | every value at a field path, as text — `written.on`, `confirmed[].by` |
//! | `year(x)`, `month(x)`, `day(x)` | the keys a date cuts to, read as EDTF |
//! | `initial(x)`, `initial(x, n)` | the first letter(s), upper-cased |
//!
//! Each grain takes a value or a list and returns a list, because a value can
//! cut to no key (`banana`, `XXXX`), one, or several (`1918/1922` at year
//! grain). The rules are [`Grain::cuts`]'.
//!
//! A function is added here by the rule prov's grains and predicates were
//! always added by: a concrete way of reading the workspace that cannot
//! otherwise be said. CEL's own functions are CEL's.
//!
//! [CEL]: https://github.com/google/cel-spec

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use cel::common::ast::{Expr, LiteralValue};
use cel::context::VariableResolver;
use cel::extractors::Arguments;
use cel::objects::{Key, Map};
use cel::{Context, ExecutionError, FunctionContext, IdedExpr, Program, ResolveResult};
use prov_graph::field::{FieldPath, values_at};
use prov_graph::meta::{Mapping, Value};

use crate::select::Row;
use prov_grain::{Grain, scalar_texts};

/// prov's functions — the only names an expression may call beyond CEL's own.
pub const FUNCTIONS: &[&str] = &[
    "present", "first", "field", "year", "month", "day", "initial",
];

/// CEL's standard functions, as the `cel` crate implements them.
///
/// The crate has no static checker, so it cannot say before running whether a
/// name exists — a misspelled `dya(created)` would otherwise surface as an
/// error on every document instead of once, in the config. The macros (`has`,
/// `all`, `exists`, `exists_one`, `map`, `filter`) are expanded by the parser
/// and never appear as calls.
const CEL_FUNCTIONS: &[&str] = &[
    "bool",
    "bytes",
    "contains",
    "double",
    "duration",
    "dyn",
    "endsWith",
    "getDate",
    "getDayOfMonth",
    "getDayOfWeek",
    "getDayOfYear",
    "getFullYear",
    "getHours",
    "getMilliseconds",
    "getMinutes",
    "getMonth",
    "getSeconds",
    "hasValue",
    "int",
    "matches",
    "or",
    "orValue",
    "size",
    "startsWith",
    "string",
    "timestamp",
    "type",
    "uint",
    "value",
];

/// CEL's type names. A field the document does not carry reads as `null`, but
/// these are left to CEL when absent, so `type(x) == string` still means the
/// type.
const TYPE_NAMES: &[&str] = &[
    "bool",
    "bytes",
    "double",
    "int",
    "list",
    "map",
    "null_type",
    "string",
    "type",
    "uint",
];

/// A parsed CEL expression, with the source it was written as.
///
/// Two expressions are equal when their source is: the compiled program is a
/// cache of the text, never a second fact about it.
#[derive(Clone)]
pub struct Expression {
    source: String,
    program: Arc<Program>,
    /// The tree that runs: the program's own, with every container `in` and a
    /// comprehension read through [`CONTAINER`]. See [`absent_is_empty`].
    tree: Arc<IdedExpr>,
}

impl Expression {
    /// Parse `source`, and refuse a call to a function neither CEL nor prov
    /// defines — the one mistake an interpreter without a type checker would
    /// otherwise only report per document.
    pub fn parse(source: &str) -> Result<Self, ExpressionError> {
        let source = source.trim();
        if source.is_empty() {
            return Err(ExpressionError::Empty);
        }
        let program = Program::compile(source)
            .map_err(|errors| ExpressionError::Syntax(first_line(&errors.to_string())))?;
        let references = program.references();
        let mut unknown: Vec<&str> = references
            .functions()
            .into_iter()
            .filter(|name| is_named_function(name) && !is_known_function(name))
            .collect();
        unknown.sort_unstable();
        if let Some(name) = unknown.first() {
            return Err(ExpressionError::UnknownFunction((*name).to_string()));
        }
        let mut tree = program.expression().clone();
        absent_is_empty(&mut tree);
        Ok(Expression {
            source: source.to_string(),
            program: Arc::new(program),
            tree: Arc::new(tree),
        })
    }

    /// The expression as written, trimmed.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The fields the expression reads by name — every free variable but
    /// `doc`. Comprehension variables (`p` in `people.map(p, …)`) are
    /// included, since the parser does not tell them apart; a caller using
    /// this to find the fields a view depends on reads it as *at most*.
    pub fn fields(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .program
            .references()
            .variables()
            .into_iter()
            .filter(|name| *name != "doc")
            .map(str::to_string)
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// What this expression does, when it is one of the shapes a person could
    /// be told in words — a field, a chain, a grain over either. See
    /// [`KeyShape`].
    pub fn key_shape(&self) -> KeyShape {
        KeyShape::of(self.program.expression())
    }
}

impl fmt::Debug for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Expression").field(&self.source).finish()
    }
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

impl PartialEq for Expression {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for Expression {}

/// Why an expression was refused before it ever ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpressionError {
    /// Nothing was written.
    Empty,
    /// CEL could not parse it; the message is the parser's, first line.
    Syntax(String),
    /// It calls a function that neither CEL nor prov defines.
    UnknownFunction(String),
}

impl fmt::Display for ExpressionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExpressionError::Empty => f.write_str("the expression is empty"),
            ExpressionError::Syntax(message) => write!(f, "{message}"),
            ExpressionError::UnknownFunction(name) => write!(
                f,
                "there is no function `{name}` — prov adds {} to CEL's own",
                FUNCTIONS
                    .iter()
                    .map(|f| format!("`{f}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl std::error::Error for ExpressionError {}

/// A key expression in a shape that can be said in words.
///
/// The about page describes each view to a reader who will never see its
/// config, and a frontend deciding whether a view reads chronologically wants
/// to know whether it cuts by a date grain. Both need the *shape* of the
/// common cases, not an evaluator, and [`Other`](Self::Other) is the honest
/// answer for everything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyShape {
    /// A field, by name or by `field('a.b')`.
    Field(String),
    /// `first(a, b, …)`.
    First(Vec<KeyShape>),
    /// A grain over a shape — `month(first(date_of_document, created))`.
    Cut(Grain, Box<KeyShape>),
    /// Anything else.
    Other,
}

impl KeyShape {
    fn of(expr: &IdedExpr) -> Self {
        match &expr.expr {
            Expr::Ident(name) if name != "doc" => KeyShape::Field(name.clone()),
            Expr::Call(call) if call.target.is_none() => {
                let args = &call.args;
                match (call.func_name.as_str(), args.as_slice()) {
                    ("field", [path]) => match &path.expr {
                        Expr::Literal(LiteralValue::String(path)) => {
                            KeyShape::Field(path.inner().to_string())
                        }
                        _ => KeyShape::Other,
                    },
                    ("first", args) if !args.is_empty() => {
                        KeyShape::First(args.iter().map(KeyShape::of).collect())
                    }
                    ("year", [inner]) => KeyShape::cut(Grain::Year, inner),
                    ("month", [inner]) => KeyShape::cut(Grain::Month, inner),
                    ("day", [inner]) => KeyShape::cut(Grain::Day, inner),
                    ("initial", [inner]) => KeyShape::cut(Grain::Initial(1), inner),
                    ("initial", [inner, n]) => match &n.expr {
                        Expr::Literal(LiteralValue::Int(n)) if *n.inner() > 0 => {
                            KeyShape::cut(Grain::Initial(*n.inner() as usize), inner)
                        }
                        _ => KeyShape::Other,
                    },
                    _ => KeyShape::Other,
                }
            }
            _ => KeyShape::Other,
        }
    }

    fn cut(grain: Grain, inner: &IdedExpr) -> Self {
        match KeyShape::of(inner) {
            KeyShape::Other => KeyShape::Other,
            inner => KeyShape::Cut(grain, Box::new(inner)),
        }
    }
}

/// Evaluates expressions against rows.
///
/// Built once per run: CEL's standard library and prov's functions are
/// registered here, and each row is evaluated in a scope of its own on top.
pub struct Evaluator {
    root: Context<'static>,
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl Evaluator {
    /// An evaluator with CEL's standard library and prov's functions.
    pub fn new() -> Self {
        let mut root = Context::default();
        root.add_function("present", present);
        root.add_function("first", first);
        root.add_function("field", field);
        root.add_function("year", |value: cel::Value| cut(Grain::Year, &value));
        root.add_function("month", |value: cel::Value| cut(Grain::Month, &value));
        root.add_function("day", |value: cel::Value| cut(Grain::Day, &value));
        root.add_function("initial", initial);
        root.add_function(CONTAINER, |value: cel::Value| -> ResolveResult {
            Ok(match value {
                cel::Value::Null => cel::Value::List(Arc::new(Vec::new())),
                other => other,
            })
        });
        Evaluator { root }
    }

    /// What `expr` comes to for `row`, or the reason it could not be
    /// evaluated, in a sentence.
    pub fn value(&self, expr: &Expression, row: &Row) -> Result<cel::Value, String> {
        let scope = RowScope {
            row,
            doc: doc_value(row),
        };
        let mut ctx = self.root.new_inner_scope();
        ctx.set_variable_resolver(&scope);
        ctx.resolve(&expr.tree).map_err(|e| explain(&e))
    }

    /// Whether `row` meets `expr` — which must come out `true` or `false`.
    pub fn test(&self, expr: &Expression, row: &Row) -> Result<bool, String> {
        match self.value(expr, row)? {
            cel::Value::Bool(b) => Ok(b),
            other => Err(format!(
                "a condition must come out true or false, and this came out {}",
                describe(&other)
            )),
        }
    }

    /// The group keys `expr` gives `row`: a value's text, each item of a
    /// list (flattened, so `[day(created), day(updated)]` is one list of
    /// days), and nothing for `null` or an empty string. Deduplicated, in the
    /// order they came.
    pub fn keys(&self, expr: &Expression, row: &Row) -> Result<Vec<String>, String> {
        let mut keys = Vec::new();
        collect_keys(&self.value(expr, row)?, &mut keys)?;
        let mut seen = std::collections::HashSet::new();
        keys.retain(|k| seen.insert(k.clone()));
        Ok(keys)
    }
}

fn collect_keys(value: &cel::Value, out: &mut Vec<String>) -> Result<(), String> {
    match value {
        cel::Value::List(items) => {
            for item in items.iter() {
                collect_keys(item, out)?;
            }
            Ok(())
        }
        cel::Value::Null => Ok(()),
        other => match cel_text(other) {
            Some(text) => {
                if !text.is_empty() {
                    out.push(text);
                }
                Ok(())
            }
            None => Err(format!(
                "a key must be text, a number, or a list of them, and this came out {}",
                describe(other)
            )),
        },
    }
}

/// The variables a row's expressions see. See the module docs.
struct RowScope<'r> {
    row: &'r Row,
    doc: cel::Value,
}

impl VariableResolver for RowScope<'_> {
    fn resolve(&self, name: &str) -> Option<cel::Value> {
        if name == "doc" {
            return Some(self.doc.clone());
        }
        match self.row.meta.get(name) {
            Some(value) => Some(to_cel(value)),
            None if TYPE_NAMES.contains(&name) => None,
            None => Some(cel::Value::Null),
        }
    }
}

/// The `doc` variable for a row.
fn doc_value(row: &Row) -> cel::Value {
    let text = |s: &str| cel::Value::String(Arc::new(s.to_string()));
    let opt = |s: Option<&str>| s.map_or(cel::Value::Null, text);
    let ancestors: Vec<cel::Value> = row
        .ancestors
        .iter()
        .map(|a| {
            map_value(vec![
                ("path", text(&a.path.to_string_lossy())),
                ("title", opt(a.title.as_deref())),
                ("id", opt(a.id.as_deref())),
            ])
        })
        .collect();
    map_value(vec![
        ("path", text(&row.path.to_string_lossy())),
        ("title", opt(row.title())),
        ("id", opt(row.id.as_deref())),
        ("meta", to_cel(&row.meta)),
        ("ancestors", cel::Value::List(Arc::new(ancestors))),
    ])
}

fn map_value(entries: Vec<(&str, cel::Value)>) -> cel::Value {
    let map: HashMap<Key, cel::Value> = entries
        .into_iter()
        .map(|(k, v)| (Key::String(Arc::new(k.to_string())), v))
        .collect();
    cel::Value::Map(Map { map: Arc::new(map) })
}

/// A metadata value as CEL sees it.
fn to_cel(value: &Value) -> cel::Value {
    match value {
        Value::Null => cel::Value::Null,
        Value::Bool(b) => cel::Value::Bool(*b),
        Value::Int(i) => cel::Value::Int(*i),
        Value::Float(f) => cel::Value::Float(*f),
        Value::String(s) => cel::Value::String(Arc::new(s.clone())),
        Value::Sequence(items) => cel::Value::List(Arc::new(items.iter().map(to_cel).collect())),
        Value::Mapping(map) => {
            let map: HashMap<Key, cel::Value> = map
                .iter()
                .map(|(k, v)| (Key::String(Arc::new(k.clone())), to_cel(v)))
                .collect();
            cel::Value::Map(Map { map: Arc::new(map) })
        }
    }
}

/// A CEL value back as metadata — for `field()`, which walks a path the way a
/// `fields` declaration does and so needs prov's own navigation.
fn from_cel(value: &cel::Value) -> Value {
    match value {
        cel::Value::Null => Value::Null,
        cel::Value::Bool(b) => Value::Bool(*b),
        cel::Value::Int(i) => Value::Int(*i),
        cel::Value::UInt(u) => i64::try_from(*u).map_or(Value::Float(*u as f64), Value::Int),
        cel::Value::Float(f) => Value::Float(*f),
        cel::Value::String(s) => Value::String(s.to_string()),
        cel::Value::List(items) => Value::Sequence(items.iter().map(from_cel).collect()),
        cel::Value::Map(map) => {
            let mut out = Mapping::new();
            let mut entries: Vec<(String, Value)> = map
                .map
                .iter()
                .filter_map(|(k, v)| match k {
                    Key::String(k) => Some((k.to_string(), from_cel(v))),
                    _ => None,
                })
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, v) in entries {
                out.insert(k, v);
            }
            Value::Mapping(out)
        }
        _ => Value::Null,
    }
}

/// The text a scalar says, or `None` for a value that has no single text.
fn cel_text(value: &cel::Value) -> Option<String> {
    Some(match value {
        cel::Value::String(s) => s.trim().to_string(),
        cel::Value::Int(i) => i.to_string(),
        cel::Value::UInt(u) => u.to_string(),
        cel::Value::Float(f) => f.to_string(),
        cel::Value::Bool(b) => b.to_string(),
        _ => return None,
    })
}

/// Every non-empty scalar text in a value or a list of them.
fn texts(value: &cel::Value) -> Vec<String> {
    match value {
        cel::Value::List(items) => items.iter().flat_map(texts).collect(),
        other => cel_text(other)
            .filter(|t| !t.is_empty())
            .into_iter()
            .collect(),
    }
}

fn is_present(value: &cel::Value) -> bool {
    match value {
        cel::Value::Null => false,
        cel::Value::String(s) => !s.trim().is_empty(),
        cel::Value::List(items) => items.iter().any(is_present),
        cel::Value::Map(map) => !map.map.is_empty(),
        _ => true,
    }
}

fn strings(keys: Vec<String>) -> cel::Value {
    cel::Value::List(Arc::new(
        keys.into_iter()
            .map(|k| cel::Value::String(Arc::new(k)))
            .collect(),
    ))
}

/// The function a container position is read through — not a name anyone
/// writes, which is why it cannot be spelled as an identifier.
const CONTAINER: &str = "@prov_container";

/// Rewrite `tree` so an absent field in a container position is an empty list.
///
/// A document without `people` reads `people` as `null`, and CEL has no
/// meaning for `'Ada' in null` or `null.exists(…)`: it fails, on every such
/// document, which makes the commonest question about a list field — does it
/// name this? — report a failure per document that never had one. prov reads
/// `null` there as `[]`, which is what "this document lists nobody" means.
/// Only those two positions: `size(null)` and `null.startsWith(…)` still fail,
/// and are reported.
fn absent_is_empty(tree: &mut IdedExpr) {
    let wrap = |inner: &mut IdedExpr| {
        let taken = std::mem::take(inner);
        *inner = IdedExpr {
            id: taken.id,
            expr: Expr::Call(cel::common::ast::CallExpr {
                func_name: CONTAINER.to_string(),
                target: None,
                args: vec![taken],
            }),
        };
    };
    match &mut tree.expr {
        Expr::Call(call) => {
            if let Some(target) = call.target.as_deref_mut() {
                absent_is_empty(target);
            }
            for arg in &mut call.args {
                absent_is_empty(arg);
            }
            if call.func_name == cel::common::ast::operators::IN
                && let [_, container] = call.args.as_mut_slice()
            {
                wrap(container);
            }
        }
        Expr::Comprehension(comp) => {
            for part in [
                &mut comp.iter_range,
                &mut comp.accu_init,
                &mut comp.loop_cond,
                &mut comp.loop_step,
                &mut comp.result,
            ] {
                absent_is_empty(part);
            }
            wrap(&mut comp.iter_range);
        }
        Expr::List(list) => list.elements.iter_mut().for_each(absent_is_empty),
        Expr::Map(cel::common::ast::MapExpr { entries })
        | Expr::Struct(cel::common::ast::StructExpr { entries, .. }) => {
            for entry in entries {
                match &mut entry.expr {
                    cel::common::ast::EntryExpr::MapEntry(e) => {
                        absent_is_empty(&mut e.key);
                        absent_is_empty(&mut e.value);
                    }
                    cel::common::ast::EntryExpr::StructField(f) => absent_is_empty(&mut f.value),
                }
            }
        }
        Expr::Select(select) => absent_is_empty(&mut select.operand),
        _ => {}
    }
}

// ── prov's functions ────────────────────────────────────────────────────────

/// `present(x)`: `x` carries a value — not `null`, not blank, not a list of
/// blanks. What `where: { has: x }` meant: a field written empty has nothing
/// to group or display.
fn present(value: cel::Value) -> ResolveResult {
    Ok(cel::Value::Bool(is_present(&value)))
}

/// `first(a, b, …)`: the first argument that is [`present`], else `null`.
///
/// A present value that a grain cannot cut is still chosen: falling through
/// to `created` because `date_of_document` held something unparseable would
/// file the document under a date it does not claim, and leaving it
/// ungrouped shows the bad value instead.
fn first(Arguments(args): Arguments) -> ResolveResult {
    Ok(args
        .iter()
        .find(|v| is_present(v))
        .cloned()
        .unwrap_or(cel::Value::Null))
}

/// `field('a.b')`: every scalar at a field path, as text — `written.on`
/// inside a mapping, `confirmed[].by` inside every item of a list. Empty when
/// the path reaches nothing, never an error, which is what CEL's own `a.b`
/// cannot promise on a document without an `a`.
fn field(ftx: &FunctionContext, path: Arc<String>) -> ResolveResult {
    let meta = match ftx.ptx.get_variable("doc") {
        Some(doc) => {
            let doc: cel::Value = doc.as_ref().try_into()?;
            match &doc {
                cel::Value::Map(map) => map
                    .map
                    .get(&Key::String(Arc::new("meta".into())))
                    .map(from_cel)
                    .unwrap_or(Value::Null),
                _ => Value::Null,
            }
        }
        None => Value::Null,
    };
    let found: Vec<String> = values_at(&meta, &FieldPath::parse(&path))
        .into_iter()
        .flat_map(|(_, value)| scalar_texts(value))
        .collect();
    Ok(strings(found))
}

/// `year(x)` and its siblings: every key the texts of `x` cut to.
fn cut(grain: Grain, value: &cel::Value) -> ResolveResult {
    Ok(strings(
        texts(value).iter().flat_map(|t| grain.cuts(t)).collect(),
    ))
}

/// `initial(x)` / `initial(x, n)`.
fn initial(ftx: &FunctionContext, Arguments(args): Arguments) -> ResolveResult {
    let n = match args.as_slice() {
        [_] => 1,
        [_, cel::Value::Int(n)] if *n > 0 => *n as usize,
        [_, _] => return Err(ftx.error("the letter count must be a whole number above 0")),
        _ => return Err(ExecutionError::invalid_argument_count(1, args.len())),
    };
    cut(Grain::Initial(n), &args[0])
}

// ── Messages ────────────────────────────────────────────────────────────────

/// An evaluation error, in a sentence.
///
/// The one rewrite is the misleading case: the `cel` crate reports a call
/// with arguments no overload takes as an undeclared reference to the
/// function, which is what `size(nickname)` on a document without a
/// `nickname` looks like. The function exists; its argument was `null`.
fn explain(error: &ExecutionError) -> String {
    match error {
        ExecutionError::UndeclaredReference(name) if is_known_function(name) => format!(
            "`{name}` cannot take what it was given here — often a field this document does \
             not have, which reads as null (`present()` checks for one)"
        ),
        ExecutionError::NoSuchOverload => "an operator was given values it has no meaning for — \
             often a field this document does not have, which reads as null (`present()` checks \
             for one)"
            .to_string(),
        other => other.to_string(),
    }
}

fn describe(value: &cel::Value) -> &'static str {
    match value {
        cel::Value::List(_) => "a list",
        cel::Value::Map(_) => "a map",
        cel::Value::Function(..) => "a function",
        cel::Value::Int(_) | cel::Value::UInt(_) | cel::Value::Float(_) => "a number",
        cel::Value::String(_) => "text",
        cel::Value::Bytes(_) => "bytes",
        cel::Value::Bool(_) => "true or false",
        cel::Value::Duration(_) => "a duration",
        cel::Value::Timestamp(_) => "a timestamp",
        cel::Value::Null => "null",
        _ => "a value prov cannot name",
    }
}

/// Whether a referenced name is a function someone wrote, rather than one of
/// the operator functions the parser desugars `==`, `in` and `!` into
/// (`_==_`, `@in`, `!_`).
fn is_named_function(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

fn is_known_function(name: &str) -> bool {
    FUNCTIONS.contains(&name) || CEL_FUNCTIONS.contains(&name)
}

/// The parser's message without its `ERROR: <input>:` prefix and the source
/// excerpt it prints below — a config finding is one line.
fn first_line(message: &str) -> String {
    let line = message.lines().next().unwrap_or(message).trim();
    line.strip_prefix("ERROR: <input>:")
        .map(|rest| format!("at {}", rest.trim()))
        .unwrap_or_else(|| line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::select::Ancestor;
    use std::path::PathBuf;

    fn row(fields: &[(&str, Value)]) -> Row {
        let mut meta = Mapping::new();
        for (k, v) in fields {
            meta.insert((*k).into(), v.clone());
        }
        Row {
            path: PathBuf::from("tasks/fix.md"),
            id: Some("abc1234".into()),
            ancestors: vec![
                Ancestor {
                    path: PathBuf::from("README.md"),
                    title: Some("Home".into()),
                    id: None,
                },
                Ancestor {
                    path: PathBuf::from("tasks/tasks.md"),
                    title: Some("Tasks".into()),
                    id: Some("tsk0001".into()),
                },
            ],
            meta: Value::Mapping(meta),
        }
    }

    fn text(s: &str) -> Value {
        Value::String(s.into())
    }

    fn seq(items: &[&str]) -> Value {
        Value::Sequence(items.iter().map(|s| text(s)).collect())
    }

    fn test(src: &str, row: &Row) -> Result<bool, String> {
        Evaluator::new().test(&Expression::parse(src).expect(src), row)
    }

    fn keys(src: &str, row: &Row) -> Vec<String> {
        Evaluator::new()
            .keys(&Expression::parse(src).expect(src), row)
            .unwrap_or_else(|e| panic!("{src}: {e}"))
    }

    #[test]
    fn a_missing_field_is_null_not_an_error() {
        let r = row(&[("status", text("open"))]);
        assert_eq!(test("status == 'open'", &r), Ok(true));
        assert_eq!(test("draft == 'yes'", &r), Ok(false));
        assert_eq!(test("draft == null", &r), Ok(true));
        assert_eq!(test("!(draft in ['a'])", &r), Ok(true));
    }

    #[test]
    fn present_means_carries_a_value() {
        let r = row(&[
            ("blank", text("  ")),
            ("people", seq(&["Ada"])),
            ("empty", seq(&[""])),
            ("zero", Value::Int(0)),
        ]);
        assert_eq!(test("present(people)", &r), Ok(true));
        assert_eq!(test("present(zero)", &r), Ok(true));
        assert_eq!(test("present(blank)", &r), Ok(false));
        assert_eq!(test("present(empty)", &r), Ok(false));
        assert_eq!(test("present(absent)", &r), Ok(false));
    }

    #[test]
    fn first_takes_the_first_present_value_and_does_not_fall_through_a_bad_one() {
        let r = row(&[
            ("date_of_document", text("banana")),
            ("created", text("2026-07-24")),
        ]);
        assert!(keys("month(first(date_of_document, created))", &r).is_empty());
        assert_eq!(keys("month(first(nothing, created))", &r), ["2026-07"]);
    }

    #[test]
    fn grains_read_edtf_and_may_give_several_keys() {
        let r = row(&[
            ("span", text("1918/1920")),
            ("about", text("1913~")),
            ("undated", text("XXXX")),
        ]);
        assert_eq!(keys("year(span)", &r), ["1918", "1919", "1920"]);
        assert_eq!(keys("year(about)", &r), ["1913"]);
        assert!(keys("year(undated)", &r).is_empty());
    }

    #[test]
    fn a_union_of_keys_is_a_list_and_duplicates_collapse() {
        let r = row(&[
            ("created", text("2026-09-01")),
            ("updated", text("2026-09-01T10:00:00Z")),
        ]);
        assert_eq!(keys("[day(created), day(updated)]", &r), ["2026-09-01"]);
        let r = row(&[
            ("created", text("2026-09-01")),
            ("updated", text("2026-09-20")),
        ]);
        assert_eq!(
            keys("[day(created), day(updated)]", &r),
            ["2026-09-01", "2026-09-20"]
        );
    }

    #[test]
    fn initial_cuts_by_character() {
        let r = row(&[("people", seq(&["Ålesund", "ada"]))]);
        assert_eq!(keys("initial(people)", &r), ["Å", "A"]);
        assert_eq!(keys("initial(people, 2)", &r), ["ÅL", "AD"]);
    }

    #[test]
    fn doc_carries_the_document_and_its_ancestors() {
        let r = row(&[("title", text("Fix it"))]);
        assert_eq!(test("doc.title == 'Fix it'", &r), Ok(true));
        assert_eq!(test("doc.id == 'abc1234'", &r), Ok(true));
        assert_eq!(test("doc.path.startsWith('tasks/')", &r), Ok(true));
        assert_eq!(
            test("doc.ancestors.exists(a, a.title == 'Tasks')", &r),
            Ok(true)
        );
        assert_eq!(
            test("doc.ancestors.exists(a, a.id == 'tsk0001')", &r),
            Ok(true)
        );
        assert_eq!(
            test("doc.ancestors.exists(a, a.title == 'Proposals')", &r),
            Ok(false)
        );
    }

    #[test]
    fn field_reads_a_path_and_is_empty_when_it_reaches_nothing() {
        let mut written = Mapping::new();
        written.insert("on".into(), text("/Calendar/2026/09/17.md"));
        let r = row(&[("written", Value::Mapping(written))]);
        assert_eq!(keys("field('written.on')", &r), ["/Calendar/2026/09/17.md"]);
        assert!(keys("field('confirmed[].by')", &r).is_empty());
        assert_eq!(test("'x' in field('nothing.here')", &r), Ok(false));
    }

    #[test]
    fn an_absent_list_is_empty_to_in_and_to_comprehensions() {
        let r = row(&[("people", seq(&["Ada"]))]);
        assert_eq!(test("'Ada' in people", &r), Ok(true));
        assert_eq!(test("'Ada' in tags", &r), Ok(false));
        assert_eq!(test("tags.exists(t, t == 'x')", &r), Ok(false));
        assert_eq!(test("tags.all(t, t == 'x')", &r), Ok(true));
        assert_eq!(keys("tags.map(t, t + '!')", &r), Vec::<String>::new());
        // …inside other expressions too, and only in those positions.
        assert_eq!(test("[('Ada' in tags)] == [false]", &r), Ok(true));
        assert!(test("size(tags) > 0", &r).is_err());
    }

    #[test]
    fn type_names_still_mean_types() {
        let r = row(&[("rating", Value::Int(4))]);
        assert_eq!(test("type(rating) == int", &r), Ok(true));
    }

    #[test]
    fn an_unknown_function_is_refused_at_parse() {
        assert_eq!(
            Expression::parse("dya(created)").unwrap_err(),
            ExpressionError::UnknownFunction("dya".into())
        );
        assert!(Expression::parse("status.startsWith('a') && size(people) > 1").is_ok());
        assert!(matches!(
            Expression::parse("status ==").unwrap_err(),
            ExpressionError::Syntax(_)
        ));
        assert_eq!(Expression::parse("  ").unwrap_err(), ExpressionError::Empty);
    }

    #[test]
    fn failures_are_explained() {
        let r = row(&[]);
        let err = test("size(nickname) > 2", &r).unwrap_err();
        assert!(err.contains("present()"), "{err}");
        let err = test("'text'", &r).unwrap_err();
        assert!(err.contains("true or false"), "{err}");
        let err = Evaluator::new()
            .keys(&Expression::parse("doc").unwrap(), &r)
            .unwrap_err();
        assert!(err.contains("a map"), "{err}");
    }

    #[test]
    fn key_shapes_name_the_common_cases() {
        let shape = |s: &str| Expression::parse(s).unwrap().key_shape();
        assert_eq!(shape("status"), KeyShape::Field("status".into()));
        assert_eq!(
            shape("field('written.on')"),
            KeyShape::Field("written.on".into())
        );
        assert_eq!(
            shape("month(first(date_of_document, created))"),
            KeyShape::Cut(
                Grain::Month,
                Box::new(KeyShape::First(vec![
                    KeyShape::Field("date_of_document".into()),
                    KeyShape::Field("created".into()),
                ]))
            )
        );
        assert_eq!(
            shape("initial(people, 2)"),
            KeyShape::Cut(
                Grain::Initial(2),
                Box::new(KeyShape::Field("people".into()))
            )
        );
        assert_eq!(shape("[day(created), day(updated)]"), KeyShape::Other);
        assert_eq!(shape("doc.title"), KeyShape::Other);
    }
}
