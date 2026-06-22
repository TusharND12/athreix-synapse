//! Synapse CLI (`syn`) — terminal-native, fully-offline observability for AI
//! coding agents. Reuses the same engine as the desktop app (`synapse-core`).
//!
//!   syn watch [PATH]          observe a project live (streams to the terminal)
//!   syn dashboard [PATH]      live TUI cockpit
//!   syn status [PATH]         agents + project health
//!   syn log [PATH] --tail N   recent events  (--json for machine output)
//!   syn checkpoint [PATH]     snapshot the working tree   (--label "…")
//!   syn checkpoints [PATH]    list checkpoints
//!   syn restore <ID> [PATH]   restore the working tree to a checkpoint
//!   syn twin [PATH]           dependency graph   (--json)
//!   syn heatmap [PATH]        most-changed files

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};

use synapse_core::agents::{detect_agent, detected_session, workspace_session, AgentRegistry};
use synapse_core::events::{AgentKind, EventKind, RiskLevel, SynapseEvent};
use synapse_core::scan::{new_system, scan_agents};
use synapse_core::snapshots::Snapshots;
use synapse_core::store::Db;
use synapse_core::watcher::{self, EventSink};
use synapse_core::{policy, recap, timemachine, twin};

mod dashboard;
mod theme;
mod tui;

