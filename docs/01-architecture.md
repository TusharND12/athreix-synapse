# 01 — Architecture

Synapse is terminal-native: one Rust engine, one terminal binary. No webview, no
server, no network.

```
┌───────────────────────────── synapse (process) ─────────────────────────────┐
│  synapse-cli  (TUI + subcommands)                                            │
│    • ratatui/crossterm UI  • embedded PTY (portable-pty + vt100)             │
│    • mouse, themes, palette, quick-open, explorer, git/diff panes            │
│        │ calls                                                               │
│        ▼                                                                     │
│  synapse-core  (engine, no UI)                                               │
│    events · store(SQLite) · snapshots(CAS) · watcher(notify) · scan(sysinfo) │
│    timemachine · twin · explain · policy · recap                            │
└──────────────────────────────────────────────────────────────────────────────┘
            │ watches / snapshots / reads git
            ▼
   the user's project  +  a hidden  .synapse/  store (SQLite + blobs)
```

## Layers

1. **Capture** — the `notify` file watcher and the `sysinfo` process scanner turn
   the outside world into normalized `SynapseEvent`s; `synapse run` PTY-wraps an
   agent/command to also capture commands, errors and exit.
2. **Persist** — append events to SQLite (`store`); snapshot file content into a
   content-addressed blob store (`snapshots`). The append-only log *is* the audit.
3. **Project** — derived views: recent activity, file/heatmap stats, the
   dependency graph (`twin`), state-at-time-t (`timemachine`), and the recap.
4. **Present** — the CLI: the TUI (`tui`) and one-shot subcommands.

## Data flow (a file changes)

```
agent edits file → notify fires → core reads bytes, snapshots a blob,
diffs vs the previous snapshot (similar) → SynapseEvent{kind,+/-,path,agent}
→ append to SQLite → policy check (warn/deny) → UI renders within ~100ms
```

The active agent for a project comes from the scanner (`root → AgentKind`), so
file changes are attributed to the real agent (Claude/Aider/Gemini/…).

## Why these choices

- **TUI, not a GUI** — runs in any terminal on any OS; no Electron/WebView.
- **Pure Rust, offline** — `rusqlite` (bundled SQLite), pure-Rust content-addressed
  snapshots, `syntect` with the `fancy-regex` backend — no C build deps, no network.
- **Event sourcing** — replay, time-travel, recap and audit fall out of one log.

## Crates & key modules

```
synapse-core/src/
  events/      the SynapseEvent model (contract)
  store/       SQLite append log + projections (sessions, file_stats, checkpoints)
  snapshots/   content-addressed blob store (sha-256 + zlib)
  watcher/     notify → events (+ git-op detection, policy hook)
  scan.rs      process-table scan → detected agents
  timemachine/ checkpoints, restore, state_at (rewind)
  twin/        import-graph dependency builder
  explain/     rule-based change explanations
  policy.rs    guardrail rules
  recap.rs     "what did the AI do" summaries

synapse-cli/src/
  main.rs      command dispatch + one-shot subcommands
  tui.rs       the VS Code-style TUI
  dashboard.rs simple live cockpit
  theme.rs     themes + config
```
