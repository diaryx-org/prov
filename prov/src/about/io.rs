//! Where the pure generator meets the filesystem.
//!
//! Everything above this module is a function of configuration; everything
//! here decides *when* to call it, *where* the result goes, and *what changed*
//! since the last time it was written. That is [`Workspace`]'s job, not
//! [`generate`](super::generate)'s, so these are `Workspace` methods living
//! beside the generator they drive rather than inside it — the boundary the
//! parent module's doc comment draws between "a pure function of
//! configuration" and the corpus it is written into.

use std::path::{Path, PathBuf};

use crate::change::ChangeSet;
use crate::config::WorkspaceConfig;
use crate::workspace::Workspace;
use prov_graph::error::Result;
use prov_graph::fs::Storage;
use prov_graph::index::IndexStore;

use super::AboutContext;

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Write the generated `about.md` and point the root at it, in one change
    /// set. Returns the path written.
    ///
    /// Unlike [`link_sidecar`](Self::link_sidecar) — which bootstraps a
    /// whole-file *record store* and leaves it alone thereafter — this rewrites
    /// the file **whole** every time, because the page is a pure function of
    /// configuration and there is nothing in it to preserve. Spec §4 calls this
    /// target kind *generated prose*: no inverse, no `part_of`, no id, not in
    /// the spanning tree, and never merged.
    ///
    /// The pointer is created if absent and left alone if present, so a
    /// workspace that has moved its page keeps it where it put it.
    pub async fn write_about(
        &self,
        root_doc: &Path,
        config: &WorkspaceConfig,
        ctx: &AboutContext,
    ) -> Result<PathBuf> {
        let page = super::generate(config, self.relations(), ctx)?;
        let path = match self.about_path(root_doc).await? {
            Some(existing) => existing,
            None => PathBuf::from(default_about_name(config.content_format)),
        };

        let mut cs = ChangeSet::new();
        cs.write(&path, page);
        // Point the root at it only when it is not already pointed at, so the
        // root's own bytes are untouched on an ordinary regeneration.
        if self.about_path(root_doc).await?.is_none()
            && let Some(pointer) = self.relations().about_relation()
        {
            let (text, doc) = self.load(root_doc).await?;
            let updated = prov_graph::edit::set_in_text(
                &text,
                doc.carrier,
                pointer,
                prov_graph::edit::infer_scalar(&path.to_string_lossy()),
            )?;
            cs.write(root_doc, updated);
        }
        cs.apply(self.fs(), self.root()).await?;
        Ok(path)
    }

    /// Remove the generated page and the root's pointer to it — the
    /// `about: structure` → `off` transition.
    ///
    /// Deleting is safe here in a way it is not anywhere else in prov: the page
    /// is derived, so nothing user-authored can be lost (spec §4 — "a pure
    /// function of configuration, therefore discardable"). It is *not* routed to
    /// the recycle bin for the same reason; a bin entry would promise a recovery
    /// worth having, and regeneration is always available instead.
    ///
    /// Returns the path removed, or `None` when there was no page to remove.
    pub async fn remove_about(&self, root_doc: &Path) -> Result<Option<PathBuf>> {
        let Some(path) = self.about_path(root_doc).await? else {
            return Ok(None);
        };
        let mut cs = ChangeSet::new();
        if self.exists(&path).await? {
            cs.remove(&path);
        }
        if let Some(pointer) = self.relations().about_relation() {
            let (text, doc) = self.load(root_doc).await?;
            let updated = prov_graph::edit::unset_in_text(&text, doc.carrier, pointer)?;
            cs.write(root_doc, updated);
        }
        cs.apply(self.fs(), self.root()).await?;
        Ok(Some(path))
    }

    /// The page prov *would* generate, beside what is on disk — the staleness
    /// question, answered without writing anything.
    ///
    /// `Ok(None)` means the page is current. `Ok(Some(diff))` carries the
    /// expected page and what is actually there (`None` when the file is
    /// missing), which is what `check` reports and `prov about --check` prints.
    ///
    /// **The comparison is over the body only.** The metadata block is excluded
    /// deliberately, and that single choice does two jobs: a content-only page
    /// (`embed_style: separate`, where there is no block at all) has nothing
    /// missing from the comparison, and `generated_by: prov <version>` never
    /// makes a workspace stale merely because prov was upgraded. A byline that
    /// names an older version is harmless; a `check` that fires in every
    /// workspace on earth after a release is not.
    pub async fn about_diff(
        &self,
        root_doc: &Path,
        config: &WorkspaceConfig,
        ctx: &AboutContext,
    ) -> Result<Option<AboutDiff>> {
        let expected = super::generate(config, self.relations(), ctx)?;
        let Some(path) = self.about_path(root_doc).await? else {
            return Ok(Some(AboutDiff {
                path: PathBuf::from(default_about_name(config.content_format)),
                expected,
                actual: None,
            }));
        };
        if !self.exists(&path).await? {
            return Ok(Some(AboutDiff {
                path,
                expected,
                actual: None,
            }));
        }
        let actual = self.read_text(&path).await?;
        if super::same_body(&actual, &expected, config.content_format) {
            return Ok(None);
        }
        Ok(Some(AboutDiff {
            path,
            expected,
            actual: Some(actual),
        }))
    }

    /// The [`Finding::AboutStale`] this workspace's generated page warrants, if
    /// any — the `check` view over [`about_diff`](Self::about_diff).
    ///
    /// Silent when the workspace asks for no page (`about: off`) *and* declares
    /// no pointer: nothing was promised, so nothing is broken. A workspace that
    /// still declares a pointer is still checked, because the pointer is a
    /// promise regardless of what the axis now says.
    ///
    /// [`Finding::AboutStale`]: crate::validate::Finding::AboutStale
    pub async fn check_about(
        &self,
        root_doc: &Path,
        config: &WorkspaceConfig,
        ctx: &AboutContext,
    ) -> Result<Option<crate::validate::Finding>> {
        let declared = self.about_path(root_doc).await?.is_some();
        if !super::enabled(config) && !declared {
            return Ok(None);
        }
        Ok(self.about_diff(root_doc, config, ctx).await?.map(|diff| {
            crate::validate::Finding::AboutStale {
                path: diff.path,
                missing: diff.actual.is_none(),
                expected: diff.expected,
            }
        }))
    }
}

