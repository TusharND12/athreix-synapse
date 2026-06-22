# 05 — Security Model

Synapse sits between developers and autonomous agents that read and write the
whole codebase. Trust is the product.

## 1. Local-first, no exfiltration

All data — events, snapshots, source — stays on the machine in `.synapse/`. There
is **no network, no telemetry**. (A future, opt-in LLM explainability enrichment
would be clearly labeled and disclosed before enabling; it is not built.)

## 2. The audit log is the database

The append-only event store *is* a tamper-evident record of every action: who
(agent/session), what (diff), when (ts). Append-only + content-addressed
snapshots mean history can't be silently rewritten. (Optional future:
hash-chaining each row for cryptographic tamper-evidence.)

## 3. Policy guardrails

The `policy` engine evaluates every change and flags it in the timeline:
- **Deny**-class rules (e.g. `.env` edits, deleting auth files) — meant to block.
- **Warn**-class rules (secrets/payments/migrations, >200-line changes).
Run `synapse policy` to see the active set. Risk scoring on each change surfaces
blast radius so review is informed.

## 4. Reversibility as a safety net

Time Machine — `synapse rewind <minutes>`, checkpoints, and per-event snapshots —
makes any agent action undoable independent of the user's git. Safety guarantees:
- **Rewind never deletes unhistoried files** — only files Synapse has actually
  recorded can be removed, so pre-existing/untouched files are always kept.
- Both `rewind` and `restore` **auto-save a checkpoint of the current state first**,
  so any restore is itself reversible.
- Restores only ever touch the project tree and **skip** `.git`, `node_modules`,
  `target`, etc.

## 5. Secrets are never snapshotted

The watcher skips capturing the *contents* of secret files (`.env*`, `*.pem`,
`*.key`, `id_rsa`, `.npmrc`, `*secret*`, `*credential*`, …): the event still shows
in the timeline for visibility, but nothing sensitive is written to the (plaintext)
snapshot store.

## 6. Bounded, prunable storage

`.synapse/` holds only this project's data. `synapse prune [--days N]` removes old
events and garbage-collects unreferenced snapshot blobs, so storage stays bounded.

## 7. Process isolation

Agents launched via `synapse run` (or the bake-off) run as child processes with a
scoped working directory; the bake-off isolates each agent in its own git
worktree so they can't collide. Subprocesses (`git`, `clip`, the shell PTY) are
always invoked with **arguments**, never an interpolated shell string — no command
injection. There is no `unsafe` code and no network in the project.

## Threat model (v1)

| Threat | Mitigation |
|---|---|
| Agent makes a destructive change | policy flags + Time Machine (rewind keeps unhistoried files) |
| Rewind/restore destroying data | scoped deletion + auto "before restore" checkpoint |
| Secrets copied to disk | secret files are never snapshotted |
| Unbounded `.synapse/` growth | `synapse prune` (event retention + blob GC) |
| Malicious change hidden in noise | per-change explainability + risk + heatmap |
| History rewritten to hide actions | append-only log + content-addressed snapshots |
| Code content leaving the device | local-first; **no network at all**; no `unsafe` |

Out of scope for v1: a fully compromised host OS, or agents the user has
deliberately granted unsandboxed shell access.
