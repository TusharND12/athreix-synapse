# 06 — Ambient Detection (Universal Terminal Compatibility)

> Synapse adapts to developers. Developers do not adapt to Synapse.

The goal: a developer starts *any* AI agent in *any* terminal on *any* OS, and
the project simply **appears** in Synapse — files, diffs, git ops and the
architecture graph updating live, with **zero configuration**.

## How it works

A background **scanner** (`synapse-core/src/scan.rs`, driven by the CLI) polls the
process table (~2.5s, via `sysinfo`), identifies running AI agents, resolves each
one's working directory, and:

1. **Records the agent** in a shared registry (`root → AgentKind`) so the file
   watcher **attributes** each change to the real agent (Claude/Aider/Gemini/…),
   not a generic "human".
2. **Maintains sessions** — a live session per detected agent, marked idle when
   the agent exits — feeding the activity/agent panels.

## Why it works with *any* terminal / agent

We never wrap or replace the terminal — we observe the **filesystem + process
table + git**. The user's terminal (Windows Terminal, iTerm2, Warp, Alacritty,
Kitty, WSL, …) and agent (Claude Code, Aider, Gemini CLI, Codex, OpenCode,
Continue, …) are irrelevant to the mechanism. Agent identity is matched by binary
name *or* command-line token, so node-/python-wrapped agents are caught too
(`detect_agent`).

## The capture tiers (what's zero-config vs opt-in)

| Signal | Zero-config? | Mechanism |
|---|---|---|
| File create/modify/delete/rename + diffs | ✅ | `notify` watcher (Tier-1) |
| Which agent is running, where | ✅ | process scanner |
| Git operations (commit/branch/checkout/merge) | ✅ | watch `.git/HEAD`, `refs/heads`, `COMMIT_EDITMSG` |
| Commands / errors / exit | ✅ via `synapse run` | PTY wrap (or the in-TUI terminal) |
| Build/test running elsewhere | 🟡 partial | detect build/test child processes |
| Agent reasoning / decisions | ⚙️ future | tail agent log files / agent-native hooks |

**Honest framing:** read-only observability is zero-config; capturing commands is
one command away (`synapse run <agent>`), and it *still* doesn't change how you
work — you run the same agent, just through Synapse.

## Cross-platform reality: reading a process's cwd

- **Linux** — `/proc/<pid>/cwd`. ✅
- **macOS** — `libproc`. ✅
- **Windows** — another process's cwd usually isn't readable without elevated
  PEB access; `sysinfo` often returns empty. **Fallback:** attribute detected
  agents to the project Synapse is watching (the directory you launched it in).

## Status

Implemented: process scanner, agent attribution, `.git` op detection, and PTY
capture via `synapse run` / the in-TUI terminal. Deferred: shell integration and
per-agent log tailing for full reasoning capture.
