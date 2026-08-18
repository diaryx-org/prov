use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use prov_graph::content::ContentFormat;
use prov_graph::document::MetaCarrier;
use prov_graph::error::{Error, Result};

use super::docs::Authoring;
use super::event_id::comparable;
use super::layout::{StoreLocation, blob_path, event_path, id_stamp_of, store_dir};
use super::model::{Event, Latest, Presence, Retrieved, Subject, Summary, Version};
use super::paths::{path_sort_key, under};
use super::{EVENTS_DIR, HISTORY_DIR, HistoryReadHost, HistoryStore};

impl<H: HistoryReadHost> HistoryStore<H> {
    /// The content format the store's documents are authored in — the root
    /// document's own, falling back to Markdown when the root is a whole-file
    /// metadata document (which has no prose body to inherit).
    pub fn content(&self, root_doc: &Path) -> ContentFormat {
        ContentFormat::from_extension(root_doc).unwrap_or(ContentFormat::Markdown)
    }

    /// The extension the store's documents are authored with.
    pub fn ext(&self, root_doc: &Path) -> &'static str {
        self.content(root_doc).extension()
    }

    /// The fenced-frontmatter archetype the store's documents are authored in.
    ///
    /// Resolved from the workspace's **declared embedding** — the `(embed_style,
    /// default_embed_format)` pair every other document prov authors goes
    /// through — so a fig workspace's history reads like the rest of it, and an
    /// HTML workspace's history is an HTML data island rather than a `;;;` fence
    /// sitting in a `.html` file that no browser will render.
    ///
    /// Two styles have no fenced archetype and fall back to the format's plain
    /// frontmatter carrier: `separate` (a whole-file sidecar, which an event
    /// document cannot be — it has a prose body, and the manifest is the point of
    /// it), and any `(style, format)` pair fig has no fence for. The fallback is
    /// what keeps the store authored in *some* legible carrier rather than
    /// failing the capture over a presentational choice.
    pub fn embed(&self) -> Result<fig::EmbedType> {
        let format = self.host().default_embed_format();
        let carrier = prov_graph::document::embed_carrier(self.host().embed_style(), format)
            .filter(|c| matches!(c, MetaCarrier::Fenced(_)))
            .unwrap_or_else(|| prov_graph::document::frontmatter_carrier(format));
        match carrier {
            MetaCarrier::Fenced(embed) => Ok(embed),
            // `frontmatter_carrier` only ever returns a fenced archetype.
            _ => Err(Error::Structure(
                "history events need a fenced frontmatter carrier".into(),
            )),
        }
    }

    /// How the store's documents are authored, resolved once: the extension they
    /// get, the grammar their prose is written in, and the carrier their
    /// frontmatter rides in.
    ///
    /// Carried together because they are one decision. Resolving them separately
    /// is how the store came to write `.html` files holding Markdown bodies: the
    /// extension followed the workspace and the body did not.
    pub fn authoring(&self, root_doc: &Path) -> Result<Authoring> {
        Ok(Authoring {
            ext: self.ext(root_doc).to_string(),
            content: self.content(root_doc),
            embed: self.embed()?,
        })
    }

    /// The store index document, and how it was found.
    ///
    /// The root's `history` pointer first. Failing that, the **conventional
    /// path** is probed on disk — a store whose pointer a transport mangled out of
    /// the root is still a store, and the alternative is that prov goes blind to
    /// an intact safety net while a shell and `cp` can still recover from it.
    /// Failing both, the path the first capture will bootstrap into, reported
    /// [`Absent`](StoreLocation::Absent).
    pub async fn store_index(&self, root_doc: &Path) -> Result<(PathBuf, StoreLocation)> {
        if let Some(path) = self.host().history_path(root_doc).await? {
            return Ok((path, StoreLocation::Declared));
        }
        let conventional = PathBuf::from(HISTORY_DIR).join(format!("index.{}", self.ext(root_doc)));
        let found = match self.host().graph().exists(&conventional).await? {
            true => StoreLocation::Conventional,
            false => StoreLocation::Absent,
        };
        Ok((conventional, found))
    }

    /// The **capture set**: the live graph, minus prov's two byte-parking stores
    /// and its one derived page.
    ///
    /// [`reachable_files`](HistoryReadHost::reachable_files) — §8's bounded walk,
    /// the same population `check` validates — with **three** exclusions, each
    /// load-bearing:
    ///
    /// - **The store itself.** It is reachable off the root, so a naive "capture
    ///   everything reachable" would capture the store inside the store: no
    ///   capture could ever be empty, and an exact restore of an old event would
    ///   delete every event newer than it, destroying the recovery points
    ///   themselves. The store is the one subtree the mechanism is deliberately
    ///   blind to, and the one exclusion history applies for itself.
    /// - **The recycle bin's `items/`.** Already unreached, and excluded even so,
    ///   on purpose: bytes the user has consigned to the bin should not be
    ///   *newly* retained by a routine capture.
    /// - **The generated about page.** It is *derived* — a pure function of the
    ///   configuration, which this same manifest captures — so parking its bytes
    ///   stores nothing that cannot be reproduced, and a new blob would be parked
    ///   on every config change for no recovery value. Restoring an event
    ///   restores the config that determines the page, and `check` reports the
    ///   page as stale until it is rewritten from that config, which is the same
    ///   repair by a shorter route. Excluding it also removes an ordering hazard:
    ///   the first capture *bootstraps* the store, which changes what the page
    ///   says about this workspace, so a captured page would be one the capture
    ///   itself invalidated.
    ///
    /// The last two are the host's to name
    /// ([`history_exclusions`](HistoryReadHost::history_exclusions)) — where the
    /// bin parks and which page is derived are facts about the workspace, not
    /// about the store.
    ///
    /// Everything else structural stays in — the registry, the config document,
    /// and the recycle bin's *index*. Capturing the bin index keeps the common
    /// case correct: a document live at capture time comes back live, and the bin
    /// index reverts to a state that does not list it.
    ///
    /// Returned in **manifest order** — [`path_sort_key`], byte-wise ascending on
    /// the joined path string (§3.1) — not the component-wise order a
    /// `BTreeSet<PathBuf>` iterates in. The two agree almost everywhere and
    /// disagree exactly where a file and a same-named directory are siblings
    /// (`notes.md` next to `notes/`), which is precisely the case a real
    /// workspace produces and a `Path`-ordered manifest would get wrong.
    pub async fn capture_set(&self, root_doc: &Path) -> Result<Vec<PathBuf>> {
        let (store_index, _) = self.store_index(root_doc).await?;
        let store = store_dir(&store_index);
        let excluded = self.host().history_exclusions(root_doc).await?;
        let mut files: Vec<PathBuf> = self
            .host()
            .reachable_files(root_doc)
            .await?
            .into_iter()
            .filter(|p| !under(p, &store))
            .filter(|p| !excluded.iter().any(|dir| under(p, dir)))
            .collect();
        files.sort_by_key(|p| path_sort_key(p));
        Ok(files)
    }

    /// What the store holds, without reading the history in it — the cheap
    /// answer to "is a capture due?".
    ///
    /// [`list`](Self::list) is the wrong way to ask that. It parses every event
    /// document, and each holds one row per file in the workspace, so a host
    /// asking on every open pays O(events × files) forever. This walks the
    /// shard tree — one listing per month that has events — and reads **one**
    /// document.
    ///
    /// ## Why one document is both necessary and enough
    ///
    /// An id carries its own timestamp, but only to the minute
    /// (`<YYYY>-<MM>-<DD>-<HHMM>-<8 hex>`, [`mint_id`](super::event_id::mint_id)),
    /// where `created` is written to [`FRACTION_DIGITS`] places. So filenames
    /// alone cannot order two events captured in the same minute, and the 8-hex
    /// suffix is a content digest — sorting by it would be arbitrary, and would
    /// disagree with `list` exactly when two captures land close together, which
    /// is the case a cadence check meets on a busy day.
    ///
    /// What filenames *can* do is narrow. Truncation to the minute is monotonic,
    /// so the greatest `created` in the store is certainly inside the greatest
    /// stamp present: reading that bucket — nearly always one file — and ordering
    /// it the way [`events_in`](Self::events_in) does settles it exactly. A
    /// bucket whose documents were all torn in transit yields nothing to order,
    /// so the search falls to the next stamp down rather than reporting no
    /// history at all; `events` still counts the torn slots, because the file is
    /// evidence a capture happened even when its contents are not.
    ///
    /// [`FRACTION_DIGITS`]: super::event_id::FRACTION_DIGITS
    pub async fn summary(&self, root_doc: &Path) -> Result<Summary> {
        let (store_index, found) = self.store_index(root_doc).await?;
        if !found.exists() {
            return Ok(Summary::default());
        }
        let ext = self.ext(root_doc);
        let events_root = store_dir(&store_index).join(EVENTS_DIR);

        // One listing per shard. Ids are grouped by their minute stamp so the
        // newest bucket is in hand without a second pass, and a store with no
        // stamped ids at all (nothing but torn files) still reports its slots.
        let mut buckets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut slots = 0usize;
        for year in self.subdirs(&events_root).await? {
            for month in self.subdirs(&events_root.join(&year)).await? {
                let shard = events_root.join(&year).join(&month);
                for id in self.shard_event_ids(&shard, ext).await? {
                    slots += 1;
                    if let Some(stamp) = id_stamp_of(&id) {
                        buckets.entry(stamp).or_default().insert(id);
                    }
                }
            }
        }

        // Newest stamp first, stopping at the first bucket that yields a readable
        // event. `events_in`'s own ordering, so the answer is the one
        // `list().last()` would have given.
        let mut latest = None;
        for (_, ids) in buckets.iter().rev() {
            let mut readable: Vec<Event> = Vec::new();
            for id in ids {
                let path = event_path(&store_index, id, ext)?;
                let Ok((_, doc)) = self.host().graph().load(&path).await else {
                    continue;
                };
                if let Some(event) = super::docs::parse_event(&path, id, &doc.meta) {
                    readable.push(event);
                }
            }
            readable.sort_by(|a, b| {
                comparable(&a.created)
                    .cmp(&comparable(&b.created))
                    .then_with(|| a.id.cmp(&b.id))
            });
            if let Some(event) = readable.pop() {
                latest = Some(Latest {
                    id: event.id,
                    created: event.created,
                });
                break;
            }
        }

        Ok(Summary {
            store_exists: true,
            events: slots,
            latest,
        })
    }

    /// What the store occupies on disk, in bytes — every event document, every
    /// blob, every index.
    ///
    /// Separate from [`summary`](Self::summary), and expensive in the way that
    /// one is not: a [`DirEntry`](prov_graph::fs::DirEntry) carries no length, so
    /// this is one `metadata` call per file in the store. Over a file-provider
    /// backend that is a per-file round trip, so it belongs behind a screen a
    /// person opened on purpose — not on a path that runs at every vault open.
    ///
    /// A file that vanishes mid-walk (a prune racing this, a transport moving
    /// bytes) contributes nothing rather than failing the total: the answer is a
    /// size to show someone, not an accounting record.
    pub async fn store_bytes(&self, root_doc: &Path) -> Result<u64> {
        let (store_index, found) = self.store_index(root_doc).await?;
        if !found.exists() {
            return Ok(0);
        }
        let mut total = 0u64;
        let mut stack = vec![store_dir(&store_index)];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = self.host().graph().listing(&dir).await else {
                continue;
            };
            for entry in entries {
                let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let path = dir.join(name);
                if entry.file_type().is_dir() {
                    stack.push(path);
                } else if let Ok(meta) = self.host().graph().stat(&path).await {
                    total += meta.len();
                }
            }
        }
        Ok(total)
    }

    /// Every event in the store, oldest first (by `created`, then id).
    ///
    /// Read by **scanning the shard directories**, not by following the index
    /// documents — the indexes are a rebuildable cache, so a mangled one must not
    /// be able to hide an event that is sitting right there. A document that does
    /// not parse, or that carries no manifest, is skipped rather than fatal.
    pub async fn list(&self, root_doc: &Path) -> Result<Vec<Event>> {
        let (store_index, found) = self.store_index(root_doc).await?;
        if !found.exists() {
            return Ok(Vec::new());
        }
        let (events, _) = self.events_in(&store_index, self.ext(root_doc)).await?;
        Ok(events)
    }

    /// [`list`](Self::list) against a store index already in hand — so a pass
    /// that has resolved the store once does not resolve it again through the
    /// root.
    ///
    /// Returns the events that loaded and parsed, oldest first, **alongside every
    /// event-shaped file that did not** — its path and why.
    /// [`shard_event_ids`](Self::shard_event_ids) finds a file by name alone
    /// (§4's id shape plus the extension), so a document a transport tore in
    /// transit — half-written, or a conflict marker landing inside its
    /// frontmatter — is still counted as an event *slot* even though nothing in
    /// it can be trusted.
    ///
    /// The read-only callers ([`list`](Self::list), `history-show`,
    /// `history-log`) drop the second list on the floor: a degraded read is
    /// exactly what those verbs are for, and the store-format doc says so (§7,
    /// §10). The callers that *destroy* — `history-prune` and `history-forget` —
    /// and the `check` sweep must not: a blob set built only from the survivors
    /// is a bound with an unknown hole in it, and a prune or forget that trusts
    /// it can free bytes a torn event was the only record of naming.
    pub async fn events_in(
        &self,
        store_index: &Path,
        ext: &str,
    ) -> Result<(Vec<Event>, Vec<(PathBuf, String)>)> {
        let events_root = store_dir(store_index).join(EVENTS_DIR);
        let mut events = Vec::new();
        let mut unreadable = Vec::new();
        for year in self.subdirs(&events_root).await? {
            for month in self.subdirs(&events_root.join(&year)).await? {
                let shard = events_root.join(&year).join(&month);
                for id in self.shard_event_ids(&shard, ext).await? {
                    let path = shard.join(format!("{id}.{ext}"));
                    match self.host().graph().load(&path).await {
                        Ok((_, doc)) => match super::docs::parse_event(&path, &id, &doc.meta) {
                            Some(event) => events.push(event),
                            None => unreadable.push((
                                path,
                                "not a history event document (no `created` or `files`)"
                                    .to_string(),
                            )),
                        },
                        Err(e) => unreadable.push((path, e.to_string())),
                    }
                }
            }
        }
        // Normalized, not raw: a store mixes the precisions of every version that
        // ever wrote into it (see [`comparable`]). The id tiebreak survives for
        // the genuine tie — two devices landing on the same microsecond — where it
        // is arbitrary but deterministic, which is all an ordering owes a fork.
        events.sort_by(|a, b| {
            comparable(&a.created)
                .cmp(&comparable(&b.created))
                .then_with(|| a.id.cmp(&b.id))
        });
        unreadable.sort();
        Ok((events, unreadable))
    }

    /// One event by id, resolved through the **pure id → path function** rather
    /// than through any index — so an event answers for itself with every index
    /// document in the store destroyed.
    ///
    /// `Ok(None)` when the store holds no such event (including when there is no
    /// store yet). An error when `id` is not an event id at all, or when the
    /// document is sitting there but is not an event.
    pub async fn event(&self, root_doc: &Path, id: &str) -> Result<Option<Event>> {
        let (store_index, found) = self.store_index(root_doc).await?;
        if !found.exists() {
            return Ok(None);
        }
        let path = event_path(&store_index, id, self.ext(root_doc))?;
        if !self.host().graph().exists(&path).await? {
            return Ok(None);
        }
        let (_, doc) = self.host().graph().load(&path).await?;
        super::docs::parse_event(&path, id, &doc.meta)
            .map(Some)
            .ok_or_else(|| Error::Structure(format!("`{id}` is not a history event document")))
    }

    /// The captured paths in `event` whose pre-image bytes are **not** parked in
    /// the store — the "this event is half-synced" report.
    ///
    /// A manifest and the blobs it names travel over the transport
    /// independently, and a small event document routinely lands well before a
    /// hundred megabytes of bytes it points at. That is ordinary in-flight state
    /// rather than damage, which is exactly why it has to be legible under a
    /// *read* verb before anyone asks a restore to act on it — and why a restore
    /// reports this same set rather than computing its own.
    ///
    /// Presence is tested once per distinct hash, not once per row: a manifest
    /// routinely names one blob from several paths, and a workspace is captured
    /// whole. A row whose hash prov could not have parked in the first place
    /// (a foreign digest, a mangled string) names no blob that could be found, so
    /// it counts as missing rather than failing the whole read.
    pub async fn missing_blobs(&self, root_doc: &Path, event: &Event) -> Result<BTreeSet<PathBuf>> {
        let (store_index, _) = self.store_index(root_doc).await?;
        let mut seen: BTreeMap<&str, bool> = BTreeMap::new();
        let mut missing = BTreeSet::new();
        for file in &event.files {
            let present = match seen.get(file.hash.as_str()) {
                Some(present) => *present,
                None => {
                    let present = match blob_path(&store_index, &file.hash) {
                        Ok(blob) => self.host().graph().exists(&blob).await?,
                        Err(_) => false,
                    };
                    seen.insert(&file.hash, present);
                    present
                }
            };
            if !present {
                missing.insert(file.path.clone());
            }
        }
        Ok(missing)
    }

    /// The bytes one captured file held at `event` — the pre-image its manifest
    /// row names, read straight out of the blob store.
    ///
    /// The primitive the other read verbs were missing. [`event`](Self::event)
    /// reports what a capture *recorded*; this produces what it *holds*, which is
    /// what makes the store usable with tools that are not prov at all:
    ///
    /// ```text
    /// prov history-cat <event> notes.md | diff - notes.md
    /// ```
    ///
    /// A lookup, not a reconstruction. A manifest row names a content-addressed
    /// blob directly, so the cost is one read and does not grow with the number
    /// of events between that capture and now — the payoff for storing full
    /// manifests that a delta log would have had to fold to match.
    ///
    /// The subject is a [`Subject`] rather than a bare path for the reason
    /// [`log`](Self::log) takes one: an id reaches a document that has since
    /// moved, where a path-keyed lookup silently misses it and reports the
    /// document as never captured. A [`Subject::Path`] is matched against the
    /// path the manifest **recorded** — what the document was called at that
    /// capture, which is not necessarily what it is called now.
    ///
    /// Absence comes back in three kinds rather than as one error; see
    /// [`Retrieved`], and `HistoryIssue::BlobMissing` for why the distinction is
    /// worth carrying.
    pub async fn cat(
        &self,
        root_doc: &Path,
        event: &Event,
        subject: &Subject,
    ) -> Result<Retrieved> {
        let Some(file) = event.files.iter().find(|file| match subject {
            Subject::Id(id) => file.id.as_ref() == Some(id),
            Subject::Path(path) => &file.path == path,
        }) else {
            return Ok(Retrieved::Unrecorded);
        };
        let (path, hash) = (file.path.clone(), file.hash.clone());

        let (store_index, _) = self.store_index(root_doc).await?;
        // A hash prov could not have parked names no blob that could be found —
        // the same judgement `missing_blobs` makes about a foreign or mangled
        // digest: absent, rather than an error that fails the whole read.
        let Ok(blob) = blob_path(&store_index, &hash) else {
            return self.absent(root_doc, path, hash).await;
        };
        if !self.host().graph().exists(&blob).await? {
            return self.absent(root_doc, path, hash).await;
        }
        let bytes = self.host().graph().read_bytes(&blob).await?;
        Ok(Retrieved::Bytes { path, hash, bytes })
    }

    /// Which kind of absence: destroyed on purpose, or not arrived yet.
    ///
    /// The tombstone list is the only thing that tells them apart, and it is read
    /// **only once the bytes are known to be gone** — so the ordinary path, where
    /// the blob is right there, pays nothing for a distinction it does not need.
    async fn absent(&self, root_doc: &Path, path: PathBuf, hash: String) -> Result<Retrieved> {
        Ok(match self.forgotten(root_doc).await?.contains(&hash) {
            true => Retrieved::Forgotten { path, hash },
            false => Retrieved::NoBytes { path, hash },
        })
    }

    /// One document's lineage across every capture, oldest first: pull its row
    /// out of each manifest in turn, and keep only the events where that row
    /// *changed*.
    ///
    /// This is the payoff for the manifest's `id` column, and it is a **derived
    /// query, not a storage design** — nothing in the store is keyed by document,
    /// and nothing here writes. Following a [`Subject::Id`] makes the lineage
    /// rename-robust in a way no path-keyed store can be: a move shows as one
    /// document that changed path, where a path-keyed view shows two unrelated
    /// lineages that happen to abut.
    ///
    /// Consecutive events are deduped on the **whole manifest row** — path, id
    /// and hash — not on the hash alone. A rename leaves the bytes
    /// byte-identical, so a hash-only dedupe would swallow precisely the event
    /// that following an id exists to surface. Including the id means a document
    /// acquiring one is a point too, which is right: the row changed.
    ///
    /// An event that does not mention the subject records [`Presence::Gone`], but
    /// only once the document has been seen, so a lineage starts where its
    /// document does rather than with a run of absences.
    ///
    /// A path-keyed lineage does not simply stop at a rename. When the tracked
    /// path leaves a capture set, [`infer_rename`] asks whether the manifests
    /// already say where it went — and when they do so unambiguously, the
    /// lineage follows, with that point marked [`inferred`](Version::inferred)
    /// so a display never presents a guess with the confidence of a recorded id.
    /// An id-keyed lineage never infers: the `id` column is the answer. Events are walked in
    /// capture order (`created`, then id), so concurrent captures on two devices
    /// interleave rather than branching — this is a display, and `history-list`
    /// is where forks are named.
    ///
    /// Cost is one pass over every event document in the store. That is the
    /// honest price of storing by consistent cut and querying by document, and it
    /// is why this is a query rather than an index.
    pub async fn log(&self, root_doc: &Path, subject: &Subject) -> Result<Vec<Version>> {
        let events = self.list(root_doc).await?;
        let mut log: Vec<Version> = Vec::new();
        // The path a path-keyed lineage is currently following, which a rename
        // moves. `None` for an id-keyed one: the `id` column is the answer, so
        // there is nothing to track and nothing to guess.
        let mut tracked = match subject {
            Subject::Path(path) => Some(path.clone()),
            Subject::Id(_) => None,
        };
        let mut earlier: Option<&Event> = None;

        for event in &events {
            // A path that has left this capture set may have been renamed rather
            // than deleted. Asked only when it is absent — the ordinary event,
            // where the row is right there, never pays for the inference.
            let inferred = match (&tracked, earlier) {
                (Some(path), Some(earlier))
                    if !event.files.iter().any(|file| &file.path == path) =>
                {
                    match infer_rename(earlier, event, path) {
                        Some(moved) => {
                            tracked = Some(moved);
                            true
                        }
                        None => false,
                    }
                }
                _ => false,
            };

            let row = match &tracked {
                Some(path) => event.files.iter().find(|file| &file.path == path),
                None => event.files.iter().find(|file| match subject {
                    Subject::Id(id) => file.id.as_ref() == Some(id),
                    // Unreachable: a path subject always tracks.
                    Subject::Path(path) => &file.path == path,
                }),
            };
            let state = match row {
                Some(file) => Presence::At {
                    path: file.path.clone(),
                    id: file.id.clone(),
                    hash: file.hash.clone(),
                },
                None => Presence::Gone,
            };
            earlier = Some(event);
            match log.last() {
                // The document did not exist yet when this capture was taken.
                None if state == Presence::Gone => continue,
                Some(previous) if previous.state == state => continue,
                _ => {}
            }
            log.push(Version {
                event: event.id.clone(),
                created: event.created.clone(),
                label: event.label.clone(),
                state,
                inferred,
            });
        }
        Ok(log)
    }
}

