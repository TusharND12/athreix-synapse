//! SQLite event store (the append-only source of truth) + lightweight
//! projections. Wraps a single connection behind a mutex so both the watcher
//! thread and command handlers can use it.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;

use crate::events::SynapseEvent;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS events (
  id          TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL,
  ts          INTEGER NOT NULL,
  agent_id    TEXT NOT NULL,
  agent       TEXT NOT NULL,
  kind        TEXT NOT NULL,
  title       TEXT NOT NULL,
  path        TEXT,
  summary     TEXT,
  risk        TEXT,
  added       INTEGER,
  removed     INTEGER,
  snapshot_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);
CREATE INDEX IF NOT EXISTS idx_events_path ON events(path, ts);

CREATE TABLE IF NOT EXISTS sessions (
  id         TEXT PRIMARY KEY,
  agent      TEXT NOT NULL,
  name       TEXT NOT NULL,
  task       TEXT,
  status     TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  ended_at   INTEGER,
  tokens_in  INTEGER DEFAULT 0,
  tokens_out INTEGER DEFAULT 0,
  cost_usd   REAL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS file_stats (
  path        TEXT PRIMARY KEY,
  edits       INTEGER DEFAULT 0,
  last_ts     INTEGER,
  churn       INTEGER DEFAULT 0,
  last_blob   TEXT
);

CREATE TABLE IF NOT EXISTS checkpoints (
  id       TEXT PRIMARY KEY,
  ts       INTEGER NOT NULL,
  label    TEXT,
  tree     TEXT NOT NULL,   -- JSON: { path: blob_hash }
  auto     INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS proposals (
  id          TEXT PRIMARY KEY,
  session_id  TEXT,
  ts          INTEGER NOT NULL,
  path        TEXT NOT NULL,
  before      TEXT,
  after       TEXT,
  added       INTEGER,
  removed     INTEGER,
  status      TEXT NOT NULL,   -- pending|approved|rejected
  comment     TEXT,
  explanation TEXT             -- JSON (Explanation)
);
CREATE INDEX IF NOT EXISTS idx_proposals_status ON proposals(status, ts);
"#;

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Serialize)]
pub struct Session {
    pub id: String,
    pub agent: String,
    pub name: String,
    pub task: String,
    pub status: String,
    #[serde(rename = "filesTouched")]
    pub files_touched: i64,
    #[serde(rename = "tokensIn")]
    pub tokens_in: i64,
    #[serde(rename = "tokensOut")]
    pub tokens_out: i64,
    #[serde(rename = "costUsd")]
    pub cost_usd: f64,
    #[serde(rename = "startedAt")]
    pub started_at: i64,
}

#[derive(Serialize)]
pub struct CheckpointInfo {
    pub id: String,
    pub ts: i64,
    pub label: String,
    #[serde(rename = "fileCount")]
    pub file_count: i64,
    pub auto: bool,
}

#[derive(Serialize)]
pub struct Proposal {
    pub id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub ts: i64,
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub added: Option<i64>,
    pub removed: Option<i64>,
    pub status: String,
    pub comment: Option<String>,
    pub explanation: serde_json::Value,
}

#[derive(Serialize)]
pub struct FileStat {
    pub path: String,
    pub edits: i64,
    pub churn: i64,
    #[serde(rename = "lastTs")]
    pub last_ts: i64,
}

#[derive(Serialize)]
pub struct Health {
    #[serde(rename = "filesModified")]
    pub files_modified: i64,
    #[serde(rename = "agentsRunning")]
    pub agents_running: i64,
    pub build: String,
    pub tests: String,
    #[serde(rename = "riskScore")]
    pub risk_score: i64,
    pub coverage: i64,
    pub complexity: i64,
    #[serde(rename = "techDebt")]
    pub tech_debt: i64,
    #[serde(rename = "agentEfficiency")]
    pub agent_efficiency: i64,
    #[serde(rename = "costToday")]
    pub cost_today: f64,
}

