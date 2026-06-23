# 99 — Status & Roadmap

Synapse is a terminal-native tool: `synapse` turns any terminal into a VS Code-style
IDE for AI coding agents. Fully offline.

## Shipped

| Area | What |
|---|---|
| **Engine** (`synapse-core`) | append-only SQLite event store, content-addressed snapshots, `notify` watcher with real line diffs + git-op detection, `sysinfo` process scanner with agent attribution |
| **Terminal IDE** (`synapse`) | full-screen ratatui TUI: file-tree Explorer, embedded PTY terminal (vt100), live activity, Git panel + colorized diff, file preview |
| **Editor feel** | syntax highlighting (`syntect`), fuzzy quick-open (`Ctrl+P`), command palette (`F1`), multi-terminal tabs, themes (5, live-switchable) |
| **Mouse** | click panes/tabs/files, drag borders to resize, scroll, drag-select + copy, `y` copy-path |
| **Time** | checkpoints + restore, `synapse rewind <min>` (state-at-time), replay-ordered events |
| **Trust** | rule-based explainability, policy guardrails (live-flagged, `synapse policy`) |
| **Recap** | `synapse recap` — what the AI did, risk-flagged |
| **Multi-agent** | bake-off picker → race chosen agents in isolated git worktrees |
| **PTY capture** | `synapse run <cmd>` mirrors I/O + records commands/errors/exit |
| **Robustness** | panic-safe terminal restore, redraw-on-change, `TestBackend` render tests |
| **Distribution** | `npm install -g athreix-synapse` → global `synapse`; ~3.7 MB binary |

## Commands

`synapse` (TUI, default) · `run` · `dashboard` · `watch` · `status` · `log` ·
`twin` · `heatmap` · `checkpoint`/`checkpoints`/`restore` · `recap` · `rewind` ·
`policy` · `theme`.

## Next candidates

- Explorer scrolling for long trees (so click-to-open maps past the viewport).
- Drag-select + copy in the Preview/Diff panes (today: the terminal pane).
- Bake-off result diff/merge (compare what each agent produced).
- Codebase scrubber: a draggable timeline that rewinds the whole project (the
  `state_at` engine already exists — needs the slider UI).
- Custom user themes (`~/.synapse/themes/*.json`) + shell integration for full
  command capture without `synapse run`.
- `npm install -g synapse` wrapper (prebuilt binaries) for non-Rust users.

## The moat

The append-only, agent-attributed history of *how* the code was built — rewindable,
replayable, explainable, governable — is data no bolt-on terminal has.