#[derive(Parser)]
#[command(name = "synapse", version, about = "Synapse - terminal-native OS for AI coding agents")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// VS Code-style terminal IDE in THIS terminal (default).
    Tui { path: Option<String> },
    /// Run an agent/command through Synapse (PTY) — mirrors I/O + records it.
    Run {
        #[arg(trailing_var_arg = true, required = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
    /// Observe a project live; stream events to the terminal.
    Watch { path: Option<String> },
    /// Live TUI cockpit.
    Dashboard { path: Option<String> },
    /// Show detected agents and project health.
    Status { path: Option<String> },
    /// Print recent events.
    Log {
        path: Option<String>,
        #[arg(long, default_value_t = 20)]
        tail: i64,
        #[arg(long)]
        json: bool,
    },
    /// Snapshot the current working tree as a checkpoint.
    Checkpoint {
        path: Option<String>,
        #[arg(long)]
        label: Option<String>,
    },
    /// List checkpoints.
    Checkpoints { path: Option<String> },
    /// Restore the working tree to a checkpoint.
    Restore { id: String, path: Option<String> },
    /// Build the dependency graph (Digital Twin).
    Twin {
        path: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show the most-changed files.
    Heatmap { path: Option<String> },
    /// "What did the AI just do?" — recap recent activity.
    Recap {
        path: Option<String>,
        #[arg(long, default_value_t = 30)]
        minutes: i64,
    },
    /// Rewind the working tree to its state N minutes ago (auto-checkpoints first).
    Rewind { minutes: i64, path: Option<String> },
    /// Show the active policy guardrails.
    Policy { path: Option<String> },
    /// List themes, or set one: `synapse theme tokyo-night`.
    Theme { name: Option<String> },
}

// ── ANSI helpers (no extra deps; works in modern Windows terminals) ─────────
const RST: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const GRN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const CYN: &str = "\x1b[36m";
const YEL: &str = "\x1b[33m";
const BOLD: &str = "\x1b[1m";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// UTC HH:MM:SS from epoch millis (dependency-free).
pub fn clock(ts: i64) -> String {
    let secs = ts / 1000;
    format!("{:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}

fn resolve(path: &Option<String>) -> PathBuf {
    let p = PathBuf::from(path.clone().unwrap_or_else(|| ".".into()));
    std::fs::canonicalize(&p).unwrap_or(p)
}

/// Open (and initialize) a project's local store.
fn open(root: &PathBuf) -> Result<(Db, Snapshots), String> {
    if !root.is_dir() {
        return Err(format!("Not a directory: {}", root.display()));
    }
    let synapse_dir = root.join(".synapse");
    std::fs::create_dir_all(&synapse_dir).map_err(|e| e.to_string())?;
    let db = Db::open(&synapse_dir.join("synapse.db")).map_err(|e| e.to_string())?;
    let snaps = Snapshots::open(&synapse_dir).map_err(|e| e.to_string())?;
    db.ensure_session(&workspace_session("workspace", now_ms())).ok();
    Ok((db, snaps))
}

fn print_event(e: &SynapseEvent) {
    let mut line = format!(
        "{DIM}{}{RST}  {CYN}{:>7}{RST}  {}",
        clock(e.ts),
        e.agent.as_str(),
        e.title
    );
    if let Some(p) = &e.path {
        line += &format!("  {DIM}{p}{RST}");
    }
    if let Some(a) = e.added {
        line += &format!("  {GRN}+{a}{RST}");
    }
    if let Some(r) = e.removed {
        if r > 0 {
            line += &format!(" {RED}-{r}{RST}");
        }
    }
    println!("{line}");
}

/// Spawn the ambient agent scanner against a single watched root.
fn spawn_scanner(root_key: String, db: Db, agents: AgentRegistry, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut sys = new_system();
        let me = std::process::id();
        while !stop.load(Ordering::Relaxed) {
            let detected = scan_agents(&mut sys, me);
            if let Some((kind, _)) = detected.first() {
                agents.lock().unwrap().insert(root_key.clone(), *kind);
                let _ = db.ensure_session(&detected_session(*kind, now_ms()));
                let _ = db.set_session_status(&format!("agent-{}", kind.as_str()), "active");
            }
            std::thread::sleep(Duration::from_millis(2500));
        }
    });
}

fn emit(db: &Db, session: &str, agent: AgentKind, kind: EventKind, title: String, risk: Option<RiskLevel>) {
    let ev = SynapseEvent {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session.to_string(),
        ts: now_ms(),
        agent_id: session.to_string(),
        agent,
        kind,
        title,
        path: None,
        summary: None,
        risk,
        added: None,
        removed: None,
        snapshot_id: None,
    };
    let _ = db.append_event(&ev);
}

/// Strip ANSI/OSC escape sequences so recorded event titles are clean text.
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if c != '\u{7}' {
                out.push(c);
            }
            continue;
        }
        match chars.peek().copied() {
            // CSI: ESC [ … final letter-ish byte.
            Some('[') => {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n.is_ascii_alphabetic() || n == '~' || n == '@' {
                        break;
                    }
                }
            }
            // OSC: ESC ] … terminated by BEL or ESC \.
            Some(']') => {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n == '\u{7}' {
                        break;
                    }
                    if n == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // Other ESC sequence: drop the next byte.
            _ => {
                chars.next();
            }
        }
    }
    out
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Terminal mirroring: run a command/agent through a PTY. The user interacts with
/// it normally; Synapse mirrors the I/O to this terminal and records the command,
/// notable errors, and exit status as events (broadcast live by a running engine).
fn cmd_run(root: PathBuf, cmdv: Vec<String>) -> Result<(), String> {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};

    let (db, _snaps) = open(&root)?;
    let program = cmdv[0].clone();
    let args = cmdv[1..].to_vec();
    let cmdline = cmdv.join(" ");
    let agent = detect_agent(&program, &cmdline).unwrap_or(AgentKind::Human);
    let session = if agent == AgentKind::Human {
        "workspace".to_string()
    } else {
        let s = format!("agent-{}", agent.as_str());
        let _ = db.ensure_session(&detected_session(agent, now_ms()));
        s
    };

    emit(&db, &session, agent, EventKind::Command, format!("$ {cmdline}"), Some(RiskLevel::Low));
    println!("{DIM}synapse: mirroring `{cmdline}` ({}){RST}", agent.as_str());

    let pty = native_pty_system();
    let (cols, rows) = crossterm::terminal::size().unwrap_or((100, 30));
    let pair = pty
        .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| e.to_string())?;
    let mut builder = CommandBuilder::new(&program);
    for a in &args {
        builder.arg(a);
    }
    // cmd.exe (and others) reject the `\\?\` verbatim prefix as a cwd; strip it.
    let cwd = {
        let s = root.to_string_lossy();
        s.strip_prefix(r"\\?\").map(PathBuf::from).unwrap_or_else(|| root.clone())
    };
    builder.cwd(cwd);
    let mut child = pair.slave.spawn_command(builder).map_err(|e| e.to_string())?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(|e| e.to_string())?));

    let _ = crossterm::terminal::enable_raw_mode();

    // Forward our stdin to the child PTY (so interactive agents work).
    {
        let w = writer.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 1024];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut g = w.lock().unwrap();
                        if g.write_all(&buf[..n]).is_err() || g.flush().is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }

    // Mirror PTY output → our stdout on a thread, extracting notable lines as
    // events. ConPTY may not EOF promptly after the child exits, so we don't join
    // this thread — the process exits cleanly once `child.wait()` returns below.
    {
        let rdb = db.clone();
        let rsession = session.clone();
        std::thread::spawn(move || {
            let mut out = std::io::stdout();
            let mut linebuf = String::new();
            let mut err_count = 0u32;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let _ = out.write_all(&buf[..n]);
                        let _ = out.flush();
                        if let Ok(s) = std::str::from_utf8(&buf[..n]) {
                            linebuf.push_str(s);
                            while let Some(idx) = linebuf.find('\n') {
                                let line: String = linebuf.drain(..=idx).collect();
                                let clean = strip_ansi(&line);
                                let lt = clean.trim();
                                if err_count < 50 && lt.to_lowercase().contains("error") {
                                    err_count += 1;
                                    emit(&rdb, &rsession, agent, EventKind::Command, format!("error: {}", trunc(lt, 120)), Some(RiskLevel::High));
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    let status = child.wait().map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(150)); // let the mirror thread flush
    let _ = crossterm::terminal::disable_raw_mode();
    let code = status.exit_code();
    emit(
        &db,
        &session,
        agent,
        EventKind::Build,
        format!("{program} exited (code {code})"),
        Some(if code == 0 { RiskLevel::Low } else { RiskLevel::High }),
    );
    println!("\n{DIM}synapse: `{program}` exited ({code}){RST}");
    Ok(())
}

fn cmd_watch(root: PathBuf) -> Result<(), String> {
    let (db, snaps) = open(&root)?;
    let agents: AgentRegistry = Arc::new(Mutex::new(HashMap::new()));
    let key = root.to_string_lossy().to_string();

    let sink: EventSink = Arc::new(|e: &SynapseEvent| print_event(e));
    let _watcher = watcher::start(
        root.clone(),
        db.clone(),
        snaps,
        sink,
        "workspace".into(),
        agents.clone(),
    )
    .map_err(|e| e.to_string())?;

    let stop = Arc::new(AtomicBool::new(false));
    spawn_scanner(key, db, agents, stop.clone());

    println!(
        "{BOLD}Synapse{RST} watching {CYN}{}{RST}  {DIM}(offline - Ctrl-C to stop){RST}\n",
        root.display()
    );
    let s = stop.clone();
    ctrlc::set_handler(move || s.store(true, Ordering::Relaxed)).ok();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(150));
    }
    println!("\n{DIM}stopped.{RST}");
    Ok(())
}

fn cmd_status(root: PathBuf) -> Result<(), String> {
    let (db, _) = open(&root)?;
    let sessions = db.list_sessions().map_err(|e| e.to_string())?;
    let h = db.health().map_err(|e| e.to_string())?;
    println!("{BOLD}Synapse · {}{RST}\n", root.display());
    println!(
        "  files modified {BOLD}{}{RST}   agents running {BOLD}{}{RST}   build {} / tests {}",
        h.files_modified, h.agents_running, h.build, h.tests
    );
    println!(
        "  risk {} / complexity {} / tracked files {}\n",
        h.risk_score, h.complexity, h.files_modified
    );
    println!("  {BOLD}Agents{RST}");
    if sessions.is_empty() {
        println!("    {DIM}none yet — run an agent in this folder{RST}");
    }
    for s in sessions {
        let dot = if s.status == "active" { GRN } else { DIM };
        println!(
            "    {dot}*{RST} {:<18} {DIM}{}{RST}  ({} files)",
            s.name, s.status, s.files_touched
        );
    }
    Ok(())
}

fn cmd_log(root: PathBuf, tail: i64, json: bool) -> Result<(), String> {
    let (db, _) = open(&root)?;
    let events = db.recent_events(tail).map_err(|e| e.to_string())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&events).unwrap_or_default());
        return Ok(());
    }
    // recent_events is newest-first; print oldest-first for a natural log.
    for v in events.iter().rev() {
        let clk = v.get("ts").and_then(|t| t.as_i64()).map(clock).unwrap_or_default();
        let agent = v.get("agent").and_then(|a| a.as_str()).unwrap_or("?");
        let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("");
        let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("");
        println!("{DIM}{clk}{RST}  {CYN}{agent:>7}{RST}  {title}  {DIM}{path}{RST}");
    }
    Ok(())
}