impl Db {
    pub fn open(db_path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn ensure_session(&self, s: &Session) -> rusqlite::Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT OR IGNORE INTO sessions (id, agent, name, task, status, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![s.id, s.agent, s.name, s.task, s.status, s.started_at],
        )?;
        Ok(())
    }

    pub fn set_session_status(&self, id: &str, status: &str) -> rusqlite::Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE sessions SET status = ?2 WHERE id = ?1",
            rusqlite::params![id, status],
        )?;
        Ok(())
    }

    pub fn append_event(&self, e: &SynapseEvent) -> rusqlite::Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT OR REPLACE INTO events
               (id, session_id, ts, agent_id, agent, kind, title, path, summary, risk, added, removed, snapshot_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            rusqlite::params![
                e.id,
                e.session_id,
                e.ts,
                e.agent_id,
                e.agent.as_str(),
                e.kind.as_str(),
                e.title,
                e.path,
                e.summary,
                e.risk.as_ref().map(|r| r.as_str()),
                e.added,
                e.removed,
                e.snapshot_id,
            ],
        )?;
        Ok(())
    }

    /// Record an edit against a path and return the previous blob hash (if any).
    pub fn bump_file(&self, path: &str, ts: i64, churn: i64, blob: Option<&str>) -> Option<String> {
        let c = self.conn.lock().unwrap();
        let prev: Option<String> = c
            .query_row(
                "SELECT last_blob FROM file_stats WHERE path = ?1",
                [path],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        let _ = c.execute(
            "INSERT INTO file_stats (path, edits, last_ts, churn, last_blob)
               VALUES (?1, 1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
               edits = edits + 1,
               last_ts = ?2,
               churn = churn + ?3,
               last_blob = COALESCE(?4, last_blob)",
            rusqlite::params![path, ts, churn, blob],
        );
        prev
    }

    fn map_events(
        &self,
        order: &str,
        limit: i64,
    ) -> rusqlite::Result<Vec<serde_json::Value>> {
        let c = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT id, session_id, ts, agent_id, agent, kind, title, path, summary, risk, added, removed
             FROM events ORDER BY ts {order}, rowid {order} LIMIT ?1"
        );
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map([limit], |r| {
            Ok(serde_json::json!({
                "id": r.get::<_, String>(0)?,
                "sessionId": r.get::<_, String>(1)?,
                "ts": r.get::<_, i64>(2)?,
                "agentId": r.get::<_, String>(3)?,
                "agent": r.get::<_, String>(4)?,
                "kind": r.get::<_, String>(5)?,
                "title": r.get::<_, String>(6)?,
                "path": r.get::<_, Option<String>>(7)?,
                "summary": r.get::<_, Option<String>>(8)?,
                "risk": r.get::<_, Option<String>>(9)?,
                "added": r.get::<_, Option<i64>>(10)?,
                "removed": r.get::<_, Option<i64>>(11)?,
            }))
        })?;
        rows.collect()
    }

    /// Newest-first (for the live timeline).
    pub fn recent_events(&self, limit: i64) -> rusqlite::Result<Vec<serde_json::Value>> {
        self.map_events("DESC", limit)
    }

    /// Oldest-first (for replay playback).
    pub fn events_ascending(&self, limit: i64) -> rusqlite::Result<Vec<serde_json::Value>> {
        self.map_events("ASC", limit)
    }

    /// Events at or after a timestamp, oldest-first (for the recap).
    pub fn events_since_ts(&self, since: i64) -> rusqlite::Result<Vec<serde_json::Value>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT id, session_id, ts, agent_id, agent, kind, title, path, summary, risk, added, removed
             FROM events WHERE ts >= ?1 ORDER BY ts ASC, rowid ASC",
        )?;
        let rows = stmt.query_map([since], |r| {
            Ok(serde_json::json!({
                "ts": r.get::<_, i64>(2)?,
                "agent": r.get::<_, String>(4)?,
                "kind": r.get::<_, String>(5)?,
                "title": r.get::<_, String>(6)?,
                "path": r.get::<_, Option<String>>(7)?,
                "risk": r.get::<_, Option<String>>(9)?,
                "added": r.get::<_, Option<i64>>(10)?,
                "removed": r.get::<_, Option<i64>>(11)?,
            }))
        })?;
        rows.collect()
    }

    /// (ts, path, kind, snapshot_id) for every file event — folds into the
    /// project state at any past timestamp (the rewind / scrubber engine).
    pub fn file_versions(&self) -> rusqlite::Result<Vec<(i64, String, String, Option<String>)>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT ts, path, kind, snapshot_id FROM events
             WHERE path IS NOT NULL ORDER BY ts ASC, rowid ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?))
        })?;
        rows.collect()
    }

    /// Highest event rowid (the tail cursor for cross-process broadcast).
    pub fn max_event_rowid(&self) -> i64 {
        let c = self.conn.lock().unwrap();
        c.query_row("SELECT COALESCE(MAX(rowid),0) FROM events", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// Events with rowid greater than `after`, oldest-first — including those
    /// written by *other* processes (e.g. `synapse run`). Returns (rowid, event).
    pub fn events_since(&self, after: i64) -> rusqlite::Result<Vec<(i64, serde_json::Value)>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT rowid, id, session_id, ts, agent_id, agent, kind, title, path, summary, risk, added, removed
             FROM events WHERE rowid > ?1 ORDER BY rowid ASC LIMIT 500",
        )?;
        let rows = stmt.query_map([after], |r| {
            let rowid: i64 = r.get(0)?;
            Ok((
                rowid,
                serde_json::json!({
                    "id": r.get::<_, String>(1)?,
                    "sessionId": r.get::<_, String>(2)?,
                    "ts": r.get::<_, i64>(3)?,
                    "agentId": r.get::<_, String>(4)?,
                    "agent": r.get::<_, String>(5)?,
                    "kind": r.get::<_, String>(6)?,
                    "title": r.get::<_, String>(7)?,
                    "path": r.get::<_, Option<String>>(8)?,
                    "summary": r.get::<_, Option<String>>(9)?,
                    "risk": r.get::<_, Option<String>>(10)?,
                    "added": r.get::<_, Option<i64>>(11)?,
                    "removed": r.get::<_, Option<i64>>(12)?,
                }),
            ))
        })?;
        rows.collect()
    }

    pub fn insert_checkpoint(
        &self,
        id: &str,
        ts: i64,
        label: &str,
        tree_json: &str,
        auto: bool,
    ) -> rusqlite::Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO checkpoints (id, ts, label, tree, auto) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![id, ts, label, tree_json, auto as i64],
        )?;
        Ok(())
    }

    pub fn list_checkpoints(&self) -> rusqlite::Result<Vec<CheckpointInfo>> {
        let c = self.conn.lock().unwrap();
        let mut stmt =
            c.prepare("SELECT id, ts, COALESCE(label,''), tree, auto FROM checkpoints ORDER BY ts DESC")?;
        let rows = stmt.query_map([], |r| {
            let tree: String = r.get(3)?;
            let file_count = serde_json::from_str::<serde_json::Value>(&tree)
                .ok()
                .and_then(|v| v.as_object().map(|o| o.len() as i64))
                .unwrap_or(0);
            Ok(CheckpointInfo {
                id: r.get(0)?,
                ts: r.get(1)?,
                label: r.get(2)?,
                file_count,
                auto: r.get::<_, i64>(4)? != 0,
            })
        })?;
        rows.collect()
    }

    /// Returns the `path -> blob_hash` tree JSON for a checkpoint.
    pub fn get_checkpoint_tree(&self, id: &str) -> rusqlite::Result<Option<String>> {
        let c = self.conn.lock().unwrap();
        c.query_row("SELECT tree FROM checkpoints WHERE id = ?1", [id], |r| r.get(0))
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
    }

    // ── Surgery Mode proposals ────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    pub fn insert_proposal(
        &self,
        id: &str,
        session_id: &str,
        ts: i64,
        path: &str,
        before: Option<&str>,
        after: Option<&str>,
        added: i64,
        removed: i64,
        explanation_json: &str,
    ) -> rusqlite::Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO proposals (id, session_id, ts, path, before, after, added, removed, status, comment, explanation)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'pending',NULL,?9)",
            rusqlite::params![id, session_id, ts, path, before, after, added, removed, explanation_json],
        )?;
        Ok(())
    }

    pub fn get_proposal_status(&self, id: &str) -> Option<String> {
        let c = self.conn.lock().unwrap();
        c.query_row("SELECT status FROM proposals WHERE id = ?1", [id], |r| r.get(0))
            .ok()
    }

    pub fn proposal_path(&self, id: &str) -> Option<String> {
        let c = self.conn.lock().unwrap();
        c.query_row("SELECT path FROM proposals WHERE id = ?1", [id], |r| r.get(0))
            .ok()
    }

    pub fn decide_proposal(
        &self,
        id: &str,
        approved: bool,
        comment: Option<&str>,
    ) -> rusqlite::Result<usize> {
        let status = if approved { "approved" } else { "rejected" };
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE proposals SET status = ?2, comment = ?3 WHERE id = ?1 AND status = 'pending'",
            rusqlite::params![id, status, comment],
        )
    }

    pub fn list_proposals(&self, only_pending: bool) -> rusqlite::Result<Vec<Proposal>> {
        let c = self.conn.lock().unwrap();
        let sql = if only_pending {
            "SELECT id, COALESCE(session_id,''), ts, path, before, after, added, removed, status, comment, explanation
             FROM proposals WHERE status = 'pending' ORDER BY ts DESC"
        } else {
            "SELECT id, COALESCE(session_id,''), ts, path, before, after, added, removed, status, comment, explanation
             FROM proposals ORDER BY ts DESC LIMIT 100"
        };
        let mut stmt = c.prepare(sql)?;
        let rows = stmt.query_map([], |r| {
            let expl: Option<String> = r.get(10)?;
            Ok(Proposal {
                id: r.get(0)?,
                session_id: r.get(1)?,
                ts: r.get(2)?,
                path: r.get(3)?,
                before: r.get(4)?,
                after: r.get(5)?,
                added: r.get(6)?,
                removed: r.get(7)?,
                status: r.get(8)?,
                comment: r.get(9)?,
                explanation: expl
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::Value::Null),
            })
        })?;
        rows.collect()
    }

    pub fn list_sessions(&self) -> rusqlite::Result<Vec<Session>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT s.id, s.agent, s.name, COALESCE(s.task,''), s.status,
                    (SELECT COUNT(DISTINCT path) FROM events WHERE session_id = s.id AND path IS NOT NULL),
                    s.tokens_in, s.tokens_out, s.cost_usd, s.started_at
             FROM sessions s ORDER BY s.started_at ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Session {
                id: r.get(0)?,
                agent: r.get(1)?,
                name: r.get(2)?,
                task: r.get(3)?,
                status: r.get(4)?,
                files_touched: r.get(5)?,
                tokens_in: r.get(6)?,
                tokens_out: r.get(7)?,
                cost_usd: r.get(8)?,
                started_at: r.get(9)?,
            })
        })?;
        rows.collect()
    }

    /// Heatmap source — files by activity (hottest first).
    pub fn list_file_stats(&self, limit: i64) -> rusqlite::Result<Vec<FileStat>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT path, edits, churn, COALESCE(last_ts,0)
             FROM file_stats ORDER BY edits DESC, churn DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok(FileStat {
                path: r.get(0)?,
                edits: r.get(1)?,
                churn: r.get(2)?,
                last_ts: r.get(3)?,
            })
        })?;
        rows.collect()
    }

    pub fn health(&self) -> rusqlite::Result<Health> {
        let c = self.conn.lock().unwrap();
        let files_modified: i64 = c.query_row(
            "SELECT COUNT(*) FROM events WHERE kind IN ('created','modified','deleted')",
            [],
            |r| r.get(0),
        )?;
        let agents_running: i64 =
            c.query_row("SELECT COUNT(*) FROM sessions WHERE status = 'active'", [], |r| {
                r.get(0)
            })?;
        let distinct_files: i64 = c.query_row(
            "SELECT COUNT(*) FROM file_stats",
            [],
            |r| r.get(0),
        )?;
        // Phase 1 heuristics — replaced with real signals as later phases land.
        let complexity = (distinct_files.min(100)) as i64;
        let risk_score = (files_modified % 40) as i64;
        Ok(Health {
            files_modified,
            agents_running: agents_running.max(0),
            build: "unknown".into(),
            tests: "unknown".into(),
            risk_score,
            coverage: 0,
            complexity,
            tech_debt: 0,
            agent_efficiency: 0,
            cost_today: 0.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AgentKind, EventKind, RiskLevel, SynapseEvent};

    fn temp_db() -> Db {
        let dir = std::env::temp_dir().join(format!("syn-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("t.db")).unwrap()
    }

    fn ev(id: &str, kind: EventKind, path: Option<&str>) -> SynapseEvent {
        SynapseEvent {
            id: id.into(),
            session_id: "workspace".into(),
            ts: 1,
            agent_id: "workspace".into(),
            agent: AgentKind::Human,
            kind,
            title: "t".into(),
            path: path.map(|p| p.into()),
            summary: None,
            risk: Some(RiskLevel::Low),
            added: Some(3),
            removed: Some(1),
            snapshot_id: None,
        }
    }

    fn session() -> Session {
        Session {
            id: "workspace".into(),
            agent: "human".into(),
            name: "Workspace".into(),
            task: "t".into(),
            status: "active".into(),
            files_touched: 0,
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: 0.0,
            started_at: 0,
        }
    }

    #[test]
    fn append_query_and_health() {
        let db = temp_db();
        db.ensure_session(&session()).unwrap();
        db.append_event(&ev("e1", EventKind::Created, Some("a.ts"))).unwrap();
        db.append_event(&ev("e2", EventKind::Modified, Some("a.ts"))).unwrap();
        db.append_event(&ev("e3", EventKind::Reasoning, None)).unwrap();

        let events = db.recent_events(10).unwrap();
        assert_eq!(events.len(), 3);
        // recent_events is newest-first.
        assert_eq!(events[0]["id"], "e3");

        let h = db.health().unwrap();
        assert_eq!(h.files_modified, 2, "created+modified count as file changes");
        assert_eq!(h.agents_running, 1, "one active session");
    }

    #[test]
    fn bump_file_returns_previous_blob() {
        let db = temp_db();
        assert_eq!(db.bump_file("a.ts", 1, 0, Some("hashA")), None);
        assert_eq!(
            db.bump_file("a.ts", 2, 0, Some("hashB")),
            Some("hashA".to_string()),
            "second edit sees the first snapshot as previous"
        );
    }

    #[test]
    fn proposal_decision_flow() {
        let db = temp_db();
        db.insert_proposal("p1", "s", 1, "auth.ts", Some("old"), Some("new"), 1, 1, "{}")
            .unwrap();
        assert_eq!(db.get_proposal_status("p1").as_deref(), Some("pending"));
        assert_eq!(db.list_proposals(true).unwrap().len(), 1);

        let n = db.decide_proposal("p1", true, Some("looks good")).unwrap();
        assert_eq!(n, 1, "one row updated");
        assert_eq!(db.get_proposal_status("p1").as_deref(), Some("approved"));
        assert_eq!(db.list_proposals(true).unwrap().len(), 0, "no longer pending");

        // Deciding again is a no-op (already decided).
        assert_eq!(db.decide_proposal("p1", false, None).unwrap(), 0);
    }
}