/// The path `path` was renamed to between `earlier` and `later`, if the two
/// manifests say so unambiguously.
///
/// The manifests already carry what is needed: a path that disappears and a path
/// that appears carrying **the same hash** in the same event is a rename, since a
/// move leaves the bytes byte-identical. That recovers lineage across a rename
/// for every document with no id — which, in an archive of any age, is nearly all
/// of them.
///
/// ## Why it must be one-to-one
///
/// The pairing is only sound when it is unambiguous, and the tempting weaker rule
/// is wrong in a way a real workspace produces on day one: **every empty file
/// shares one digest**, as does every copy of the same boilerplate. If two paths
/// carrying that digest left and three appeared, any pairing is a guess, and a
/// guess presented as lineage is worse than the honest break — so exactly one
/// must have gone and exactly one arrived, and the one that went must be the
/// document being followed.
///
/// What survives that rule is a residual false positive nothing in the store can
/// rule out: one file deleted and one unrelated file created with byte-identical
/// content in the same capture. It is indistinguishable from a rename *in the
/// data*, which is why the point it produces is marked
/// [`inferred`](Version::inferred) rather than presented as a recorded fact.
fn infer_rename(earlier: &Event, later: &Event, path: &Path) -> Option<PathBuf> {
    // The bytes the tracked document had when it was last seen. Absent means it
    // was already gone, and a lineage does not resume by inference.
    let hash = &earlier.files.iter().find(|f| f.path == path)?.hash;
    let held = |event: &Event, path: &Path| event.files.iter().any(|f| f.path == path);

    let mut gone = earlier
        .files
        .iter()
        .filter(|f| &f.hash == hash && !held(later, &f.path));
    let mut arrived = later
        .files
        .iter()
        .filter(|f| &f.hash == hash && !held(earlier, &f.path));

    let (left, came) = (gone.next()?, arrived.next()?);
    (left.path == path && gone.next().is_none() && arrived.next().is_none())
        .then(|| came.path.clone())
}

/// Format the paths [`HistoryStore::events_in`] could not read, for a refusal
/// message a destructive verb raises rather than acting on an incomplete
/// reference set.
pub fn describe_unreadable(unreadable: &[(PathBuf, String)]) -> String {
    unreadable
        .iter()
        .map(|(path, error)| format!("{} ({error})", path.display()))
        .collect::<Vec<_>>()
        .join(", ")
}
