//! Talking to the historica store: what stands, and rewriting the region.
//!
//! Everything here goes through the `historica` library rather than
//! re-deriving its formats — the store's own `Skipped::parse` reads the rules,
//! its `Rule` renders the lines this crate writes, and the tracked set is the
//! merged tree at the store's current heads, computed by the store. What this
//! module adds is the one convention historica does not have: a **generated
//! region** inside `skipped.txt`, fenced by marker comments, regenerated whole
//! the way a changelog's generated region is. Everything outside the markers
//! belongs to the person and is preserved line for line.
//!
//! The markers are `#` comment lines, so a historica that has never heard of
//! prov reads the file unchanged — the region is a convention *within* the
//! format, not an extension of it.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use historica::store::{MaterialiseError, STORE_DIR, Store, StoreError};
use historica::working::{MalformedSkip, Rule, SKIPPED_FILE, Skipped};

use crate::Skiplist;

/// The line opening the generated region. Matched by prefix, so later
/// wording changes do not orphan older regions.
pub const REGION_BEGIN: &str = "# prov:begin — computed from the workspace graph and regenerated whole; \
     edits between the markers are overwritten";

/// The line closing the generated region.
pub const REGION_END: &str = "# prov:end";

fn is_begin(line: &str) -> bool {
    line.trim_end().starts_with("# prov:begin")
}

fn is_end(line: &str) -> bool {
    line.trim_end() == REGION_END
}

/// What the store already says, read once so planning is pure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Standing {
    /// The rules outside the region — historica's defaults and whatever the
    /// person added by hand. Theirs; never rewritten.
    pub hand: Vec<Rule>,
    /// The rules the region holds now, in file order.
    pub region: Vec<Rule>,
    /// Every path the store's tree holds at its current heads. A rule may
    /// never cover one: historica refuses to record while it does.
    pub tracked: BTreeSet<String>,
}

impl Standing {
    /// Read the store beside the workspace at `root`.
    ///
    /// The store's absence is an error rather than an empty answer — a
    /// skiplist with nowhere to land is a prompt to run `historica init`, and
    /// pretending otherwise would compute a plan nothing can apply.
    pub fn read(root: &Path) -> Result<Self, StandingError> {
        let store = Store::open(root.join(STORE_DIR))?;
        let text = read_skipped(root)?;
        let split = split_region(&text)?;
        let mut hand = rules_of(&split.before).map_err(StandingError::Skip)?;
        hand.extend(rules_of(&split.after).map_err(StandingError::Skip)?);
        let region = rules_of(&split.region).map_err(StandingError::Skip)?;

        let history = store.history();
        let superseded = history.superseded();
        let heads: Vec<_> = history
            .heads()
            .into_iter()
            .filter(|head| !superseded.contains(head))
            .collect();
        let tracked = match heads.is_empty() {
            true => BTreeSet::new(),
            false => store
                .merged_tree_of(&heads)?
                .tree
                .files()
                .map(|(_, path)| path.to_owned())
                .collect(),
        };

        Ok(Self {
            hand,
            region,
            tracked,
        })
    }
}

/// Rewrite the generated region of `skipped.txt` to say what `skiplist`
/// computed, leaving every line outside the markers as it stands.
///
/// A file with no markers yet gains the region at its end; a plan with no
/// rules and no region to empty writes nothing at all. The result is parsed
/// with historica's own reader before it is written — this crate never leaves
/// behind a file the store would refuse — and lands by rename, so a crash
/// leaves the old file, not half of a new one.
pub fn apply(root: &Path, skiplist: &Skiplist) -> Result<(), StandingError> {
    let path = root.join(STORE_DIR).join(SKIPPED_FILE);
    let text = read_skipped(root)?;
    let split = split_region(&text)?;
    if skiplist.rules.is_empty() && !split.found {
        return Ok(());
    }

    let mut out = String::new();
    out.push_str(&split.before);
    out.push_str(REGION_BEGIN);
    out.push('\n');
    for skip in &skiplist.rules {
        out.push_str(&skip.rule.to_string());
        out.push('\n');
    }
    out.push_str(REGION_END);
    out.push('\n');
    out.push_str(&split.after);

    Skipped::parse(&out).map_err(StandingError::Skip)?;
    let staged = path.with_file_name(format!("{SKIPPED_FILE}.new"));
    fs::write(&staged, &out)?;
    fs::rename(&staged, &path)?;
    Ok(())
}

