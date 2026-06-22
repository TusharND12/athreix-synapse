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
makes any agent action undoable independent of the user's git. A hard floor
against destructive automation (a "before rewind" checkpoint is auto-saved first).

## 5. Process isolation

Agents launched via `synapse run` (or the bake-off) run as child processes with a
scoped working directory; the bake-off isolates each agent in its own git
worktree so they can't collide.

## Threat model (v1)

| Threat | Mitigation |
|---|---|
| Agent makes a destructive change | policy flags + Time Machine rewind/restore |
| Malicious change hidden in noise | per-change explainability + risk + heatmap |
| History rewritten to hide actions | append-only log + content-addressed snapshots |
| Code content leaving the device | local-first; **no network at all** |

Out of scope for v1: a fully compromised host OS, or agents the user has
deliberately granted unsandboxed shell access.
