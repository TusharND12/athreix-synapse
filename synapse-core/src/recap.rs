//! "What did the AI just do?" — turns a window of the event log into a plain,
//! risk-flagged changelog. Offline and deterministic (an optional local model
//! could enrich the prose later). See docs/02-event-model.md.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
pub struct Recap {
    pub window_minutes: i64,
    pub total: usize,
    pub files_changed: usize,
    pub added: i64,
    pub removed: i64,
    pub by_agent: Vec<(String, usize)>,
    pub by_kind: Vec<(String, usize)>,
    pub highlights: Vec<String>,
    /// Pre-rendered human summary (one item per line).
    pub lines: Vec<String>,
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}
fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}

pub fn build(events: &[Value], window_minutes: i64) -> Recap {
    let mut by_agent: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut files: BTreeMap<String, ()> = BTreeMap::new();
    let (mut added, mut removed) = (0i64, 0i64);
    let mut highlights: Vec<String> = Vec::new();

    for e in events {
        *by_agent.entry(s(e, "agent").to_string()).or_default() += 1;
        *by_kind.entry(s(e, "kind").to_string()).or_default() += 1;
        let path = s(e, "path");
        if !path.is_empty() {
            files.insert(path.to_string(), ());
        }
        added += i(e, "added");
        removed += i(e, "removed");

        let risk = s(e, "risk");
        let kind = s(e, "kind");
        if risk == "high" || risk == "critical" {
            highlights.push(format!("⚠ {} {}", risk, s(e, "title")));
        } else if kind == "checkpoint" || kind == "approval_granted" || kind == "command" {
            highlights.push(s(e, "title").to_string());
        }
    }
    highlights.truncate(12);

    let mut agents: Vec<(String, usize)> = by_agent.into_iter().collect();
    agents.sort_by(|a, b| b.1.cmp(&a.1));
    let mut kinds: Vec<(String, usize)> = by_kind.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1));

    let mut lines = Vec::new();
    lines.push(format!("Last {window_minutes} min — {} actions", events.len()));
    lines.push(format!(
        "{} files changed   +{added} / -{removed} lines",
        files.len()
    ));
    if !agents.is_empty() {
        let who = agents
            .iter()
            .map(|(a, n)| format!("{a} ×{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("by: {who}"));
    }
    if !highlights.is_empty() {
        lines.push(String::new());
        lines.push("Highlights:".into());
        for h in &highlights {
            lines.push(format!("  • {h}"));
        }
    }
    if events.is_empty() {
        lines = vec![format!("No activity in the last {window_minutes} min.")];
    }

    Recap {
        window_minutes,
        total: events.len(),
        files_changed: files.len(),
        added,
        removed,
        by_agent: agents,
        by_kind: kinds,
        highlights,
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summarizes_a_window() {
        let events = vec![
            json!({"agent":"claude","kind":"created","title":"Created a.ts","path":"a.ts","added":10,"removed":0,"risk":"low"}),
            json!({"agent":"claude","kind":"modified","title":"Modified a.ts","path":"a.ts","added":3,"removed":1,"risk":"high"}),
            json!({"agent":"aider","kind":"modified","title":"Edited b.ts","path":"b.ts","added":2,"removed":2,"risk":"low"}),
        ];
        let r = build(&events, 30);
        assert_eq!(r.total, 3);
        assert_eq!(r.files_changed, 2);
        assert_eq!(r.added, 15);
        assert_eq!(r.removed, 3);
        assert_eq!(r.by_agent[0].0, "claude"); // most active
        assert!(r.highlights.iter().any(|h| h.contains("high")));
    }

    #[test]
    fn empty_window() {
        let r = build(&[], 10);
        assert_eq!(r.total, 0);
        assert!(r.lines[0].contains("No activity"));
    }
}
