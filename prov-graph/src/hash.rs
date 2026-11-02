//! Hash maps and sets for prov's own keys — paths and names — with a faster
//! hasher than std's.
//!
//! A walk asks a path-keyed set or map once per link per document: the read
//! memo, the queue, the reachable set's view. std's default, SipHash, is
//! built to stand up to keys an attacker chose, and over ten thousand
//! documents it was a twentieth of a `check`. foldhash is several times
//! faster on short keys and still seeded per process, so a workspace's file
//! names cannot be chosen to make it slow on purpose.

/// A [`HashMap`](std::collections::HashMap) with the faster hasher.
pub type FastMap<K, V> = std::collections::HashMap<K, V, foldhash::fast::RandomState>;

/// A [`HashSet`](std::collections::HashSet) with the faster hasher.
pub type FastSet<T> = std::collections::HashSet<T, foldhash::fast::RandomState>;
