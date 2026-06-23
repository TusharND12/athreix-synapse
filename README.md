<div align="center">

# ⬡ Synapse

**The terminal-native OS for AI coding agents.** · by **Athreix**

Visible · Explainable · Approvable · Reversible · Replayable — fully offline.

</div>

---

`synapse` turns any terminal into a VS Code-style IDE for working with AI coding
agents (Claude Code, Gemini CLI, Aider, Codex, …): a file Explorer, a real
embedded terminal, live activity, git, diffs, time-travel and policy guardrails —
all inside the terminal, on any OS. No editor, no plugins, no network.

## Install

```bash
npm install -g athreix-synapse        # downloads the prebuilt `synapse` binary (win/macOS/linux · x64/arm64)
```

On macOS/Linux, if this fails with `EACCES` (permission denied):

```bash
sudo npm install -g athreix-synapse --unsafe-perm
```

## Use

```bash
cd my-project
synapse                 # the VS Code-style terminal IDE (default)
```

Inside: type your agent (`claude`, `aider`, …) into the embedded terminal and the
panels react live. Mouse works — click panes/tabs/files, drag borders to resize,
drag-select to copy. Keys: `F1` palette · `Ctrl+P` quick-open · `F2` new terminal
· `F6` focus · `F7/F8` tabs · `Ctrl+Q` quit · `y` copy path in Explorer.

### Other commands

```bash
synapse run <cmd>          # run an agent/command through a PTY (mirrors + records it)
synapse dashboard          # live TUI cockpit
synapse watch              # stream activity to the terminal
synapse status | log | twin | heatmap
synapse checkpoint --label "x" | checkpoints | restore <id>
synapse recap [--minutes N]   # what did the AI just do?
synapse rewind <minutes>      # rewind the working tree to N minutes ago
synapse policy                # show guardrails
synapse theme [name]          # list / set theme (deep-space, tokyo-night, nord, …)
```

Everything writes to a local `.synapse/` folder per project (SQLite + snapshots).

## Before you commit or deploy

Synapse **observes and records** what AI agents change — it does **not** verify
correctness or deploy anything. You stay in control:

- **Review changes** before committing/deploying — Git panel + diff (`F6 → Git`),
  or `synapse log` / `synapse recap`.
- **It's reversible** — checkpoints + `synapse rewind <minutes>` undo agent work
  (rewind never deletes files it has no history for; a "before" checkpoint is
  auto-saved).
- **Secrets** (`.env`, keys) are never snapshotted; `synapse policy` shows guardrails.
- **Run your own tests/builds** — Synapse won't do it for you.

(The terminal IDE shows these guidelines each time it launches — press any key to continue.)

## Uninstall

```bash
synapse uninstall            # shows how to remove it; --purge also deletes Synapse data
npm uninstall -g athreix-synapse   # removes the command
```

`synapse uninstall --purge` additionally removes `~/.synapse` (config) and the
current project's `.synapse/` (event log + snapshots).

## Layout

```
synapse-core/   pure-Rust engine — events, store, snapshots, watcher, time
                machine, twin, explainability, policy, recap, scan. No network.
synapse-cli/    the `synapse` binary — TUI + subcommands.
docs/           design notes (historical).
```