fn read_skipped(root: &Path) -> Result<String, StandingError> {
    match fs::read_to_string(root.join(STORE_DIR).join(SKIPPED_FILE)) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn rules_of(text: &str) -> Result<Vec<Rule>, MalformedSkip> {
    Ok(Skipped::parse(text)?.rules().cloned().collect())
}

/// The file cut at its markers. Line endings are normalised to one `\n` per
/// line; `region` is the text strictly between the marker lines.
struct SplitRegion {
    before: String,
    region: String,
    after: String,
    found: bool,
}

fn split_region(text: &str) -> Result<SplitRegion, StandingError> {
    let mut before = String::new();
    let mut region = String::new();
    let mut after = String::new();
    let mut state = State::Before;
    for (index, line) in text.lines().enumerate() {
        let at = index + 1;
        match state {
            State::Before if is_begin(line) => state = State::Within,
            State::Before if is_end(line) => {
                return Err(StandingError::Region {
                    at,
                    because: "an end marker stands before any begin marker",
                });
            }
            State::Before => push_line(&mut before, line),
            State::Within if is_end(line) => state = State::After,
            State::Within if is_begin(line) => {
                return Err(StandingError::Region {
                    at,
                    because: "a second begin marker stands inside the region",
                });
            }
            State::Within => push_line(&mut region, line),
            State::After if is_begin(line) || is_end(line) => {
                return Err(StandingError::Region {
                    at,
                    because: "only one region: a second marker stands after the region closed",
                });
            }
            State::After => push_line(&mut after, line),
        }
    }
    match state {
        State::Within => Err(StandingError::Region {
            at: text.lines().count(),
            because: "the begin marker has no end marker after it",
        }),
        found => Ok(SplitRegion {
            before,
            region,
            after,
            found: matches!(found, State::After),
        }),
    }
}

enum State {
    Before,
    Within,
    After,
}

fn push_line(text: &mut String, line: &str) {
    text.push_str(line);
    text.push('\n');
}

/// What reading or rewriting the store can refuse.
#[derive(Debug)]
pub enum StandingError {
    /// The store could not be opened or read — including its absence, which
    /// is a prompt to run `historica init` rather than a state to plan over.
    Store(StoreError),
    /// The tracked set could not be replayed from the store's documents.
    Materialise(MaterialiseError),
    /// `skipped.txt` holds a line historica's own reader refuses.
    Skip(MalformedSkip),
    /// The region's markers do not delimit one region.
    Region { at: usize, because: &'static str },
    /// Reading or writing the file itself failed.
    Io(io::Error),
}

impl fmt::Display for StandingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StandingError::Store(error) => write!(f, "{error}"),
            StandingError::Materialise(error) => write!(f, "{error}"),
            StandingError::Skip(error) => write!(f, "skipped.txt: {error}"),
            StandingError::Region { at, because } => {
                write!(f, "skipped.txt line {at}: {because}")
            }
            StandingError::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for StandingError {}

impl From<StoreError> for StandingError {
    fn from(error: StoreError) -> Self {
        StandingError::Store(error)
    }
}

impl From<MaterialiseError> for StandingError {
    fn from(error: MaterialiseError) -> Self {
        StandingError::Materialise(error)
    }
}

impl From<io::Error> for StandingError {
    fn from(error: io::Error) -> Self {
        StandingError::Io(error)
    }
}
