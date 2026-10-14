//! User configuration: `$XDG_CONFIG_HOME/clignote/config.toml`
//! (falling back to `~/.config/clignote/config.toml`).
//!
//! ```toml
//! [journal]
//! dir = "~/notes/journal"
//! file_format = "%Y-%m-%d"   # default "%Y%m%d"; ".org" is appended
//! entry_level = 1            # 1 = "* 14:05", 2 = "** 14:05" (default 1)
//! blank_line_above = true    # blank line before a new entry (default true)
//!
//! [editor]
//! line_numbers = "relative"  # "off", "absolute" or "relative" (default)
//! ```
//!
//! Environment overrides: `CLN_JOURNAL_DIR`.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clignote_core::journal::FileFormat;
use serde::Deserialize;

use crate::app::LineNumbers;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    journal: RawJournal,
    #[serde(default)]
    editor: RawEditor,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEditor {
    line_numbers: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJournal {
    dir: Option<String>,
    file_format: Option<String>,
    entry_level: Option<usize>,
    blank_line_above: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Directory holding daily journal files; `None` when no home is known.
    pub journal_dir: Option<PathBuf>,
    /// Pattern for daily journal file names.
    pub journal_file_format: FileFormat,
    /// Heading level (number of stars) of a new timed entry.
    pub journal_entry_level: usize,
    /// Whether a new entry is preceded by a blank line.
    pub journal_blank_above: bool,
    /// Initial line-number mode.
    pub line_numbers: LineNumbers,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            journal_dir: None,
            journal_file_format: FileFormat::default(),
            journal_entry_level: 1,
            journal_blank_above: true,
            line_numbers: LineNumbers::Relative,
        }
    }
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
        let defaults = Self::default();
        let journal_entry_level = match raw.journal.entry_level {
            None => defaults.journal_entry_level,
            Some(n @ 1..=6) => n,
            Some(_) => anyhow::bail!("entry_level must be between 1 and 6"),
        };
        let line_numbers = match raw.editor.line_numbers.as_deref() {
            None => defaults.line_numbers,
            Some(v) => LineNumbers::parse(v).ok_or_else(|| {
                anyhow::anyhow!("line_numbers must be \"off\", \"absolute\" or \"relative\"")
            })?,
        };
        Ok(Self {
            journal_dir: raw.journal.dir.map(|d| expand_tilde(&d, home)),
            journal_file_format,
            journal_entry_level,
            journal_blank_above: raw
                .journal
                .blank_line_above
                .unwrap_or(defaults.journal_blank_above),
            line_numbers,
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
    fn line_numbers_default_relative_and_configurable() {
        assert_eq!(Config::default().line_numbers, LineNumbers::Relative);
        let c = Config::parse("[editor]\nline_numbers = \"off\"\n", None).unwrap();
        assert_eq!(c.line_numbers, LineNumbers::Off);
        assert!(Config::parse("[editor]\nline_numbers = \"x\"\n", None).is_err());
    }

    #[test]
    fn journal_entry_style_is_configurable() {
        let d = Config::default();
        assert_eq!((d.journal_entry_level, d.journal_blank_above), (1, true));
        let c = Config::parse(
            "[journal]\nentry_level = 2\nblank_line_above = false\n",
            None,
        )
        .unwrap();
        assert_eq!((c.journal_entry_level, c.journal_blank_above), (2, false));
        assert!(Config::parse("[journal]\nentry_level = 0\n", None).is_err());
        assert!(Config::parse("[journal]\nentry_level = 7\n", None).is_err());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("[journal]\ndr = \"x\"\n", None).is_err());
        assert!(Config::parse("bogus = 1\n", None).is_err());
    }
}
