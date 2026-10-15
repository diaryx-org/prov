//! The languages a document's metadata is written in.
//!
//! fig's core names no language (fig 6): a [`fig::Format`] is a per-process
//! handle a language crate hands back when it registers, found by name, and
//! never an enum to match on. prov reads and writes a closed set of four —
//! YAML, JSON, TOML and figl, the four `twig::meta` reads frontmatter in — so
//! it names them with its own [`MetaFormat`], which matches exhaustively, and
//! turns one into fig's handle only at the boundary where fig parses or prints.

use std::fmt;

/// A metadata language prov reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetaFormat {
    /// YAML: `---` frontmatter, `.yaml`/`.yml` files. prov's default.
    Yaml,
    /// JSON: `---json` frontmatter, `.json` files.
    Json,
    /// TOML: `+++` frontmatter, `.toml` files.
    Toml,
    /// fig's own authoring dialect: `---figl` frontmatter, `.figl` files.
    /// Named `fig` through fig 5, a spelling prov still reads.
    Figl,
}

impl MetaFormat {
    /// Every format, YAML first.
    pub const ALL: [MetaFormat; 4] = [
        MetaFormat::Yaml,
        MetaFormat::Json,
        MetaFormat::Toml,
        MetaFormat::Figl,
    ];

    /// fig's name for the format, which is also what prov writes for it: in a
    /// `---<lang>` fence, and as the `metadata.format` config value.
    pub fn name(self) -> &'static str {
        match self {
            MetaFormat::Yaml => "yaml",
            MetaFormat::Json => "json",
            MetaFormat::Toml => "toml",
            MetaFormat::Figl => "figl",
        }
    }

    /// The format a name spells: fig's name, or the older spellings prov
    /// still reads (`yml` for YAML, `fig` for figl). Case-sensitive, as the
    /// config values it reads are; `None` for anything else.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "yaml" | "yml" => MetaFormat::Yaml,
            "json" => MetaFormat::Json,
            "toml" => MetaFormat::Toml,
            "figl" | "fig" => MetaFormat::Figl,
            _ => return None,
        })
    }

    /// The format of a whole-file metadata document with extension `ext`.
    pub fn from_extension(ext: &str) -> Option<Self> {
        Self::from_name(ext)
    }

    /// The extension prov gives a whole-file document in this format.
    pub fn extension(self) -> &'static str {
        self.name()
    }

    /// fig's handle for the format, registering its language on first use.
    pub fn fig(self) -> fig::Format {
        let registered = match self {
            MetaFormat::Yaml => fig_yaml::format(),
            MetaFormat::Json => fig_json::format(),
            MetaFormat::Toml => fig_toml::format(),
            MetaFormat::Figl => fig_figl::format(),
        };
        // Registration refuses only a name already taken by another language,
        // and these are fig's own crates under their own names.
        registered.unwrap_or_else(|e| panic!("fig refused to register {}: {e}", self.name()))
    }

    /// The format a fig handle is, when it is one of prov's four. A JSON
    /// dialect (`jsonc`, `json5`) is not: prov never writes one.
    pub fn from_fig(format: fig::Format) -> Option<Self> {
        Self::from_name(format.name())
    }
}

impl fmt::Display for MetaFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_format_round_trips_through_its_name_and_fig_handle() {
        for format in MetaFormat::ALL {
            assert_eq!(MetaFormat::from_name(format.name()), Some(format));
            assert_eq!(format.fig().name(), format.name());
            assert_eq!(MetaFormat::from_fig(format.fig()), Some(format));
        }
    }

    #[test]
    fn the_older_spellings_still_read() {
        assert_eq!(MetaFormat::from_name("fig"), Some(MetaFormat::Figl));
        assert_eq!(MetaFormat::from_name("yml"), Some(MetaFormat::Yaml));
        assert_eq!(MetaFormat::from_name("zon"), None);
    }
}
