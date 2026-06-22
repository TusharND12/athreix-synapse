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

## Links

- Source & docs: https://github.com/TusharND12/athreix-synapse
- License: MIT
