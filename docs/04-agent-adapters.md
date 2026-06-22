# 04 — Observing Agents

The hard part of an "OS for AI agents" is observing tools that weren't built to be
observed. Synapse uses a **tiered strategy**: a universal floor that works for any
agent with zero integration, plus richer capture where it's available — all
terminal-native, no plugins.

## Tier 1 — Universal (any agent, zero config)

**File watcher (`notify`) + git.** Any agent that touches the filesystem is
observed: created/modified/deleted/moved/renamed with real line diffs (`similar`),
plus git operations (commit/branch/checkout) by watching `.git/HEAD`,
`refs/heads`, `COMMIT_EDITMSG`. Works with Claude Code, Aider, Gemini CLI, Codex,
Cursor — anything, on day one.

## Tier 1.5 — Agent attribution (zero config)

A `sysinfo` **process scanner** identifies which AI agents are actually running
and records `root → AgentKind`, so each file change is attributed to the real
agent rather than a generic "human". See [06-ambient-detection.md](./06-ambient-detection.md).

## Tier 2 — PTY capture (`synapse run`)

Run an agent/command *through* Synapse: `synapse run claude` (or `synapse run --
npm test`). It PTY-wraps the process (`portable-pty`), mirrors I/O to your
terminal so you interact normally, and records the command, notable error lines
(ANSI-stripped), and exit status as events. The TUI's embedded terminal uses the
same PTY engine, so anything you run inside it is captured too.

## Gating — policy guardrails

Synapse can't block an arbitrary external agent's write after the fact, but the
**policy engine** (`policy.rs`) evaluates every observed change and flags
warnings/denials live in the timeline (e.g. `.env` edits, auth deletions, >200-line
changes). Combined with **Time Machine** (one-key rewind/restore), destructive
changes are caught and reversible. See [05-security-model.md](./05-security-model.md).

## Normalization

Every path produces `SynapseEvent`s (see [02-event-model.md](./02-event-model.md)).
Once an event is in the stream, the views and projections don't care which agent
or tier it came from — that abstraction holds the whole product together.
