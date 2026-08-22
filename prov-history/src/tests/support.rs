//! Fixtures the skiplist test modules share.

use std::path::{Path, PathBuf};

pub(super) use super::host::TestHost;
pub(super) use prov_graph::exec::block_on;

use historica::working::Rule;

use crate::{Skiplist, Standing};

pub(super) fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("prov-history-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(super) fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

pub(super) fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

/// Compute the skiplist over `host` with nothing standing in the store.
pub(super) fn plan(host: &TestHost) -> Skiplist {
    plan_against(host, &Standing::default())
}

/// Compute the skiplist over `host` against what a store already says.
pub(super) fn plan_against(host: &TestHost, standing: &Standing) -> Skiplist {
    block_on(crate::skiplist(host, Path::new("index.md"), standing)).unwrap()
}

/// The rules a plan computed, rendered the way the file would hold them —
/// what most assertions want to compare against.
pub(super) fn lines(skiplist: &Skiplist) -> Vec<String> {
    skiplist
        .rules
        .iter()
        .map(|skip| skip.rule.to_string())
        .collect()
}

pub(super) fn rule(line: &str) -> Rule {
    let (key, value) = line.split_once(' ').unwrap();
    match key {
        "skip" if value.ends_with('/') => Rule::Under(value.trim_end_matches('/').to_owned()),
        "skip" => Rule::Path(value.to_owned()),
        "skip-suffix" => Rule::Suffix(value.to_owned()),
        other => panic!("not a rule key: {other}"),
    }
}
