//! The event model — the heart of Synapse.
//!
//! Everything the product shows (observability, replay, time machine, heatmap,
//! surgery, war room) is a *view* over one append-only stream of these events.
//! Phase 0 defines the shape; Phase 1 adds the SQLite append log, the libgit2
//! snapshot store, the `notify` file watcher, and the Tauri event channel that
//! streams `SynapseEvent`s to the UI under the `synapse://event` topic.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Created,
    Modified,
    Deleted,
    Moved,
    Renamed,
    Command,
    Reasoning,
    Build,
    Test,
    ApprovalRequested,
    ApprovalGranted,
    Checkpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Gpt,
    Gemini,
    Aider,
    Cursor,
    Human,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RiskLevel {
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "critical")]
    Critical,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::Created => "created",
            EventKind::Modified => "modified",
            EventKind::Deleted => "deleted",
            EventKind::Moved => "moved",
            EventKind::Renamed => "renamed",
            EventKind::Command => "command",
            EventKind::Reasoning => "reasoning",
            EventKind::Build => "build",
            EventKind::Test => "test",
            EventKind::ApprovalRequested => "approval_requested",
            EventKind::ApprovalGranted => "approval_granted",
            EventKind::Checkpoint => "checkpoint",
        }
    }
}

impl AgentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Gpt => "gpt",
            AgentKind::Gemini => "gemini",
            AgentKind::Aider => "aider",
            AgentKind::Cursor => "cursor",
            AgentKind::Human => "human",
        }
    }
}

impl RiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            RiskLevel::Low => "low",
            RiskLevel::Medium => "medium",
            RiskLevel::High => "high",
            RiskLevel::Critical => "critical",
        }
    }
}

/// Mirror of the frontend `SynapseEvent` (see `src/lib/types.ts`). This is the
/// contract that travels over the Tauri event channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynapseEvent {
    pub id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// epoch millis
    pub ts: i64,
    #[serde(rename = "agentId")]
    pub agent_id: String,
    pub agent: AgentKind,
    pub kind: EventKind,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<RiskLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<u32>,
    /// Content hash of the snapshot captured for this change (Time Machine).
    #[serde(rename = "snapshotId", skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
}
