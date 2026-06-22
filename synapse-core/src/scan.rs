//! Process-table scan for ambient agent detection (engine side). Front-ends
//! decide what to do with the results (auto-watch, attribute, display).

use std::path::PathBuf;

use sysinfo::{ProcessesToUpdate, System};

use crate::agents::detect_agent;
use crate::events::AgentKind;

/// One detected agent and its working directory (None if the OS hides it).
pub type Detected = (AgentKind, Option<PathBuf>);

/// A reusable process-table handle (so front-ends don't depend on `sysinfo`).
pub fn new_system() -> System {
    System::new()
}

/// Refresh the process table and return the AI agents currently running.
/// `self_pid` is excluded so the host app never detects itself.
pub fn scan_agents(sys: &mut System, self_pid: u32) -> Vec<Detected> {
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let mut out: Vec<Detected> = Vec::new();
    for (pid, proc_) in sys.processes() {
        if pid.as_u32() == self_pid {
            continue;
        }
        let name = proc_.name().to_string_lossy().to_string();
        let cmd = proc_
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(kind) = detect_agent(&name, &cmd) {
            let cwd = proc_.cwd().map(|p| p.to_path_buf());
            if let Some(slot) = out.iter_mut().find(|(k, _)| *k == kind) {
                if slot.1.is_none() {
                    slot.1 = cwd;
                }
            } else {
                out.push((kind, cwd));
            }
        }
    }
    out
}
