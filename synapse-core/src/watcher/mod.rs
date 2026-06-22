//! Universal file watcher (Tier-1 agent adapter).
//!
//! Works for *any* agent: it observes the working tree via `notify`, snapshots
//! each new file version into the content-addressed store, computes real line
//! diffs against the previous version, appends a `SynapseEvent`, and streams it
//! to the UI on `synapse://event`. No per-agent integration required.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, FileIdMap};
use similar::{ChangeTag, TextDiff};

use crate::agents::AgentRegistry;
use crate::events::{AgentKind, EventKind, RiskLevel, SynapseEvent};
use crate::policy;
use crate::snapshots::Snapshots;
use crate::store::Db;

pub type SynapseWatcher = Debouncer<RecommendedWatcher, FileIdMap>;

/// Where finished events are delivered. The watcher persists every event to the
/// store itself; the sink handles *delivery* (GUI emit, terminal print, TUI
/// channel, …) so the engine stays front-end agnostic and fully offline.
pub type EventSink = Arc<dyn Fn(&SynapseEvent) + Send + Sync>;

pub const IGNORED: &[&str] = &[
    ".git", ".synapse", "node_modules", "target", ".next", "out", "dist",
    "build", ".cache", ".turbo", ".vercel", "coverage",
];

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn is_ignored(path: &Path, root: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|s| IGNORED.contains(&s))
            .unwrap_or(false)
    })
}

fn rel_string(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into())
}