fn cmd_checkpoint(root: PathBuf, label: Option<String>) -> Result<(), String> {
    let (db, snaps) = open(&root)?;
    let label = label.unwrap_or_else(|| "Checkpoint".into());
    let id = uuid::Uuid::new_v4().to_string();
    let info = timemachine::create(&root, &db, &snaps, &id, now_ms(), &label, false)?;
    println!(
        "{GRN}OK{RST} checkpoint {BOLD}{}{RST} captured - {} files\n  {DIM}{}{RST}",
        info.label, info.file_count, info.id
    );
    Ok(())
}

fn cmd_checkpoints(root: PathBuf) -> Result<(), String> {
    let (db, _) = open(&root)?;
    let cps = db.list_checkpoints().map_err(|e| e.to_string())?;
    if cps.is_empty() {
        println!("{DIM}no checkpoints yet — run `syn checkpoint`{RST}");
    }
    for c in cps {
        println!(
            "{DIM}{}{RST}  {BOLD}{:<24}{RST} {} files  {DIM}{}{RST}",
            clock(c.ts), c.label, c.file_count, c.id
        );
    }
    Ok(())
}

fn cmd_restore(root: PathBuf, id: String) -> Result<(), String> {
    let (db, snaps) = open(&root)?;
    let (written, deleted) = timemachine::restore(&root, &db, &snaps, &id)?;
    println!("{GRN}OK{RST} restored - {written} files written, {deleted} removed");
    Ok(())
}

