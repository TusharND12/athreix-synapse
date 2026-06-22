//! Agent adapter layer.
//!
//! Tier-1 (universal) is the file watcher. **Ambient detection** (Phase 4.5)
//! upgrades it: a process scanner identifies which AI agents are actually running
//! and where, so file changes are attributed to the real agent (Claude / Aider /
//! Gemini / …) automatically, with zero configuration. Tiers 2–4 (Claude Code
//! hooks, PTY, MCP) layer richer capture on top. See docs/04-agent-adapters.md.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::events::AgentKind;
use crate::store::Session;

/// Common interface every agent integration implements.
pub trait AgentAdapter {
    fn id(&self) -> &str;
    fn kind(&self) -> AgentKind;
    /// Can this adapter gate a write *before* it lands (Surgery Mode)?
    fn can_gate(&self) -> bool {
        false
    }
}

/// Maps a project root (string) → the agent currently active in it, so the
/// watcher can attribute file changes. Shared between the scanner and watchers.
pub type AgentRegistry = Arc<Mutex<HashMap<String, AgentKind>>>;

/// Identify a known AI coding agent from a process name + command line.
/// Matches the binary name or any token in the command line (catches
/// node/python-wrapped agents like Claude Code or Aider).
pub fn detect_agent(name: &str, cmdline: &str) -> Option<AgentKind> {
    let hay = format!("{} {}", name.to_lowercase(), cmdline.to_lowercase());
    let has = |needle: &str| {
        // Tokenize on any non-alphanumeric so "claude-code" / "@anthropic-ai/claude-code"
        // both surface a bare "claude" token, while avoiding broad substring hits.
        hay.split(|c: char| !c.is_alphanumeric()).any(|tok| tok == needle)
    };
    if has("claude") {
        Some(AgentKind::Claude)
    } else if has("aider") {
        Some(AgentKind::Aider)
    } else if has("gemini") {
        Some(AgentKind::Gemini)
    } else if has("codex") || has("opencode") {
        Some(AgentKind::Gpt)
    } else if has("cursor") || has("continue") {
        Some(AgentKind::Cursor)
    } else {
        None
    }
}

pub fn label_for(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "Claude Code",
        AgentKind::Gpt => "OpenAI Codex",
        AgentKind::Gemini => "Gemini CLI",
        AgentKind::Aider => "Aider",
        AgentKind::Cursor => "Cursor / Continue",
        AgentKind::Human => "Workspace",
    }
}

/// Build the session record for the universal watcher adapter.
pub fn workspace_session(id: &str, started_at: i64) -> Session {
    Session {
        id: id.to_string(),
        agent: AgentKind::Human.as_str().to_string(),
        name: "Workspace".to_string(),
        task: "Filesystem activity (all agents)".to_string(),
        status: "active".to_string(),
        files_touched: 0,
        tokens_in: 0,
        tokens_out: 0,
        cost_usd: 0.0,
        started_at,
    }
}

/// Build a session record for a detected agent.
pub fn detected_session(kind: AgentKind, started_at: i64) -> Session {
    Session {
        id: format!("agent-{}", kind.as_str()),
        agent: kind.as_str().to_string(),
        name: label_for(kind).to_string(),
        task: "Detected running in terminal".to_string(),
        status: "active".to_string(),
        files_touched: 0,
        tokens_in: 0,
        tokens_out: 0,
        cost_usd: 0.0,
        started_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_agents_from_name_or_cmdline() {
        assert_eq!(detect_agent("claude", ""), Some(AgentKind::Claude));
        assert_eq!(detect_agent("aider.exe", ""), Some(AgentKind::Aider));
        // node-wrapped Claude Code
        assert_eq!(
            detect_agent("node", "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"),
            Some(AgentKind::Claude)
        );
        assert_eq!(detect_agent("python", "-m aider"), Some(AgentKind::Aider));
        assert_eq!(detect_agent("gemini", ""), Some(AgentKind::Gemini));
        // unrelated process
        assert_eq!(detect_agent("bash", "ls -la"), None);
        // path containing 'claude' shouldn't false-match as a bare token... but a
        // real claude invocation will. We accept directory hits as a signal.
        assert_eq!(detect_agent("vim", "notes.txt"), None);
    }
}