fn line_diff(old: &str, new: &str) -> (u32, u32) {
    let diff = TextDiff::from_lines(old, new);
    let mut added = 0u32;
    let mut removed = 0u32;
    for ch in diff.iter_all_changes() {
        match ch.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    (added, removed)
}

/// Classify a notify event into a Synapse `EventKind`, or `None` to skip.
fn classify(kind: &notify::EventKind) -> Option<EventKind> {
    use notify::event::{CreateKind, ModifyKind, RemoveKind};
    match kind {
        notify::EventKind::Create(CreateKind::File | CreateKind::Any) => Some(EventKind::Created),
        notify::EventKind::Modify(ModifyKind::Name(_)) => Some(EventKind::Renamed),
        notify::EventKind::Modify(_) => Some(EventKind::Modified),
        notify::EventKind::Remove(RemoveKind::File | RemoveKind::Any) => Some(EventKind::Deleted),
        _ => None,
    }
}

/// Detect a meaningful git operation from a `.git/` path change.
fn git_op(path: &Path, root: &Path) -> Option<(EventKind, String)> {
    let rel = rel_string(path, root);
    if !rel.contains(".git/") && !rel.ends_with(".git") {
        return None;
    }
    let name = file_name(path);
    if name == "COMMIT_EDITMSG" {
        Some((EventKind::Command, "Git: commit created".into()))
    } else if name == "MERGE_HEAD" {
        Some((EventKind::Command, "Git: merge in progress".into()))
    } else if rel.contains(".git/refs/heads/") {
        Some((EventKind::Command, format!("Git: branch updated ({name})")))
    } else if rel.ends_with(".git/HEAD") {
        Some((EventKind::Command, "Git: HEAD moved (checkout/commit)".into()))
    } else {
        None // ignore the rest of .git's churn
    }
}

#[allow(clippy::too_many_arguments)]
fn process_path(
    path: &PathBuf,
    kind: EventKind,
    root: &Path,
    db: &Db,
    snaps: &Snapshots,
    sink: &EventSink,
    session_id: &str,
    agents: &AgentRegistry,
) {
    // Resolve which agent is currently active in this project (ambient detection).
    let agent = agents
        .lock()
        .unwrap()
        .get(&root.to_string_lossy().to_string())
        .copied()
        .unwrap_or(AgentKind::Human);

    // Git operations are surfaced even though the rest of .git is ignored.
    if let Some((gkind, title)) = git_op(path, root) {
        let ev = SynapseEvent {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            ts: now_ms(),
            agent_id: session_id.to_string(),
            agent,
            kind: gkind,
            title,
            path: None,
            summary: None,
            risk: None,
            added: None,
            removed: None,
            snapshot_id: None,
        };
        let _ = db.append_event(&ev);
        sink(&ev);
        return;
    }

    if is_ignored(path, root) {
        return;
    }
    // Directories produce their own child events; skip the dir entry itself.
    if matches!(kind, EventKind::Created | EventKind::Modified) && path.is_dir() {
        return;
    }

    let ts = now_ms();
    let rel = rel_string(path, root);
    let name = file_name(path);

    let (added, removed, snapshot_hash) = match kind {
        EventKind::Created | EventKind::Modified => {
            let Ok(bytes) = std::fs::read(path) else {
                return;
            };
            let hash = snaps.write_blob(&bytes).ok();
            let prev = db.bump_file(
                &rel,
                ts,
                0,
                hash.as_deref(),
            );
            // Real line diff against the previous snapshot, when both are text.
            let new_text = String::from_utf8(bytes).ok();
            let diff = match (prev.as_deref().and_then(|h| snaps.read_text(h)), new_text) {
                (Some(old), Some(new)) => Some(line_diff(&old, &new)),
                (None, Some(new)) => Some((new.lines().count() as u32, 0)),
                _ => None,
            };
            match diff {
                Some((a, r)) => (Some(a), Some(r), hash),
                None => (None, None, hash),
            }
        }
        EventKind::Deleted => {
            db.bump_file(&rel, ts, 0, None);
            (None, None, None)
        }
        _ => (None, None, None),
    };

    let churn = added.unwrap_or(0) as i64 + removed.unwrap_or(0) as i64;
    let risk = if churn > 120 {
        Some(RiskLevel::High)
    } else if churn > 40 {
        Some(RiskLevel::Medium)
    } else {
        Some(RiskLevel::Low)
    };

    let verb = match kind {
        EventKind::Created => "Created",
        EventKind::Modified => "Modified",
        EventKind::Deleted => "Deleted",
        EventKind::Renamed => "Renamed",
        _ => "Changed",
    };

    let event = SynapseEvent {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        ts,
        agent_id: session_id.to_string(),
        agent,
        kind,
        title: format!("{verb} {name}"),
        path: Some(rel),
        summary: None,
        risk,
        added,
        removed,
        snapshot_id: snapshot_hash,
    };

    let _ = db.append_event(&event);
    sink(&event);

    // Policy guardrails: surface warnings/denials live in the timeline.
    if let Some(p) = &event.path {
        if let Some(v) = policy::evaluate(&policy::default_rules(), p, event.kind.as_str(), churn) {
            let flag = SynapseEvent {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                ts: now_ms(),
                agent_id: session_id.to_string(),
                agent,
                kind: EventKind::ApprovalRequested,
                title: format!("Policy [{:?}]: {}", v.action, v.message),
                path: Some(p.clone()),
                summary: None,
                risk: Some(RiskLevel::High),
                added: None,
                removed: None,
                snapshot_id: None,
            };
            let _ = db.append_event(&flag);
            sink(&flag);
        }
    }
}

/// Begin watching `root`. Keep the returned watcher alive for the session.
pub fn start(
    root: PathBuf,
    db: Db,
    snaps: Snapshots,
    sink: EventSink,
    session_id: String,
    agents: AgentRegistry,
) -> Result<SynapseWatcher, Box<dyn std::error::Error>> {
    let handler_root = root.clone();
    let mut debouncer = new_debouncer(
        Duration::from_millis(400),
        None,
        move |result: DebounceEventResult| {
            let events = match result {
                Ok(events) => events,
                Err(_) => return,
            };
            for ev in events {
                let Some(kind) = classify(&ev.event.kind) else {
                    continue;
                };
                for path in &ev.event.paths {
                    process_path(
                        path,
                        kind.clone(),
                        &handler_root,
                        &db,
                        &snaps,
                        &sink,
                        &session_id,
                        &agents,
                    );
                }
            }
        },
    )?;

    debouncer
        .watcher()
        .watch(&root, RecursiveMode::Recursive)?;
    Ok(debouncer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn line_diff_counts_insertions_and_deletions() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\nd\n"; // 'b'→'B' (1 del + 1 ins), plus 'd' (1 ins)
        let (added, removed) = line_diff(old, new);
        assert_eq!(added, 2);
        assert_eq!(removed, 1);
    }

    #[test]
    fn ignores_build_artifacts() {
        let root = Path::new("/proj");
        assert!(is_ignored(Path::new("/proj/node_modules/x/y.js"), root));
        assert!(is_ignored(Path::new("/proj/.git/HEAD"), root));
        assert!(is_ignored(Path::new("/proj/.synapse/synapse.db"), root));
        assert!(!is_ignored(Path::new("/proj/src/app.ts"), root));
    }

    #[test]
    fn detects_git_operations() {
        let root = Path::new("/proj");
        assert!(git_op(Path::new("/proj/.git/COMMIT_EDITMSG"), root).is_some());
        assert!(git_op(Path::new("/proj/.git/refs/heads/main"), root).is_some());
        assert!(git_op(Path::new("/proj/.git/HEAD"), root).is_some());
        // Ordinary .git churn is not surfaced.
        assert!(git_op(Path::new("/proj/.git/objects/ab/cdef"), root).is_none());
        // Non-git path → not a git op.
        assert!(git_op(Path::new("/proj/src/app.ts"), root).is_none());
    }

    #[test]
    fn classifies_notify_kinds() {
        use notify::event::CreateKind;
        assert!(matches!(
            classify(&notify::EventKind::Create(CreateKind::File)),
            Some(EventKind::Created)
        ));
        assert!(classify(&notify::EventKind::Access(notify::event::AccessKind::Read)).is_none());
    }
}
