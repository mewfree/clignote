//! Journal directory handling for the TUI (the I/O half of org-journal
//! support; date and file-name logic lives in `clignote_core::journal`).

use std::path::Path;

use clignote_core::journal::{Date, FileFormat};

/// Dates of all daily journal files in `dir`, ascending.
pub fn list_dates(dir: &Path, fmt: &FileFormat) -> Vec<Date> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dates: Vec<Date> = entries
        .flatten()
        .filter_map(|e| Date::from_file_name(e.file_name().to_str()?, fmt))
        .collect();
    dates.sort();
    dates
}

/// The journal date of `path`, if it is a daily file directly inside `dir`.
pub fn date_of_path(path: &Path, dir: &Path, fmt: &FileFormat) -> Option<Date> {
    if path.parent()? != dir {
        return None;
    }
    Date::from_file_name(path.file_name()?.to_str()?, fmt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_of_path_requires_journal_dir() {
        let dir = Path::new("/j");
        let f = FileFormat::default();
        assert!(date_of_path(Path::new("/j/20260101.org"), dir, &f).is_some());
        assert!(date_of_path(Path::new("/other/20260101.org"), dir, &f).is_none());
        assert!(date_of_path(Path::new("/j/notes.org"), dir, &f).is_none());
    }
}
