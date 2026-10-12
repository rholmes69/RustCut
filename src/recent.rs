//! The "Open recent" list: the last few projects opened or saved, most recent first,
//! kept in `recent.json` in the per-user config folder.

use std::path::{Path, PathBuf};

pub const MAX: usize = 5;

fn file() -> PathBuf {
    crate::util::config_dir().join("recent.json")
}

/// Paths compare case-insensitively on Windows and ignore `/` vs `\`.
fn same(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    norm(a) == norm(b)
}

pub fn load() -> Vec<PathBuf> {
    let list: Vec<String> = std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let mut out: Vec<PathBuf> = Vec::new();
    for p in list.into_iter().map(PathBuf::from) {
        if !out.iter().any(|q| same(q, &p)) {
            out.push(p);
        }
    }
    out.truncate(MAX);
    out
}

fn save(list: &[PathBuf]) {
    let dir = crate::util::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let strings: Vec<String> = list.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    if let Ok(json) = serde_json::to_string_pretty(&strings) {
        let _ = std::fs::write(file(), json);
    }
}

/// Moves `path` to the top of `list` (adding it if new), keeping at most [`MAX`].
pub fn push_into(list: &mut Vec<PathBuf>, path: &Path) {
    let path = std::fs::canonicalize(path)
        .map(|p| PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\")))
        .unwrap_or_else(|_| path.to_path_buf());
    list.retain(|p| !same(p, &path));
    list.insert(0, path);
    list.truncate(MAX);
}

/// Records a project as recently used and saves the list.
pub fn push(list: &mut Vec<PathBuf>, path: &Path) {
    push_into(list, path);
    save(list);
}

pub fn remove(list: &mut Vec<PathBuf>, path: &Path) {
    list.retain(|p| !same(p, path));
    save(list);
}

pub fn clear(list: &mut Vec<PathBuf>) {
    list.clear();
    save(list);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_last_five_most_recent_first() {
        let mut list = Vec::new();
        for i in 1..=7 {
            push_into(&mut list, Path::new(&format!("C:/projects/p{i}.rcproj")));
        }
        let names: Vec<String> = list.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["p7.rcproj", "p6.rcproj", "p5.rcproj", "p4.rcproj", "p3.rcproj"]);
        // Re-opening moves it to the top without duplicating.
        push_into(&mut list, Path::new("C:/projects/p5.rcproj"));
        assert_eq!(list.len(), 5);
        assert!(list[0].ends_with("p5.rcproj"));
        assert_eq!(list.iter().filter(|p| p.ends_with("p5.rcproj")).count(), 1);
    }

    #[test]
    fn path_comparison_ignores_separators() {
        assert!(same(Path::new("C:/a/b.rcproj"), Path::new("C:\\a\\b.rcproj")));
    }
}
