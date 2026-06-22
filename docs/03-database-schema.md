# 03 — Database & Snapshot Schema

Local-first. Two stores live under a hidden `.synapse/` folder in each project
(canonical schema: `synapse-core/src/store/mod.rs`):

- `.synapse/synapse.db` — **SQLite** (via `rusqlite`, bundled): the event log + projections.
- `.synapse/snapshots/blobs/` — a **pure-Rust content-addressed store** (SHA-256
  keys + zlib), independent of the user's own git. The Time Machine backend.

## SQLite tables

```sql
-- Append-only event log (THE source of truth)
CREATE TABLE events (
  id          TEXT PRIMARY KEY,         -- uuid
  session_id  TEXT NOT NULL,
  ts          INTEGER NOT NULL,         -- epoch millis
  agent_id    TEXT NOT NULL,
  agent       TEXT NOT NULL,            -- claude|gpt|gemini|aider|cursor|human
  kind        TEXT NOT NULL,            -- created|modified|deleted|…|checkpoint
  title       TEXT NOT NULL,
  path        TEXT,
  summary     TEXT,
  risk        TEXT,                     -- low|medium|high|critical
  added       INTEGER,
  removed     INTEGER,
  snapshot_id TEXT                      -- content hash of the captured blob
);
CREATE INDEX idx_events_ts   ON events(ts);
CREATE INDEX idx_events_path ON events(path, ts);

-- Agent sessions (one per detected agent + a "workspace" session)
CREATE TABLE sessions (
  id          TEXT PRIMARY KEY,
  agent       TEXT NOT NULL,
  name        TEXT NOT NULL,
  task        TEXT,
  status      TEXT NOT NULL,            -- active|idle|…
  started_at  INTEGER NOT NULL,
  ended_at    INTEGER,
  tokens_in   INTEGER DEFAULT 0,
  tokens_out  INTEGER DEFAULT 0,
  cost_usd    REAL DEFAULT 0
);

-- Per-file activity (Heatmap source) + latest blob (diff base)
CREATE TABLE file_stats (
  path        TEXT PRIMARY KEY,
  edits       INTEGER DEFAULT 0,
  last_ts     INTEGER,
  churn       INTEGER DEFAULT 0,        -- added+removed, lifetime
  last_blob   TEXT                      -- content hash of the latest snapshot
);

-- Named checkpoints (Time Machine)
CREATE TABLE checkpoints (
  id     TEXT PRIMARY KEY,
  ts     INTEGER NOT NULL,
  label  TEXT,
  tree   TEXT NOT NULL,                 -- JSON: { path: blob_hash }
  auto   INTEGER DEFAULT 0
);
```

Projections (`file_stats`, session counts, heatmap, recap) are derived from
`events` and are fully reconstructible by replaying the log. The **Digital Twin**
graph is built on demand by scanning imports (`twin`), not stored.

## Snapshot store (content-addressed)

- Each captured file version is a **blob**, keyed by the SHA-256 of its bytes
  (automatic dedupe), zlib-compressed under `snapshots/blobs/<aa>/<rest>`.
- A **checkpoint** is a `path → blob hash` map (JSON in `checkpoints.tree`).
- **Restore / rewind** = resolve the tree for a checkpoint (or fold the event
  history to a timestamp via `state_at`), then write blobs back to the working
  tree and remove files that didn't exist then.
- Diffs (before/after) compare the current bytes against the previous blob.

## Why SQLite + a content-addressed store

SQLite is great for the *log and queries* but poor at storing many large file
revisions; a content-addressed blob store is great at *deduped revisions* but poor
at ad-hoc queries. Splitting along that seam gives each the job it's best at, and
keeps the whole thing a single embedded, offline dependency set — no C build deps.