/// The default filename for the generated page, in the workspace's content
/// format.
///
/// Load-bearing, and the reason it is a constant rather than a setting the user
/// is asked about: a person opening the directory finds this file *by its name*,
/// with no pointer traversal and no convention beyond being able to read. The
/// pointer may name any path — placement is ergonomic (spec §5) — but the
/// default must be the most guessable name in the most guessable place.
pub fn default_about_name(format: prov_graph::content::ContentFormat) -> String {
    format!("about.{}", format.extension())
}

/// What [`Workspace::about_diff`] found: the page prov would write, and what is
/// there instead.
#[derive(Debug, Clone)]
pub struct AboutDiff {
    /// Where the page lives (or would).
    pub path: PathBuf,
    /// The page prov would generate from the current configuration.
    pub expected: String,
    /// What is on disk, or `None` when the file is missing.
    pub actual: Option<String>,
}

// The `about` page's own findings — its staleness check and the autofix that
// repairs it. YAML fixtures, so gated like the rest of this crate's I/O tests.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::config::WorkspaceConfig;
    use crate::remedy::Fix;
    use crate::validate::Finding;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prov-about-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixture(tag: &str) -> (std::path::PathBuf, WorkspaceConfig, AboutContext) {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: T\nabout: about.md\n---\nbody\n",
        );
        let config = WorkspaceConfig::default();
        let ctx = AboutContext::new("index.md", "0.0.0");
        (dir, config, ctx)
    }

    #[test]
    fn a_current_page_is_not_a_finding() {
        let (dir, config, ctx) = fixture("about-current");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        block_on(ws.write_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        let finding =
            block_on(ws.check_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        assert!(finding.is_none(), "{finding:?}");
    }

    #[test]
    fn a_missing_page_the_pointer_promises_is_a_finding_but_not_a_broken_link() {
        let (dir, config, ctx) = fixture("about-missing");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let finding =
            block_on(ws.check_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        assert!(matches!(
            finding,
            Some(Finding::AboutStale { missing: true, .. })
        ));

        // The derived page is discardable, so a pointer at an absent one must not
        // also surface as a broken link — that would be a duplicate finding
        // inviting the wrong repair.
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::BrokenLink { target, .. } if target == "about.md")),
            "{findings:?}"
        );
    }

    #[test]
    fn a_hand_edited_page_is_stale_and_the_fix_restores_it() {
        let (dir, config, ctx) = fixture("about-edited");
        let mut ws = Workspace::builder(StdFs).root(&dir).build();
        let path =
            block_on(ws.write_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        let generated = std::fs::read_to_string(dir.join(&path)).unwrap();

        std::fs::write(
            dir.join(&path),
            generated.replace("is the root", "is definitely the root"),
        )
        .unwrap();
        let finding = block_on(ws.check_about(std::path::Path::new("index.md"), &config, &ctx))
            .unwrap()
            .expect("stale");
        assert!(matches!(
            finding,
            Finding::AboutStale { missing: false, .. }
        ));

        let fix = block_on(ws.suggest_fix(&finding)).unwrap().expect("a fix");
        assert!(matches!(fix, Fix::RegenerateAbout { .. }));
        block_on(ws.apply_fix(&fix)).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(&path)).unwrap(), generated);
    }

    #[test]
    fn a_version_bump_in_the_byline_is_not_staleness() {
        // The comparison is over the body only, so upgrading prov must not mark
        // every workspace on earth stale and rewrite files whose prose is
        // identical.
        let (dir, config, ctx) = fixture("about-version");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let path =
            block_on(ws.write_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        let page = std::fs::read_to_string(dir.join(&path)).unwrap();
        std::fs::write(
            dir.join(&path),
            page.replace("generated_by: prov 0.0.0", "generated_by: prov 99.0.0"),
        )
        .unwrap();

        let finding =
            block_on(ws.check_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        assert!(
            finding.is_none(),
            "a stale byline is not a stale page: {finding:?}"
        );
    }

    #[test]
    fn a_prov_upgrade_between_generation_and_check_is_not_staleness() {
        // The byline test above only ever tampers with the metadata line by
        // hand; it never actually varies `ctx.version`, so it would pass even
        // if the body itself leaked the version. Here the page is *generated*
        // under one version and *checked* under another — the scenario that
        // happens for real when two synced devices run different prov builds,
        // or a workspace is checked the day after an upgrade. Nothing about
        // the page's prose changed, so this must not be a finding.
        let (dir, config, old_ctx) = fixture("about-upgrade");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        block_on(ws.write_about(std::path::Path::new("index.md"), &config, &old_ctx)).unwrap();

        let new_ctx = AboutContext::new("index.md", "99.0.0");
        let finding =
            block_on(ws.check_about(std::path::Path::new("index.md"), &config, &new_ctx)).unwrap();
        assert!(
            finding.is_none(),
            "a page generated under one prov version must read as current under \
             another: {finding:?}"
        );
    }

    #[test]
    fn a_workspace_that_asked_for_no_page_is_silent() {
        let dir = tempdir("about-off");
        // No `about` pointer, and the axis is off: nothing was promised.
        write(&dir, "index.md", "---\ntitle: T\n---\nbody\n");
        let config = WorkspaceConfig {
            about: crate::config::About::Off,
            ..WorkspaceConfig::default()
        };
        let ctx = AboutContext::new("index.md", "0.0.0");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let finding =
            block_on(ws.check_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        assert!(finding.is_none(), "{finding:?}");
    }

    #[test]
    fn the_derived_page_is_never_parked_in_the_history_store() {
        // Capturing it would park a new blob on every config change to store
        // something the captured config already determines — and the first
        // capture bootstraps the store, which changes what the page says, so the
        // captured copy would be one the capture itself invalidated.
        let (dir, config, ctx) = fixture("about-not-captured");
        write(
            &dir,
            "index.md",
            "---\ntitle: T\nabout: about.md\n---\nbody\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        block_on(ws.write_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();

        let set = block_on(ws.history_capture_set(std::path::Path::new("index.md"))).unwrap();
        assert!(
            !set.iter().any(|p| p == std::path::Path::new("about.md")),
            "the derived page must stay out of the capture set: {set:?}"
        );
        // The root itself is still captured — only the derived page is excluded.
        assert!(
            set.iter().any(|p| p == std::path::Path::new("index.md")),
            "{set:?}"
        );
    }

    #[test]
    fn a_pointer_left_behind_is_still_checked_even_with_the_axis_off() {
        // Turning the axis off does not retract a promise the root still makes.
        let (dir, _, ctx) = fixture("about-off-but-pointed");
        let config = WorkspaceConfig {
            about: crate::config::About::Off,
            ..WorkspaceConfig::default()
        };
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let finding =
            block_on(ws.check_about(std::path::Path::new("index.md"), &config, &ctx)).unwrap();
        assert!(matches!(
            finding,
            Some(Finding::AboutStale { missing: true, .. })
        ));
    }
}
