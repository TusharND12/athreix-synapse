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

If a prebuilt binary isn't available for your platform, build from source instead:

```bash
cargo install --git https://github.com/TusharND12/athreix-synapse synapse-cli
```

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
