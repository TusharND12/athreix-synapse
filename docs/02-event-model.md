# 02 — Event Model (the heart)

Synapse is event-sourced. One **append-only** stream of `SynapseEvent`s is the
ground truth; every product surface is a projection over it.

Canonical shape lives in code: `synapse-core/src/events/mod.rs`.

## Event taxonomy

| Kind | Source | Carries |
|---|---|---|
| `created` / `modified` / `deleted` | watcher + git | path, +added/-removed, risk |
| `moved` / `renamed` | watcher + git | from→to path |
| `command` | PTY / hook | argv, cwd, exit code, duration |
| `reasoning` | agent adapter | the agent's stated *why* (1 line) |
| `build` / `test` | command result parsing | status, counts, failing items |
| `approval_requested` / `approval_granted` | policy guardrails | flagged change, decision |
| `checkpoint` | core | snapshot tree id, label |

Every event has: `id`, `sessionId`, `ts` (epoch ms), `agentId`, `agent`,
`kind`, `title`. File-scoped events add `path`, `added`, `removed`, `risk`,
and an explainability `summary`.

## Invariants

1. **Append-only.** Events are never mutated or deleted. Corrections are new
   events. This is what makes replay and audit trustworthy.
2. **Monotonic, ordered.** `(ts, id)` totally orders the stream per project.
3. **Self-describing.** An event + the snapshot it references fully reconstructs
   "what the project looked like right after this happened."

## Projections (views, all derivable)

| Projection | Feeds | How |
|---|---|---|
| Live timeline | Observability (F1) | tail of the stream |
| File tree @ t | Replay / Time Machine (F4/F8) | fold events ≤ t, resolve via snapshots |
| Session stats | agent roster | group-by `sessionId` |
| Health metrics | Command Center (F9) | rolling aggregates |
| Heat aggregates | Heatmap (F6) | count by `path` over window |
| Dependency graph | Digital Twin | import scan of the file tree |

Projections are cached as materialized tables (see `03-database-schema.md`) and
rebuilt incrementally as events arrive; a full rebuild is always possible by
replaying the log (the recovery guarantee).

## Replay

Replay = re-emit the event slice `[t0, t1]` to the UI at a chosen speed
(1×/2×/5×/10×), optionally reconstructing file state at each step from snapshots.
Because state at any `t` is a pure fold of the log, scrubbing is deterministic.
