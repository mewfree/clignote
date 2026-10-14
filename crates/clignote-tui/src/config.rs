//! User configuration: `$XDG_CONFIG_HOME/clignote/config.toml`
//! (falling back to `~/.config/clignote/config.toml`).
//!
//! ```toml
//! [journal]
//! dir = "~/notes/journal"
//! file_format = "%Y-%m-%d"   # default "%Y%m%d"; ".org" is appended
//! ```
//!
//! Environment overrides: `CLN_JOURNAL_DIR`.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clignote_core::journal::FileFormat;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    journal: RawJournal,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJournal {
    dir: Option<String>,
    file_format: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Directory holding daily journal files; `None` when no home is known.
    pub journal_dir: Option<PathBuf>,
    /// Pattern for daily journal file names.
    pub journal_file_format: FileFormat,
}

/// Replace a leading `~` or `~/` with `home`.
fn expand_tilde(raw: &str, home: Option<&Path>) -> PathBuf {
    match (raw.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        (None, Some(home)) if raw == "~" => home.to_path_buf(),
        _ => PathBuf::from(raw),
    }
}

impl Config {
    /// Parse config file text. Unknown keys are an error so typos surface.
    pub fn parse(text: &str, home: Option<&Path>) -> anyhow::Result<Self> {
        let raw: RawConfig = toml::from_str(text)?;
        let journal_file_format = match raw.journal.file_format {
            Some(p) => FileFormat::parse(&p).map_err(|e| anyhow::anyhow!(e))?,
            None => FileFormat::default(),
        };
        Ok(Self {
            journal_dir: raw.journal.dir.map(|d| expand_tilde(&d, home)),
            journal_file_format,
        })
    }

    fn file_path(home: Option<&Path>) -> Option<PathBuf> {
        match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            Some(xdg) => Some(PathBuf::from(xdg).join("clignote/config.toml")),
            None => Some(home?.join(".config/clignote/config.toml")),
        }
    }

    /// Load the config file (a missing file is fine), then apply environment
    /// overrides and defaults. A broken file yields the defaults plus a
    /// warning to show the user.
    pub fn load() -> (Self, Option<String>) {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut warning = None;
        let mut config = Self::default();

        if let Some(path) = Self::file_path(home.as_deref()) {
            match std::fs::read_to_string(&path) {
                Ok(text) => match Self::parse(&text, home.as_deref())
                    .with_context(|| format!("{}", path.display()))
                {
                    Ok(c) => config = c,
                    Err(e) => warning = Some(format!("Config error: {e:#}")),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warning = Some(format!("Cannot read {}: {e}", path.display())),
            }
        }

        if let Some(dir) = std::env::var_os("CLN_JOURNAL_DIR") {
            config.journal_dir = Some(PathBuf::from(dir));
        }
        if config.journal_dir.is_none() {
            config.journal_dir = home.map(|h| h.join("org").join("journal"));
        }
        (config, warning)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_has_no_overrides() {
        assert_eq!(Config::parse("", None).unwrap(), Config::default());
    }

    #[test]
    fn journal_dir_expands_tilde() {
        let home = Path::new("/home/me");
        let c = Config::parse("[journal]\ndir = \"~/notes/j\"\n", Some(home)).unwrap();
        assert_eq!(c.journal_dir, Some(PathBuf::from("/home/me/notes/j")));
        let c = Config::parse("[journal]\ndir = \"/abs\"\n", Some(home)).unwrap();
        assert_eq!(c.journal_dir, Some(PathBuf::from("/abs")));
    }

    #[test]
    fn journal_file_format_parses() {
        let c = Config::parse("[journal]\nfile_format = \"%Y-%m-%d\"\n", None).unwrap();
        assert_eq!(
            c.journal_file_format,
            FileFormat::parse("%Y-%m-%d").unwrap()
        );
        assert!(Config::parse("[journal]\nfile_format = \"%Y\"\n", None).is_err());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("[journal]\ndr = \"x\"\n", None).is_err());
        assert!(Config::parse("bogus = 1\n", None).is_err());
    }
}
