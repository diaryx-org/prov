use std::collections::BTreeSet;
use std::path::Path;

use prov_graph::content::ContentFormat;
use prov_graph::error::{Error, Result};
use prov_graph::link;

use super::docs::{Authoring, render_month_index, render_store_index, render_year_index};
use super::layout::{is_event_id, shard_parts};
use super::{EVENTS_DIR, HistoryReadHost, HistoryStore};

impl<H: HistoryReadHost> HistoryStore<H> {
    /// The event ids in one shard directory: every `*.<ext>` file that is not the
    /// shard's own index. Directory-driven, so it sees exactly what is there.
    pub async fn shard_event_ids(&self, shard: &Path, ext: &str) -> Result<BTreeSet<String>> {
        let suffix = format!(".{ext}");
        let index = format!("index.{ext}");
        let mut ids = BTreeSet::new();
        let Ok(entries) = self.host().graph().listing(shard).await else {
            return Ok(ids);
        };
        for entry in entries {
            let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !entry.file_type().is_file() || name.starts_with('.') || name == index {
                continue;
            }
            if let Some(stem) = name.strip_suffix(&suffix)
                && is_event_id(stem)
            {
                ids.insert(stem.to_string());
            }
        }
        Ok(ids)
    }

    /// The immediate subdirectory names of `dir`, sorted. An unreadable or absent
    /// directory is empty, not an error — the store is grown lazily.
    pub async fn subdirs(&self, dir: &Path) -> Result<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        let Ok(entries) = self.host().graph().listing(dir).await else {
            return Ok(names);
        };
        for entry in entries {
            let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if entry.file_type().is_dir() && !name.starts_with('.') {
                names.insert(name.to_string());
            }
        }
        Ok(names)
    }

    /// The text one history index document *should* hold, rebuilt from the
    /// directory it describes — the repair behind `Fix::RebuildHistoryIndex`.
    ///
    /// Takes only the index's own path: which of the three index kinds it is
    /// falls out of where it sits relative to the store's `events/` directory, so
    /// the repair needs neither the root document nor the `history` pointer —
    /// which matters, because a workspace whose *store index* was mangled is
    /// exactly when you want to rebuild without depending on it.
    ///
    /// Per-shard by construction: a mangled `2026/07/index.<ext>` is rebuilt from
    /// that one directory's listing, touching no other month.
    pub async fn index_text(&self, index: &Path) -> Result<String> {
        let index = link::normalize(index);
        // The index's own extension, not the root's: this repair is reached
        // without the root document on purpose (a workspace whose *store index*
        // was mangled is exactly when you want to rebuild without depending on
        // it), so the file being repaired is the only thing that can say what
        // grammar the store is authored in.
        let style = Authoring {
            ext: index
                .extension()
                .and_then(|e| e.to_str())
                .ok_or_else(|| Error::Structure(format!("{} has no extension", index.display())))?
                .to_string(),
            content: ContentFormat::from_extension(&index).unwrap_or(ContentFormat::Markdown),
            embed: self.embed()?,
        };
        let ext = style.ext.as_str();
        let dir = index.parent().unwrap_or(Path::new(""));

        // Locate the store's `events/` directory by name, walking up from the
        // index. Its absence means this *is* the store index.
        let depth_below_events = dir
            .components()
            .rev()
            .position(|c| c.as_os_str() == EVENTS_DIR);
        match depth_below_events {
            // `<store>/events/<year>/<month>/index.<ext>`
            Some(2) => {
                let (year, month) = shard_parts(
                    dir.parent()
                        .and_then(|p| p.parent())
                        .map(|events| dir.strip_prefix(events).unwrap_or(dir))
                        .unwrap_or(dir),
                )?;
                render_month_index(
                    &year,
                    &month,
                    &self.shard_event_ids(dir, ext).await?,
                    &style,
                )
            }
            // `<store>/events/<year>/index.<ext>`
            Some(1) => {
                let year = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                render_year_index(&year, &self.event_months(dir, ext).await?, &style)
            }
            // `<store>/index.<ext>` — the store index itself.
            _ => render_store_index(
                &self.event_years(&dir.join(EVENTS_DIR), ext).await?,
                self.forgotten_link(&index).await?.as_deref(),
                &style,
            ),
        }
    }
}
