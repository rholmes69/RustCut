//! Editor view preferences that last between sessions (not part of any project),
//! kept in `prefs.json` in the per-user config folder.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Show the colour-coded song-section band (verse, chorus, bridge…) under the timeline ruler.
    /// Music videos want it; films and other long edits usually don't.
    pub show_sections: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { show_sections: true }
    }
}

fn file() -> PathBuf {
    crate::util::config_dir().join("prefs.json")
}

pub fn parse(json: &str) -> Prefs {
    serde_json::from_str(json).unwrap_or_default()
}

pub fn load() -> Prefs {
    std::fs::read_to_string(file()).map(|s| parse(&s)).unwrap_or_default()
}

/// Changes one preference and saves the file, keeping the others as they are on disk.
pub fn update(f: impl FnOnce(&mut Prefs)) {
    let mut p = load();
    f(&mut p);
    let _ = std::fs::create_dir_all(crate::util::config_dir());
    if let Ok(json) = serde_json::to_string_pretty(&p) {
        let _ = std::fs::write(file(), json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_round_trip() {
        assert!(Prefs::default().show_sections);
        // Missing or unreadable files fall back to the defaults; unknown keys are ignored.
        assert_eq!(parse(""), Prefs::default());
        assert_eq!(parse(r#"{"future_option": 3}"#), Prefs::default());
        let off = Prefs { show_sections: false };
        assert_eq!(parse(&serde_json::to_string(&off).unwrap()), off);
    }
}
