//! Journal directory handling for the TUI (the I/O half of org-journal
//! support; date and file-name logic lives in `clignote_core::journal`).

use std::path::Path;

use clignote_core::journal::Date;

/// Dates of all daily journal files in `dir`, ascending.
pub fn list_dates(dir: &Path) -> Vec<Date> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dates: Vec<Date> = entries
        .flatten()
        .filter_map(|e| Date::from_file_name(e.file_name().to_str()?))
        .collect();
    dates.sort();
    dates
}

/// The journal date of `path`, if it is a daily file directly inside `dir`.
pub fn date_of_path(path: &Path, dir: &Path) -> Option<Date> {
    if path.parent()? != dir {
        return None;
    }
    Date::from_file_name(path.file_name()?.to_str()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_of_path_requires_journal_dir() {
        let dir = Path::new("/j");
        assert!(date_of_path(Path::new("/j/20260101.org"), dir).is_some());
        assert!(date_of_path(Path::new("/other/20260101.org"), dir).is_none());
        assert!(date_of_path(Path::new("/j/notes.org"), dir).is_none());
    }
}
