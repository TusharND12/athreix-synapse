//! Explainability engine (Feature 7). Turns a raw diff into an actionable,
//! human-readable explanation: what changed, why (best-effort), risk,
//! dependencies affected, rollback strategy, and estimated impact.
//!
//! This is the offline, rule-based engine — deterministic and always available.
//! An optional Claude API path can enrich `why`/`impact` when the user opts in
//! (see docs/05-security-model.md); the rule-based result is the floor.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Explanation {
    pub summary: String,
    pub why: String,
    pub risk: String, // low|medium|high|critical
    pub dependencies: Vec<String>,
    pub rollback: String,
    pub impact: String,
}

/// High-signal path fragments that elevate risk.
const SENSITIVE: &[&str] = &[
    "auth", "login", "session", "token", "password", "secret", "crypto",
    "payment", "billing", "stripe", "charge", "migration", "schema", "env",
    ".env", "config", "security", "permission", "acl",
];

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_sensitive(path: &str) -> bool {
    let p = path.to_lowercase();
    SENSITIVE.iter().any(|k| p.contains(k))
}

fn is_test(path: &str) -> bool {
    let p = path.to_lowercase();
    p.contains("test") || p.contains("spec") || p.contains("__tests__")
}

/// Extract imported/required modules from source text (language-agnostic best
/// effort: JS/TS `import … from '…'`, `require('…')`, Rust `use …;`, Python
/// `import …` / `from … import`).
pub fn extract_dependencies(text: &str) -> Vec<String> {
    let mut deps = Vec::new();
    // Strip a leading UTF-8 BOM (common on Windows-authored files); it isn't
    // whitespace, so without this the first `import`/`use` line wouldn't match.
    let text = text.trim_start_matches('\u{feff}');
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("import ") {
            if let Some(q) = quoted(line) {
                deps.push(q);
            } else if let Some(idx) = rest.find(" from ") {
                if let Some(q) = quoted(&rest[idx..]) {
                    deps.push(q);
                }
            } else {
                // Python `import x` or bare module.
                deps.push(rest.split_whitespace().next().unwrap_or(rest).to_string());
            }
        } else if line.starts_with("from ") {
            if let Some(idx) = line.find(" import") {
                deps.push(line[5..idx].trim().to_string());
            }
        } else if line.starts_with("use ") {
            let m = line[4..].trim_end_matches(';').trim();
            deps.push(m.to_string());
        } else if let Some(pos) = line.find("require(") {
            if let Some(q) = quoted(&line[pos..]) {
                deps.push(q);
            }
        }
    }
    deps.sort();
    deps.dedup();
    deps.truncate(12);
    deps
}

/// First single- or double-quoted substring in `s`.
fn quoted(s: &str) -> Option<String> {
    for q in ['"', '\''] {
        if let Some(start) = s.find(q) {
            if let Some(end) = s[start + 1..].find(q) {
                return Some(s[start + 1..start + 1 + end].to_string());
            }
        }
    }
    None
}

/// Produce an explanation for a change.
pub fn explain(
    path: &str,
    kind: &str, // created|modified|deleted|...
    added: u32,
    removed: u32,
    _before: Option<&str>, // reserved for the optional LLM enrichment path
    after: Option<&str>,
) -> Explanation {
    let name = file_name(path);
    let churn = added + removed;
    let sensitive = is_sensitive(path);
    let test = is_test(path);

    let risk = if kind == "deleted" && sensitive {
        "critical"
    } else if sensitive && churn > 30 {
        "high"
    } else if sensitive || churn > 120 {
        "medium"
    } else if churn > 40 {
        "medium"
    } else {
        "low"
    };

    let verb = match kind {
        "created" => "Added",
        "deleted" => "Removed",
        "renamed" => "Renamed",
        _ => "Modified",
    };
    let summary = format!("{verb} {name} (+{added}/-{removed} lines).");

    let why = if test {
        "Test/spec change — adjusts verification, not runtime behavior.".to_string()
    } else if kind == "created" {
        format!("Introduces a new file `{name}` to the project.")
    } else if kind == "deleted" {
        format!("Removes `{name}`; dependents of it may break.")
    } else if churn > 120 {
        format!("Large rewrite of `{name}` ({churn} lines touched).")
    } else {
        format!("Targeted edit to `{name}`.")
    };

    let dependencies = after.map(extract_dependencies).unwrap_or_default();

    let rollback = "Restore via the nearest Time Machine checkpoint, or revert this file to its previous snapshot (one-click).".to_string();

    let impact = if risk == "critical" || risk == "high" {
        format!(
            "Touches a sensitive area ({name}). Review carefully; {} dependency reference(s) detected.",
            dependencies.len()
        )
    } else if test {
        "Low blast radius — confined to tests.".to_string()
    } else {
        format!(
            "Localized change; {} dependency reference(s) detected.",
            dependencies.len()
        )
    };

    Explanation {
        summary,
        why,
        risk: risk.to_string(),
        dependencies,
        rollback,
        impact,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_paths_raise_risk() {
        let e = explain("src/lib/auth.ts", "modified", 50, 10, None, None);
        assert_eq!(e.risk, "high");
        let e2 = explain("src/components/Button.tsx", "modified", 5, 2, None, None);
        assert_eq!(e2.risk, "low");
    }

    #[test]
    fn deleting_sensitive_is_critical() {
        let e = explain("server/payment.ts", "deleted", 0, 80, None, None);
        assert_eq!(e.risk, "critical");
    }

    #[test]
    fn extracts_multi_language_imports() {
        let ts = "import { x } from './util';\nconst y = require(\"left-pad\");\n";
        let deps = extract_dependencies(ts);
        assert!(deps.contains(&"./util".to_string()));
        assert!(deps.contains(&"left-pad".to_string()));

        let rust = "use crate::store::Db;\n";
        assert!(extract_dependencies(rust).contains(&"crate::store::Db".to_string()));

        let py = "from os import path\n";
        assert!(extract_dependencies(py).contains(&"os".to_string()));
    }

    #[test]
    fn summary_reflects_kind_and_churn() {
        let e = explain("a/b/New.tsx", "created", 30, 0, None, None);
        assert!(e.summary.starts_with("Added New.tsx"));
    }
}
