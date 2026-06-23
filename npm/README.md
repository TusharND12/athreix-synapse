# athreix-synapse

**Synapse — the terminal-native OS for AI coding agents.** A VS Code-style TUI
(embedded terminal, file explorer, live activity, git/diff, time-travel, policy
guardrails) for Claude Code / Gemini / Aider / Codex. Fully offline.

## Install

```bash
npm install -g athreix-synapse
```

This downloads the prebuilt `synapse` binary for your OS (Windows / macOS / Linux,
x64 / arm64). Then, in any project:

```bash
cd my-project
synapse
```

### macOS / Linux: permission error?

If `npm install -g` fails with `EACCES` (permission denied), run it with
elevated permissions:

```bash
sudo npm install -g athreix-synapse --unsafe-perm
```

The `--unsafe-perm` flag lets the installer download the binary when run under
`sudo`. (Alternatively, point npm at a user-owned prefix so you never need
`sudo`: `npm config set prefix ~/.npm-global` and add `~/.npm-global/bin` to
your `PATH`.)

## Uninstall

```bash
synapse uninstall --purge          # deletes Synapse data (~/.synapse + this project's .synapse/)
npm uninstall -g athreix-synapse   # removes the command itself
```

`synapse uninstall` (without `--purge`) just prints what to remove. Run the
`--purge` step *before* `npm uninstall` if you want the data gone too.

## Links

- Source & docs: https://github.com/TusharND12/athreix-synapse
- License: MIT
