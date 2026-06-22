# Synapse — Engineering Docs

> The terminal-native OS for AI coding agents. By **Athreix**. Fully offline.

Synapse is a two-crate Rust workspace:

- **`synapse-core`** — the engine: an append-only event store, content-addressed
  snapshots, file watcher, time machine, dependency graph, explainability,
  policy guardrails, and process scanning. No GUI, no network.
- **`synapse-cli`** — the `synapse` binary: a VS Code-style TUI plus subcommands.

## The one idea

Everything reduces to **one append-only event stream** of what agents do to the
filesystem and git. This is **event sourcing**. Get the event model + snapshot
store + the agent-detection layer right, and every view (live activity, replay,
time machine, heatmap, twin, recap) is a projection over the same data.

```
agents ──observe──▶ EVENT STORE (SQLite) + SNAPSHOT STORE (content-addressed)
                          │  projections
   activity · replay · time-machine/rewind · heatmap · twin · recap · guardrails
```

## Index

| Doc | Covers |
|---|---|
| [01-architecture.md](./01-architecture.md) | crates, layers, data flow |
| [02-event-model.md](./02-event-model.md) | the event taxonomy + projections |
| [03-database-schema.md](./03-database-schema.md) | SQLite tables + snapshot store |
| [04-agent-adapters.md](./04-agent-adapters.md) | how Synapse observes each agent |
| [05-security-model.md](./05-security-model.md) | local-first, audit, guardrails |
| [06-ambient-detection.md](./06-ambient-detection.md) | auto-detecting running agents |
| [99-roadmap.md](./99-roadmap.md) | status + what's next |

Run it: `cargo install --path synapse-cli`, then `synapse` in any project.