fn cmd_twin(root: PathBuf, json: bool) -> Result<(), String> {
    let g = twin::build(&root);
    if json {
        println!("{}", serde_json::to_string_pretty(&g).unwrap_or_default());
        return Ok(());
    }
    println!(
        "{BOLD}Digital Twin{RST}  {} files · {} dependencies\n",
        g.nodes.len(),
        g.edges.len()
    );
    let mut by_kind: HashMap<&str, Vec<&str>> = HashMap::new();
    for n in &g.nodes {
        by_kind.entry(n.kind.as_str()).or_default().push(n.label.as_str());
    }
    for (kind, mut files) in by_kind {
        files.sort();
        println!("  {CYN}{kind}{RST} {DIM}({}){RST}", files.len());
        for f in files.iter().take(12) {
            println!("    {f}");
        }
        if files.len() > 12 {
            println!("    {DIM}… +{} more{RST}", files.len() - 12);
        }
    }
    Ok(())
}

fn cmd_heatmap(root: PathBuf) -> Result<(), String> {
    let (db, _) = open(&root)?;
    let stats = db.list_file_stats(40).map_err(|e| e.to_string())?;
    if stats.is_empty() {
        println!("{DIM}no activity yet — edit files (or run an agent) here{RST}");
        return Ok(());
    }
    let max = stats.iter().map(|s| s.edits).max().unwrap_or(1).max(1);
    println!("{BOLD}Code Heatmap{RST}  {DIM}(by edits){RST}\n");
    for s in stats {
        let filled = ((s.edits as f64 / max as f64) * 20.0).round() as usize;
        let bar_color = if filled > 13 { RED } else if filled > 6 { YEL } else { CYN };
        let bar = format!("{}{}", "#".repeat(filled), DIM.to_string() + &".".repeat(20 - filled));
        println!("  {bar_color}{bar}{RST}  {:>3} {DIM}edits{RST}  {}", s.edits, s.path);
    }
    Ok(())
}

