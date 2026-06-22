//! Synapse themes — high-class, professional palettes for the TUI. A theme maps
//! 9 semantic roles to colors (+ a paired syntect highlight theme). Persisted to
//! `~/.synapse/config.json`; switchable live from the command palette.

use std::path::PathBuf;

use ratatui::style::Color;

#[derive(Clone)]
pub struct Theme {
    pub name: &'static str,
    pub label: &'static str,
    pub fg: Color,
    pub muted: Color,
    pub accent: Color,
    pub accent2: Color,
    pub ok: Color,
    #[allow(dead_code)] // reserved semantic color
    pub warn: Color,
    pub err: Color,
    pub selection: Color,
    pub syntect: &'static str,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

pub fn builtins() -> Vec<Theme> {
    vec![
        Theme {
            name: "deep-space",
            label: "Deep Space",
            fg: rgb(245, 247, 251),
            muted: rgb(124, 132, 153),
            accent: rgb(129, 140, 248),
            accent2: rgb(34, 211, 238),
            ok: rgb(52, 211, 153),
            warn: rgb(251, 191, 36),
            err: rgb(251, 113, 133),
            selection: rgb(40, 44, 60),
            syntect: "base16-ocean.dark",
        },
        Theme {
            name: "graphite-mono",
            label: "Graphite Mono",
            fg: rgb(232, 234, 237),
            muted: rgb(107, 114, 128),
            accent: rgb(154, 167, 184),
            accent2: rgb(192, 199, 208),
            ok: rgb(134, 184, 154),
            warn: rgb(214, 180, 106),
            err: rgb(207, 138, 135),
            selection: rgb(38, 40, 46),
            syntect: "base16-ocean.dark",
        },
        Theme {
            name: "tokyo-night",
            label: "Tokyo Night",
            fg: rgb(192, 202, 245),
            muted: rgb(86, 95, 137),
            accent: rgb(122, 162, 247),
            accent2: rgb(187, 154, 247),
            ok: rgb(158, 206, 106),
            warn: rgb(224, 175, 104),
            err: rgb(247, 118, 142),
            selection: rgb(40, 46, 66),
            syntect: "base16-eighties.dark",
        },
        Theme {
            name: "nord",
            label: "Nord",
            fg: rgb(236, 239, 244),
            muted: rgb(123, 136, 161),
            accent: rgb(136, 192, 208),
            accent2: rgb(129, 161, 193),
            ok: rgb(163, 190, 140),
            warn: rgb(235, 203, 139),
            err: rgb(191, 97, 106),
            selection: rgb(67, 76, 94),
            syntect: "base16-ocean.dark",
        },
        Theme {
            name: "paper-pro",
            label: "Paper Pro (light)",
            fg: rgb(31, 36, 48),
            muted: rgb(107, 114, 128),
            accent: rgb(47, 111, 235),
            accent2: rgb(15, 138, 138),
            ok: rgb(26, 127, 75),
            warn: rgb(154, 103, 0),
            err: rgb(179, 38, 30),
            selection: rgb(225, 228, 235),
            syntect: "InspiredGitHub",
        },
    ]
}

#[allow(dead_code)] // used by tests / external callers
pub fn names() -> Vec<&'static str> {
    builtins().into_iter().map(|t| t.name).collect()
}

pub fn by_name(name: &str) -> Option<Theme> {
    builtins().into_iter().find(|t| t.name == name)
}

pub fn default_theme() -> Theme {
    builtins().into_iter().next().unwrap()
}

// ── persistence (~/.synapse/config.json) ────────────────────────────────────

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn config_path() -> PathBuf {
    home().join(".synapse").join("config.json")
}

pub fn load_name() -> String {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("theme").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "deep-space".to_string())
}

pub fn save_name(name: &str) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Preserve any other keys already in the config.
    let mut v = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    v["theme"] = serde_json::json!(name);
    if let Ok(s) = serde_json::to_string_pretty(&v) {
        let _ = std::fs::write(&path, s);
    }
}

pub fn load() -> Theme {
    by_name(&load_name()).unwrap_or_else(default_theme)
}

/// Whether the first-run usage guideline has already been shown.
pub fn intro_seen() -> bool {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("intro_seen").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

pub fn mark_intro_seen() {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut v = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    v["intro_seen"] = serde_json::json!(true);
    if let Ok(s) = serde_json::to_string_pretty(&v) {
        let _ = std::fs::write(&path, s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_resolve_by_name() {
        assert!(by_name("tokyo-night").is_some());
        assert!(by_name("nope").is_none());
        assert_eq!(default_theme().name, "deep-space");
        assert_eq!(names().len(), 5);
    }
}
