//! Policy guardrails — a "firewall for AI agents". Rules evaluate every change;
//! `Warn` surfaces a flagged event in the timeline, `Deny` is meant to block at
//! the Surgery pre-write gate. Built-in defaults; a project may override via
//! `.synapse/policy.json`. See docs/05-security-model.md.

use serde::Serialize;

#[derive(Clone, Copy, PartialEq, Serialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Warn,
    Deny,
}

#[derive(Clone)]
pub struct Rule {
    pub name: &'static str,
    /// lowercase substring matched against the path
    pub needle: &'static str,
    /// if set, only applies to these event kinds
    pub kinds: &'static [&'static str],
    pub action: Action,
    pub message: &'static str,
}

#[derive(Serialize, Debug)]
pub struct Violation {
    pub rule: String,
    pub action: Action,
    pub message: String,
}

pub fn default_rules() -> Vec<Rule> {
    vec![
        Rule {
            name: "secrets",
            needle: ".env",
            kinds: &[],
            action: Action::Deny,
            message: "Touching a secrets file (.env) — denied by policy",
        },
        Rule {
            name: "secrets-keys",
            needle: "secret",
            kinds: &[],
            action: Action::Warn,
            message: "Change touches a secrets-related path",
        },
        Rule {
            name: "delete-sensitive",
            needle: "auth",
            kinds: &["deleted"],
            action: Action::Deny,
            message: "Deleting an auth file — denied by policy",
        },
        Rule {
            name: "payments",
            needle: "payment",
            kinds: &[],
            action: Action::Warn,
            message: "Change touches payment code — review carefully",
        },
        Rule {
            name: "migrations",
            needle: "migration",
            kinds: &[],
            action: Action::Warn,
            message: "Database migration changed — review before applying",
        },
    ]
}

/// Evaluate a change; returns the first matching violation.
pub fn evaluate(rules: &[Rule], path: &str, kind: &str, churn: i64) -> Option<Violation> {
    let p = path.to_lowercase();
    for r in rules {
        let kind_ok = r.kinds.is_empty() || r.kinds.contains(&kind);
        if kind_ok && p.contains(r.needle) {
            return Some(Violation {
                rule: r.name.to_string(),
                action: r.action,
                message: r.message.to_string(),
            });
        }
    }
    // Built-in churn guard (not path-based).
    if churn > 200 {
        return Some(Violation {
            rule: "large-change".into(),
            action: Action::Warn,
            message: format!("Large change ({churn} lines) — review"),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_is_denied() {
        let v = evaluate(&default_rules(), "config/.env", "modified", 2).unwrap();
        assert_eq!(v.action, Action::Deny);
    }

    #[test]
    fn deleting_auth_is_denied_but_editing_warns_only_if_match() {
        let del = evaluate(&default_rules(), "src/auth/login.ts", "deleted", 5).unwrap();
        assert_eq!(del.action, Action::Deny);
        // editing auth (no rule for edit) → no violation unless churn/large
        assert!(evaluate(&default_rules(), "src/auth/login.ts", "modified", 5).is_none());
    }

    #[test]
    fn large_change_warns() {
        let v = evaluate(&default_rules(), "src/app.ts", "modified", 500).unwrap();
        assert_eq!(v.action, Action::Warn);
    }

    #[test]
    fn ordinary_change_is_clean() {
        assert!(evaluate(&default_rules(), "src/Button.tsx", "modified", 10).is_none());
    }
}