fn cmd_recap(root: PathBuf, minutes: i64) -> Result<(), String> {
    let (db, _) = open(&root)?;
    let since = now_ms() - minutes * 60_000;
    let events = db.events_since_ts(since).map_err(|e| e.to_string())?;
    let r = recap::build(&events, minutes);
    println!("{BOLD}{CYN}What the AI did{RST}  {DIM}(last {minutes}m){RST}\n");
    for line in &r.lines {
        if line.starts_with("  • ⚠") {
            println!("  {RED}{}{RST}", line.trim_start());
        } else {
            println!("  {line}");
        }
    }
    Ok(())
}

fn cmd_rewind(root: PathBuf, minutes: i64) -> Result<(), String> {
    let (db, snaps) = open(&root)?;
    // Safety: snapshot the current state before rewinding.
    let id = uuid::Uuid::new_v4().to_string();
    let _ = timemachine::create(&root, &db, &snaps, &id, now_ms(), "before rewind", true);
    let at = now_ms() - minutes * 60_000;
    let (written, deleted) = timemachine::restore_at(&root, &db, &snaps, at)?;
    println!(
        "{GRN}OK{RST} rewound ~{minutes}m - {written} files restored, {deleted} removed {DIM}(a 'before rewind' checkpoint was saved){RST}"
    );
    Ok(())
}

fn cmd_policy(root: PathBuf) -> Result<(), String> {
    let _ = open(&root)?;
    println!("{BOLD}Policy guardrails{RST}  {DIM}(built-in defaults){RST}\n");
    for r in policy::default_rules() {
        let (color, tag) = match r.action {
            policy::Action::Deny => (RED, "DENY"),
            policy::Action::Warn => (YEL, "WARN"),
        };
        println!("  {color}{tag:<4}{RST}  {BOLD}{}{RST}  {DIM}~{}{RST}\n        {}", r.name, r.needle, r.message);
    }
    println!("\n  {DIM}Large-change guard: WARN when a single edit exceeds 200 lines.{RST}");
    Ok(())
}

fn cmd_theme(name: Option<String>) -> Result<(), String> {
    match name {
        None => {
            let cur = theme::load_name();
            println!("{BOLD}Themes{RST}\n");
            for t in theme::builtins() {
                let mark = if t.name == cur { format!("{GRN}*{RST}") } else { " ".into() };
                println!("  {mark} {BOLD}{:<14}{RST} {DIM}{}{RST}", t.name, t.label);
            }
            println!("\n  {DIM}set with:  synapse theme <name>   (or live in the TUI palette){RST}");
        }
        Some(n) => {
            if theme::by_name(&n).is_some() {
                theme::save_name(&n);
                println!("{GRN}OK{RST} theme set to {BOLD}{n}{RST}");
            } else {
                return Err(format!("unknown theme '{n}' — run `synapse theme` to list"));
            }
        }
    }
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        None => tui::run(resolve(&None)),
        Some(Cmd::Tui { path }) => tui::run(resolve(&path)),
        Some(Cmd::Run { cmd }) => cmd_run(resolve(&None), cmd),
        Some(Cmd::Watch { path }) => cmd_watch(resolve(&path)),
        Some(Cmd::Dashboard { path }) => dashboard::run(resolve(&path)),
        Some(Cmd::Status { path }) => cmd_status(resolve(&path)),
        Some(Cmd::Log { path, tail, json }) => cmd_log(resolve(&path), tail, json),
        Some(Cmd::Checkpoint { path, label }) => cmd_checkpoint(resolve(&path), label),
        Some(Cmd::Checkpoints { path }) => cmd_checkpoints(resolve(&path)),
        Some(Cmd::Restore { id, path }) => cmd_restore(resolve(&path), id),
        Some(Cmd::Twin { path, json }) => cmd_twin(resolve(&path), json),
        Some(Cmd::Heatmap { path }) => cmd_heatmap(resolve(&path)),
        Some(Cmd::Recap { path, minutes }) => cmd_recap(resolve(&path), minutes),
        Some(Cmd::Rewind { minutes, path }) => cmd_rewind(resolve(&path), minutes),
        Some(Cmd::Policy { path }) => cmd_policy(resolve(&path)),
        Some(Cmd::Theme { name }) => cmd_theme(name),
    };
    if let Err(e) = result {
        eprintln!("{RED}error:{RST} {e}");
        std::process::exit(1);
    }
}
