//! Fuzzy file picker (`SPC f f`, `SPC SPC`): project file discovery,
//! subsequence scoring and the picker's selection state.

use std::path::{Path, PathBuf};

/// Upper bound on files collected, so a huge tree can't stall the UI.
const MAX_FILES: usize = 20_000;

/// Directories never worth descending into (hidden ones are skipped too).
const SKIP_DIRS: &[&str] = &["target", "node_modules"];

/// Nearest ancestor of `start` (inclusive) containing `.git`, else `start`.
pub fn project_root(start: &Path) -> PathBuf {
    start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(start)
        .to_path_buf()
}

/// All `.org` files under `root`, as sorted paths relative to `root`.
/// Hidden entries and `SKIP_DIRS` are ignored; symlinked directories are
/// not followed.
pub fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(path);
                }
            } else if name.ends_with(".org") && path.is_file() {
                if let Ok(rel) = path.strip_prefix(root) {
                    files.push(rel.to_path_buf());
                }
            }
        }
        if files.len() >= MAX_FILES {
            break;
        }
    }
    files.sort();
    files
}

/// Score `query` against `text` as a case-insensitive subsequence (smart
/// case: an uppercase letter in the query makes it case-sensitive).
/// Higher is better; `None` when the query isn't a subsequence.
fn score_subsequence(query: &[char], text: &[char], smart_case: bool) -> Option<i32> {
    let eq = |q: char, t: char| {
        if smart_case {
            q == t
        } else {
            q.to_lowercase().eq(t.to_lowercase())
        }
    };
    let mut score = 0;
    let mut ti = 0;
    let mut prev_match: Option<usize> = None;
    for &q in query {
        let found = (ti..text.len()).find(|&i| eq(q, text[i]))?;
        let at_boundary = found == 0 || matches!(text[found - 1], '/' | '_' | '-' | '.' | ' ');
        score += 1;
        if at_boundary {
            score += 8;
        }
        match prev_match {
            Some(p) if found == p + 1 => score += 10,
            Some(p) => score -= (found - p - 1).min(8) as i32,
            None => score -= found.min(8) as i32,
        }
        prev_match = Some(found);
        ti = found + 1;
    }
    Some(score)
}

/// Fuzzy score of `query` against a relative path. Matching inside the file
/// name is preferred over matching across directories.
pub fn fuzzy_score(query: &str, path: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().collect();
    let smart_case = q.iter().any(|c| c.is_uppercase());
    let full: Vec<char> = path.chars().collect();
    let name_start = path.rfind('/').map_or(0, |i| path[..=i].chars().count());
    let in_name = score_subsequence(&q, &full[name_start..], smart_case).map(|s| s + 20);
    let in_path = score_subsequence(&q, &full, smart_case);
    in_name.into_iter().chain(in_path).max()
}

// ── State ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct Picker {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
    pub query: String,
    /// Indices into `files`, best match first.
    pub results: Vec<usize>,
    pub selected: usize,
    /// First visible result (vertical scroll), kept in sync by the renderer.
    pub scroll: usize,
}

impl Picker {
    pub fn open(root: PathBuf) -> Self {
        let files = collect_files(&root);
        let mut picker = Self {
            root,
            files,
            ..Self::default()
        };
        picker.refilter();
        picker
    }

    pub fn refilter(&mut self) {
        let mut scored: Vec<(i32, usize)> = self
            .files
            .iter()
            .enumerate()
            .filter_map(|(i, p)| fuzzy_score(&self.query, &p.to_string_lossy()).map(|s| (s, i)))
            .collect();
        // Best score first; ties keep the (alphabetical) file order, shorter first.
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| {
                let (la, lb) = (
                    self.files[a.1].as_os_str().len(),
                    self.files[b.1].as_os_str().len(),
                );
                la.cmp(&lb).then(a.1.cmp(&b.1))
            })
        });
        self.results = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn push(&mut self, c: char) {
        self.query.push(c);
        self.refilter();
    }

    pub fn pop(&mut self) {
        if self.query.pop().is_some() {
            self.refilter();
        }
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.refilter();
    }

    pub fn move_down(&mut self) {
        self.selected = (self.selected + 1).min(self.results.len().saturating_sub(1));
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Path (relative to `root`) of the selected result.
    pub fn selected_path(&self) -> Option<&Path> {
        let idx = *self.results.get(self.selected)?;
        Some(self.files[idx].as_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker_with(files: &[&str]) -> Picker {
        let mut p = Picker {
            files: files.iter().map(PathBuf::from).collect(),
            ..Picker::default()
        };
        p.refilter();
        p
    }

    fn ranked(p: &Picker) -> Vec<String> {
        p.results
            .iter()
            .map(|&i| p.files[i].to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn non_subsequence_does_not_match() {
        assert!(fuzzy_score("xyz", "notes/todo.org").is_none());
        assert!(fuzzy_score("odt", "todo.org").is_none());
    }

    #[test]
    fn empty_query_matches_everything() {
        let p = picker_with(&["b.org", "a.org"]);
        assert_eq!(ranked(&p), ["b.org", "a.org"]);
    }

    #[test]
    fn matching_is_case_insensitive_unless_query_has_uppercase() {
        assert!(fuzzy_score("todo", "TODO.org").is_some());
        assert!(fuzzy_score("TODO", "todo.org").is_none());
        assert!(fuzzy_score("TODO", "TODO.org").is_some());
    }

    #[test]
    fn file_name_match_beats_directory_match() {
        let mut p = picker_with(&["projects/work/inbox.org", "work.org"]);
        p.push('w');
        p.push('o');
        p.push('r');
        p.push('k');
        assert_eq!(ranked(&p)[0], "work.org");
    }

    #[test]
    fn consecutive_and_boundary_matches_rank_higher() {
        let mut p = picker_with(&["a_x_p_p.org", "app.org"]);
        for c in "app".chars() {
            p.push(c);
        }
        assert_eq!(ranked(&p), ["app.org", "a_x_p_p.org"]);
    }

    #[test]
    fn filtering_drops_non_matches_and_resets_selection() {
        let mut p = picker_with(&["one.org", "two.org", "three.org"]);
        p.move_down();
        assert_eq!(p.selected, 1);
        p.push('t');
        assert_eq!(p.selected, 0);
        assert_eq!(p.results.len(), 2);
        p.pop();
        assert_eq!(p.results.len(), 3);
    }

    #[test]
    fn selection_is_clamped() {
        let mut p = picker_with(&["a.org", "b.org"]);
        p.move_up();
        assert_eq!(p.selected, 0);
        p.move_down();
        p.move_down();
        assert_eq!(p.selected, 1);
    }

    #[test]
    fn collect_files_skips_hidden_and_non_org() {
        let root = std::env::temp_dir().join(format!("cln-picker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["notes", ".hidden", "target"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in [
            "a.org",
            "notes/b.org",
            "notes/c.txt",
            ".hidden/d.org",
            "target/e.org",
            ".f.org",
        ] {
            std::fs::write(root.join(file), "").unwrap();
        }
        let files = collect_files(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            files,
            [PathBuf::from("a.org"), PathBuf::from("notes/b.org")]
        );
    }
}
