use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineStatus {
    #[default]
    Unchanged,
    Added,
    Modified,
}

/// Fetch the committed (HEAD) content of `path` from git.
/// Returns `None` if the file is not tracked, the repo doesn't exist, etc.
pub fn get_head_lines(path: &Path) -> Option<Vec<String>> {
    let work_dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    // `HEAD:./filename` resolves the path relative to the current working
    // directory inside the repo, which is what we want.
    let file_name = path.file_name()?;
    let show_arg = format!("HEAD:./{}", file_name.to_string_lossy());

    let output = Command::new("git")
        .arg("show")
        .arg(&show_arg)
        .current_dir(work_dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = std::str::from_utf8(&output.stdout).ok()?;
    Some(text.lines().map(str::to_owned).collect())
}

/// Diff `current` buffer lines against `head` (committed) lines.
///
/// Returns one `LineStatus` per line in `current`:
/// - `Added`    — line did not exist in HEAD (pure insertion)
/// - `Modified` — line is part of a hunk that replaced old content
/// - `Unchanged` — line is identical in HEAD
///
/// If `head` is `None` the file is new; every line is `Added`.
pub fn diff_with_head(head: Option<&[String]>, current: &[String]) -> Vec<LineStatus> {
    let Some(head) = head else {
        return vec![LineStatus::Added; current.len()];
    };

    let old: Vec<&str> = head.iter().map(String::as_str).collect();
    let new: Vec<&str> = current.iter().map(String::as_str).collect();

    lcs_classify(&old, &new)
}

// ── LCS-based line diff ───────────────────────────────────────────────────────

fn lcs_classify(old: &[&str], new: &[&str]) -> Vec<LineStatus> {
    let m = old.len();
    let n = new.len();
    let mut result = vec![LineStatus::Unchanged; n];

    if m == 0 {
        result.fill(LineStatus::Added);
        return result;
    }
    if n == 0 {
        return result;
    }

    // Strip common prefix / suffix to shrink the LCS problem.
    let prefix = old
        .iter()
        .zip(new.iter())
        .take_while(|(a, b)| a == b)
        .count();
    if prefix == m && prefix == n {
        return result; // identical
    }
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();

    let old_mid = &old[prefix..m - suffix];
    let new_mid = &new[prefix..n - suffix];

    if old_mid.is_empty() {
        // Pure insertion — no old lines to replace.
        result[prefix..n - suffix].fill(LineStatus::Added);
        return result;
    }
    if new_mid.is_empty() {
        return result; // Pure deletion — nothing to mark.
    }

    let mm = old_mid.len();
    let mn = new_mid.len();

    // Guard against pathological cases: fall back to all-Modified when the
    // matrix would be too large (> ~250 k entries).
    if mm * mn > 250_000 {
        result[prefix..n - suffix].fill(LineStatus::Modified);
        return result;
    }

    // Build LCS DP table (working backwards) as one flat, row-major buffer.
    let stride = mn + 1;
    let mut dp = vec![0u16; (mm + 1) * stride];
    for i in (0..mm).rev() {
        for j in (0..mn).rev() {
            dp[i * stride + j] = if old_mid[i] == new_mid[j] {
                dp[(i + 1) * stride + j + 1].saturating_add(1)
            } else {
                dp[(i + 1) * stride + j].max(dp[i * stride + j + 1])
            };
        }
    }

    // Walk the edit script, grouping consecutive changes into hunks.
    let mut i = 0usize;
    let mut j = 0usize;

    while i < mm || j < mn {
        if i < mm && j < mn && old_mid[i] == new_mid[j] {
            i += 1;
            j += 1;
            continue;
        }

        // Start of a change hunk — consume until the next equal line.
        let hunk_j_start = j;
        let mut has_delete = false;
        let mut has_insert = false;

        while i < mm || j < mn {
            if i < mm && j < mn && old_mid[i] == new_mid[j] {
                break; // back to equal
            }
            if i < mm && (j >= mn || dp[(i + 1) * stride + j] >= dp[i * stride + j + 1]) {
                has_delete = true;
                i += 1;
            } else if j < mn {
                has_insert = true;
                j += 1;
            }
        }

        if has_insert {
            let status = if has_delete {
                LineStatus::Modified
            } else {
                LineStatus::Added
            };
            let lo = prefix + hunk_j_start;
            let hi = prefix + j;
            result[lo..hi].fill(status);
        }
    }

    result
}
