//! Synapse TUI — a VS Code-style interface that runs *inside any terminal*.
//!
//! Explorer (lazy tree) · multi-tab embedded terminals (real PTYs via vt100) ·
//! file preview + git diff · Git panel · live activity · command palette + agent
//! launcher. No window, no GUI — pure terminal, works everywhere.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseEventKind,
};
use crossterm::execute;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme as SynTheme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use synapse_core::agents::AgentRegistry;
use synapse_core::events::SynapseEvent;
use synapse_core::watcher::{self, EventSink};

use crate::clock;
use crate::theme::Theme;

const HIDDEN: &[&str] = &["node_modules", ".git", "target", ".next", "out", "dist", ".synapse", ".turbo"];

fn clean_cwd(p: &str) -> String {
    let s = p.strip_prefix(r"\\?\").unwrap_or(p);
    // `\\?\UNC\server\share` → `\\server\share`
    if let Some(rest) = s.strip_prefix(r"UNC\") {
        format!(r"\\{rest}")
    } else {
        s.to_string()
    }
}

/// Copy text to the system clipboard. On Windows we use the native `clip.exe`
/// (a fresh process each call → reliable, repeatable, unlike a long-lived
/// clipboard handle); elsewhere the cross-platform clipboard. Returns success.
fn copy_to_clipboard(text: &str) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        if let Ok(mut child) = Command::new("clip")
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            .spawn()
        {
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(text.as_bytes());
                drop(si); // close stdin so clip reads EOF and commits
            }
            if child.wait().map(|s| s.success()).unwrap_or(false) {
                return true;
            }
        }
    }
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_text(text.to_string()))
        .is_ok()
}
fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".into())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
    }
}

// ── Quick-open (fuzzy file finder) ──────────────────────────────────────────

struct QuickOpen {
    query: String,
    sel: usize,
    all: Vec<(String, String)>, // (rel, abs) snapshot
    results: Vec<(String, String)>,
}

/// Bake-off: pick which agents to race on the same task (#7).
struct BakeOff {
    agents: Vec<(&'static str, bool)>,
    sel: usize,
}

fn new_bakeoff() -> BakeOff {
    BakeOff {
        agents: vec![("claude", false), ("gemini", false), ("aider", false), ("codex", false)],
        sel: 0,
    }
}

/// Create an isolated git worktree for an agent (so they don't collide). Falls
/// back to the project root if git isn't available.
fn create_worktree(root: &Path, agent: &str) -> PathBuf {
    let wt = root.join(".synapse").join("bakeoff").join(agent);
    if let Some(parent) = wt.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let branch = format!("synapse/bakeoff-{agent}");
    let ok = Command::new("git")
        .arg("-C")
        .arg(clean_cwd(&root.to_string_lossy()))
        .args(["worktree", "add", "--force", "-B", &branch])
        .arg(&wt)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if ok {
        wt
    } else {
        root.to_path_buf()
    }
}

/// Recursively collect project files (rel, abs), honoring ignores. Capped.
fn walk_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    if out.len() >= 8000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if HIDDEN.contains(&name.as_str()) {
            continue;
        }
        let p = e.path();
        match e.file_type() {
            Ok(ft) if ft.is_dir() => walk_files(root, &p, out),
            Ok(ft) if ft.is_file() => {
                let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                out.push((rel, p.to_string_lossy().to_string()));
            }
            _ => {}
        }
        if out.len() >= 8000 {
            return;
        }
    }
}

fn fuzzy(files: &[(String, String)], q: &str) -> Vec<(String, String)> {
    if q.is_empty() {
        return files.iter().take(40).cloned().collect();
    }
    let m = SkimMatcherV2::default();
    let mut scored: Vec<(i64, (String, String))> = files
        .iter()
        .filter_map(|f| m.fuzzy_match(&f.0, q).map(|s| (s, f.clone())))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(40).map(|(_, f)| f).collect()
}

// ── Syntax highlighting ─────────────────────────────────────────────────────

fn ext_of(name: &str) -> String {
    name.rsplit('.').next().unwrap_or("txt").to_string()
}

fn pick_syn(ts: &ThemeSet, name: &str) -> SynTheme {
    ts.themes.get(name).or_else(|| ts.themes.values().next()).cloned().unwrap_or_default()
}

fn highlight(content: &str, ext: &str, ps: &SyntaxSet, theme: &SynTheme) -> Vec<Line<'static>> {
    let syntax = ps.find_syntax_by_extension(ext).unwrap_or_else(|| ps.find_syntax_plain_text());
    let mut h = HighlightLines::new(syntax, theme);
    let mut out: Vec<Line<'static>> = Vec::new();
    for line in LinesWithEndings::from(content) {
        let ranges = h.highlight_line(line, ps).unwrap_or_default();
        let spans: Vec<Span> = ranges
            .into_iter()
            .map(|(st, text)| {
                let fg = st.foreground;
                Span::styled(
                    text.trim_end_matches(['\n', '\r']).to_string(),
                    Style::default().fg(Color::Rgb(fg.r, fg.g, fg.b)),
                )
            })
            .collect();
        out.push(Line::from(spans));
        if out.len() >= 6000 {
            break;
        }
    }
    if out.is_empty() {
        out.push(Line::from(""));
    }
    out
}

/// Read + syntax-highlight a file into ratatui lines (guards big/binary files).
fn open_file(abs: &str, name: &str, ps: &SyntaxSet, theme: &SynTheme) -> Vec<Line<'static>> {
    match std::fs::read(clean_cwd(abs)) {
        Ok(bytes) if bytes.len() <= 1_000_000 => match String::from_utf8(bytes) {
            Ok(s) => highlight(&s, &ext_of(name), ps, theme),
            Err(_) => vec![Line::from("<binary file>")],
        },
        Ok(_) => vec![Line::from("<file too large to preview>")],
        Err(_) => vec![Line::from("<unreadable>")],
    }
}

// ── Explorer tree ──────────────────────────────────────────────────────────

struct TNode {
    name: String,
    path: String,
    is_dir: bool,
    expanded: bool,
    children: Option<Vec<TNode>>,
}

fn read_dir_nodes(path: &str) -> Vec<TNode> {
    let mut v: Vec<TNode> = std::fs::read_dir(clean_cwd(path))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    if HIDDEN.contains(&name.as_str()) {
                        return None;
                    }
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    Some(TNode {
                        name,
                        path: e.path().to_string_lossy().to_string(),
                        is_dir,
                        expanded: false,
                        children: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    v
}

struct FlatRow {
    path: String,
    name: String,
    is_dir: bool,
    depth: usize,
    expanded: bool,
}

fn flatten(nodes: &[TNode], depth: usize, out: &mut Vec<FlatRow>) {
    for n in nodes {
        out.push(FlatRow {
            path: n.path.clone(),
            name: n.name.clone(),
            is_dir: n.is_dir,
            depth,
            expanded: n.expanded,
        });
        if n.is_dir && n.expanded {
            if let Some(ch) = &n.children {
                flatten(ch, depth + 1, out);
            }
        }
    }
}

fn toggle(nodes: &mut [TNode], path: &str) -> bool {
    for n in nodes.iter_mut() {
        if n.path == path {
            n.expanded = !n.expanded;
            if n.expanded && n.children.is_none() {
                n.children = Some(read_dir_nodes(&n.path));
            }
            return true;
        }
        if n.is_dir && n.expanded {
            if let Some(ch) = n.children.as_mut() {
                if toggle(ch, path) {
                    return true;
                }
            }
        }
    }
    false
}

// ── Embedded terminal tab ──────────────────────────────────────────────────

struct Term {
    title: String,
    parser: vt100::Parser,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    rx: Receiver<Vec<u8>>,
}

fn spawn_term(cwd: &Path, title: &str) -> Result<Term, String> {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| e.to_string())?;
    let mut cmd = CommandBuilder::new(default_shell());
    cmd.cwd(clean_cwd(&cwd.to_string_lossy()));
    let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let (tx, rx) = channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    Ok(Term {
        title: title.to_string(),
        parser: vt100::Parser::new(24, 80, 4000),
        writer,
        master: pair.master,
        child,
        rx,
    })
}

// ── vt100 → ratatui ────────────────────────────────────────────────────────

fn conv(c: vt100::Color) -> Option<Color> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// A normalized, inner-relative text selection: (start_col, start_row, end_col, end_row).
type Sel = (u16, u16, u16, u16);

/// Clamp an absolute drag (anchor→cursor) into a normalized inner-relative selection.
fn norm_sel(inner: Rect, a: Sel) -> Option<Sel> {
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let cx = |c: u16| c.clamp(inner.x, inner.x + inner.width - 1) - inner.x;
    let ry = |r: u16| r.clamp(inner.y, inner.y + inner.height - 1) - inner.y;
    let (ac, ar, cc, cr) = a;
    let p1 = (ry(ar), cx(ac));
    let p2 = (ry(cr), cx(cc));
    let (s, e) = if p1 <= p2 { (p1, p2) } else { (p2, p1) };
    if s == e {
        return None; // a plain click, not a drag
    }
    Some((s.1, s.0, e.1, e.0))
}

fn cell_selected(row: u16, col: u16, sel: Option<Sel>) -> bool {
    if let Some((sc, sr, ec, er)) = sel {
        let ge = row > sr || (row == sr && col >= sc);
        let le = row < er || (row == er && col <= ec);
        ge && le
    } else {
        false
    }
}

/// Extract the selected text from a terminal screen (for clipboard copy).
fn extract_sel(screen: &vt100::Screen, sel: Sel) -> String {
    let (_, cols) = screen.size();
    let (sc, sr, ec, er) = sel;
    let mut out = String::new();
    for row in sr..=er {
        let c0 = if row == sr { sc } else { 0 };
        let c1 = if row == er { ec } else { cols.saturating_sub(1) };
        let mut line = String::new();
        for col in c0..=c1 {
            if let Some(cell) = screen.cell(row, col) {
                let s = cell.contents();
                line.push_str(if s.is_empty() { " " } else { &s });
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn screen_to_text(screen: &vt100::Screen, sel: Option<Sel>) -> Text<'static> {
    let (rows, cols) = screen.size();
    let mut lines: Vec<Line> = Vec::with_capacity(rows as usize);
    for row in 0..rows {
        let mut spans: Vec<Span> = Vec::new();
        let mut run = String::new();
        let mut run_style: Option<Style> = None;
        for col in 0..cols {
            let (ch, base) = match screen.cell(row, col) {
                Some(cell) => {
                    let mut st = Style::default();
                    if let Some(c) = conv(cell.fgcolor()) {
                        st = st.fg(c);
                    }
                    if let Some(c) = conv(cell.bgcolor()) {
                        st = st.bg(c);
                    }
                    if cell.bold() {
                        st = st.add_modifier(Modifier::BOLD);
                    }
                    if cell.inverse() {
                        st = st.add_modifier(Modifier::REVERSED);
                    }
                    let c = cell.contents();
                    (if c.is_empty() { " ".to_string() } else { c }, st)
                }
                None => (" ".to_string(), Style::default()),
            };
            let style = if cell_selected(row, col, sel) {
                base.add_modifier(Modifier::REVERSED)
            } else {
                base
            };
            if run_style == Some(style) {
                run.push_str(&ch);
            } else {
                if let Some(s) = run_style {
                    spans.push(Span::styled(std::mem::take(&mut run), s));
                }
                run = ch;
                run_style = Some(style);
            }
        }
        if let Some(s) = run_style {
            spans.push(Span::styled(run, s));
        }
        lines.push(Line::from(spans));
    }
    Text::from(lines)
}

fn key_to_bytes(code: KeyCode, mods: KeyModifiers) -> Option<Vec<u8>> {
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let bytes = match code {
        KeyCode::Char(c) => {
            if ctrl && c.is_ascii_alphabetic() {
                vec![(c.to_ascii_uppercase() as u8) & 0x1f]
            } else {
                c.to_string().into_bytes()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        _ => return None,
    };
    Some(bytes)
}

// ── git ────────────────────────────────────────────────────────────────────

fn git(root: &PathBuf, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(clean_cwd(&root.to_string_lossy()))
        .args(args)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        None
    }
}

struct GitState {
    branch: String,
    changes: Vec<(String, String)>, // (status, path)
    sel: usize,
}

fn refresh_git(root: &PathBuf) -> GitState {
    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "—".into());
    let changes = git(root, &["status", "--porcelain"])
        .map(|s| {
            s.lines()
                .filter(|l| l.len() > 3)
                .map(|l| (l[..2].trim().to_string(), l[3..].trim().to_string()))
                .collect()
        })
        .unwrap_or_default();
    GitState { branch, changes, sel: 0 }
}

// ── App ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Explorer,
    Center,
    Git,
    Activity,
}

#[derive(Clone, Copy, PartialEq)]
enum Center {
    Term,
    Editor,
    Diff,
}

struct Palette {
    query: String,
    sel: usize,
}

const ACTIONS: &[(&str, &str)] = &[
    ("New terminal", "new"),
    ("Launch Claude", "claude"),
    ("Launch Gemini", "gemini"),
    ("Launch Aider", "aider"),
    ("Launch Codex", "codex"),
    ("Agent bake-off (pick agents)", "bakeoff"),
    ("Recap - what did the AI do?", "recap"),
    ("Close terminal", "close"),
    ("Show terminal", "term"),
    ("Quit", "quit"),
];

struct Regions {
    top: Rect,
    explorer: Rect,
    tabs: Rect,
    content: Rect,
    git: Rect,
    activity: Rect,
    status: Rect,
}

/// User-adjustable pane sizes (drag the borders with the mouse).
struct Panes {
    exp_w: u16,
    right_w: u16,
    hsplit: u16, // git/activity vertical split %
}

fn layout(full: Rect, p: &Panes) -> Regions {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .split(full);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(p.exp_w), Constraint::Min(16), Constraint::Length(p.right_w)])
        .split(rows[1]);
    let center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(body[1]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(p.hsplit), Constraint::Percentage(100 - p.hsplit)])
        .split(body[2]);
    Regions {
        top: rows[0],
        explorer: body[0],
        tabs: center[0],
        content: center[1],
        git: right[0],
        activity: right[1],
        status: rows[2],
    }
}

pub fn run(root: PathBuf) -> Result<(), String> {
    let (db, snaps) = crate::open(&root)?;
    let agents: AgentRegistry = Arc::new(Mutex::new(HashMap::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let sink: EventSink = Arc::new(|_e: &SynapseEvent| {});
    let _watcher = watcher::start(root.clone(), db.clone(), snaps, sink, "workspace".into(), agents.clone())
        .map_err(|e| e.to_string())?;
    crate::spawn_scanner(root.to_string_lossy().to_string(), db.clone(), agents, stop.clone());

    // Syntax highlighting (loaded once; fancy-regex backend, no C deps).
    let ps = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    // Active UI theme + its paired syntect theme (both switchable live).
    let mut ui_theme = crate::theme::load();
    let mut syn_theme = pick_syn(&ts, ui_theme.syntect);

    // Project file index for fuzzy quick-open (built in the background).
    let files: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let files = files.clone();
        let root2 = root.clone();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            walk_files(&root2, &root2, &mut out);
            *files.lock().unwrap() = out;
        });
    }

    // State
    let mut tree = read_dir_nodes(&root.to_string_lossy());
    let mut rows: Vec<FlatRow> = Vec::new();
    flatten(&tree, 0, &mut rows);
    let mut exp_sel = 0usize;

    let mut terms = vec![spawn_term(&root, "shell")?];
    let mut active = 0usize;
    let mut center = Center::Term;
    let mut focus = Focus::Center;

    let mut editor_title = String::new();
    let mut editor: Vec<Line<'static>> = Vec::new();
    let mut editor_scroll = 0usize;
    let mut diff_title = String::new();
    let mut diff: Vec<String> = Vec::new();
    let mut diff_scroll = 0usize;

    let mut gitst = refresh_git(&root);
    let mut palette: Option<Palette> = None;
    let mut quick: Option<QuickOpen> = None;
    let mut bakeoff: Option<BakeOff> = None;
    let mut last_size: (u16, u16) = (0, 0);
    let mut status_msg = String::new();

    // Restore the terminal even on panic — a crash must never wreck the shell.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        let _ = ratatui::restore();
        prev_hook(info);
    }));

    let mut term = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let mut panes = Panes { exp_w: 28, right_w: 40, hsplit: 45 };
    let mut drag: Option<u8> = None; // 0=explorer|center, 1=center|right, 2=git/activity
    let mut sel: Option<Sel> = None; // mouse text selection in the terminal pane
    let mut intro = true; // usage guideline shown on every launch
    let mut dirty = true;
    let mut last_beat = std::time::Instant::now();
    let mut last_git = std::time::Instant::now();
    let res = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        for t in terms.iter_mut() {
            while let Ok(chunk) = t.rx.try_recv() {
                t.parser.process(&chunk);
                dirty = true;
            }
        }

        // Resize active terminal to the content pane.
        if let Ok(sz) = term.size() {
            let r = layout(Rect::new(0, 0, sz.width, sz.height), &panes);
            let cols = r.content.width.saturating_sub(2);
            let prows = r.content.height.saturating_sub(2);
            if (cols, prows) != last_size && cols > 0 && prows > 0 {
                last_size = (cols, prows);
                for t in terms.iter_mut() {
                    t.parser.set_size(prows, cols);
                    let _ = t.master.resize(PtySize { rows: prows, cols, pixel_width: 0, pixel_height: 0 });
                }
                dirty = true;
            }
        }

        // Heartbeat: refresh activity/clock ~1.4×/s; git status every ~2s.
        if last_beat.elapsed() >= Duration::from_millis(700) {
            last_beat = std::time::Instant::now();
            dirty = true;
        }
        if last_git.elapsed() >= Duration::from_secs(2) {
            last_git = std::time::Instant::now();
            let gsel = gitst.sel;
            gitst = refresh_git(&root);
            gitst.sel = gsel.min(gitst.changes.len().saturating_sub(1));
            dirty = true;
        }

        // Draw only when something changed (event-driven, low idle CPU).
        if dirty {
            let recent = db.recent_events(60).unwrap_or_default();
            let sessions = db.list_sessions().unwrap_or_default();
            if let Err(e) = term.draw(|f| {
                ui(
                    f, &ui_theme, &panes, &terms, active, center, &rows, exp_sel, &recent, sessions.len(), &gitst,
                    focus, &editor, editor_scroll, &editor_title, &diff, diff_scroll, &diff_title,
                    &palette, &quick, &bakeoff, &sel, intro, &root, &status_msg,
                );
            }) {
                break Err(e.to_string());
            }
            dirty = false;
        }

        if !matches!(event::poll(Duration::from_millis(50)), Ok(true)) {
            continue;
        }
        dirty = true;
        let Ok(ev) = event::read() else { continue };

        // ── Mouse: drag pane borders to resize, click to focus, wheel scrolls ──
        if let Event::Mouse(me) = ev {
            let sz = term.size().unwrap_or_default();
            let full = Rect::new(0, 0, sz.width, sz.height);
            let r = layout(full, &panes);
            let exp_edge = r.explorer.x + r.explorer.width; // explorer | center
            let right_edge = r.git.x; // center | right
            let hsplit_y = r.activity.y; // git | activity
            let near = |a: u16, b: u16| (a as i32 - b as i32).abs() <= 1;
            let inside = |rect: Rect, x: u16, y: u16| {
                x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
            };
            match me.kind {
                MouseEventKind::Down(_) => {
                    if me.row == r.tabs.y && me.column >= r.tabs.x {
                        // Click a terminal tab in the tab bar to switch to it.
                        let mut x = r.tabs.x;
                        for (i, t) in terms.iter().enumerate() {
                            let w = format!(" {}:{} ", i + 1, t.title).chars().count() as u16;
                            if me.column >= x && me.column < x + w {
                                active = i;
                                center = Center::Term;
                                focus = Focus::Center;
                                last_size = (0, 0);
                                break;
                            }
                            x += w;
                        }
                    } else if near(me.column, exp_edge) {
                        drag = Some(0);
                    } else if near(me.column, right_edge) {
                        drag = Some(1);
                    } else if me.column >= r.git.x && near(me.row, hsplit_y) {
                        drag = Some(2);
                    } else if inside(r.explorer, me.column, me.row) {
                        focus = Focus::Explorer;
                        // Map the click to a row (skip the top border) and open it.
                        let idx = (me.row.saturating_sub(r.explorer.y + 1)) as usize;
                        if let Some(rr) = rows.get(idx) {
                            exp_sel = idx;
                            let (is_dir, path, name) = (rr.is_dir, rr.path.clone(), rr.name.clone());
                            if is_dir {
                                toggle(&mut tree, &path);
                                rows.clear();
                                flatten(&tree, 0, &mut rows);
                                exp_sel = exp_sel.min(rows.len().saturating_sub(1));
                            } else {
                                editor_title = name.clone();
                                editor = open_file(&path, &name, &ps, &syn_theme);
                                editor_scroll = 0;
                                center = Center::Editor;
                                focus = Focus::Center;
                            }
                        }
                    } else if inside(r.content, me.column, me.row) {
                        focus = Focus::Center;
                        // Start a text selection in the terminal pane.
                        if center == Center::Term {
                            sel = Some((me.column, me.row, me.column, me.row));
                        }
                    } else if inside(r.git, me.column, me.row) {
                        focus = Focus::Git;
                    } else if inside(r.activity, me.column, me.row) {
                        focus = Focus::Activity;
                    }
                }
                // Some terminals report a press-drag as Moved — handle both.
                MouseEventKind::Drag(_) | MouseEventKind::Moved => {
                    if drag.is_some() {
                        match drag {
                            Some(0) => {
                                panes.exp_w = me.column.clamp(14, full.width.saturating_sub(34));
                                last_size = (0, 0);
                            }
                            Some(1) => {
                                panes.right_w =
                                    full.width.saturating_sub(me.column).clamp(20, full.width.saturating_sub(40));
                                last_size = (0, 0);
                            }
                            Some(2) => {
                                let body_h = full.height.saturating_sub(2).max(1);
                                let rel = me.row.saturating_sub(r.git.y);
                                panes.hsplit = ((rel as u32 * 100 / body_h as u32) as u16).clamp(20, 80);
                            }
                            _ => {}
                        }
                    } else if let Some(s) = sel.as_mut() {
                        // Extend the terminal text selection.
                        s.2 = me.column;
                        s.3 = me.row;
                    } else {
                        // Plain hover with nothing active — don't waste a redraw.
                        dirty = false;
                    }
                }
                MouseEventKind::Up(_) => {
                    drag = None;
                    // Finish a selection → copy it to the clipboard.
                    if let Some(s) = sel.take() {
                        let content = r.content;
                        let inner = Rect::new(
                            content.x + 1,
                            content.y + 1,
                            content.width.saturating_sub(2),
                            content.height.saturating_sub(2),
                        );
                        if center == Center::Term {
                            if let Some(ns) = norm_sel(inner, s) {
                                let text = extract_sel(terms[active].parser.screen(), ns);
                                if !text.trim().is_empty() && copy_to_clipboard(&text) {
                                    status_msg = format!("copied {} chars", text.chars().count());
                                }
                            }
                        }
                    }
                }
                MouseEventKind::ScrollUp => match focus {
                    Focus::Explorer => exp_sel = exp_sel.saturating_sub(1),
                    Focus::Git => gitst.sel = gitst.sel.saturating_sub(1),
                    Focus::Center if center == Center::Editor => editor_scroll = editor_scroll.saturating_sub(3),
                    Focus::Center if center == Center::Diff => diff_scroll = diff_scroll.saturating_sub(3),
                    _ => {}
                },
                MouseEventKind::ScrollDown => match focus {
                    Focus::Explorer => exp_sel = (exp_sel + 1).min(rows.len().saturating_sub(1)),
                    Focus::Git => {
                        if gitst.sel + 1 < gitst.changes.len() {
                            gitst.sel += 1;
                        }
                    }
                    Focus::Center if center == Center::Editor => editor_scroll += 3,
                    Focus::Center if center == Center::Diff => diff_scroll += 3,
                    _ => {}
                },
                _ => {}
            }
            continue;
        }

        let Event::Key(k) = ev else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }

        // Usage guideline (shown every launch): any key dismisses it.
        if intro {
            intro = false;
            continue;
        }

        // Palette captures everything while open.
        if let Some(p) = palette.as_mut() {
            match k.code {
                KeyCode::Esc => palette = None,
                KeyCode::Up => p.sel = p.sel.saturating_sub(1),
                KeyCode::Down => p.sel += 1,
                KeyCode::Backspace => {
                    p.query.pop();
                    p.sel = 0;
                }
                KeyCode::Char(c) => {
                    p.query.push(c);
                    p.sel = 0;
                }
                KeyCode::Enter => {
                    let filtered = filter_actions(&p.query);
                    if let Some((_, id)) =
                        filtered.get(p.sel.min(filtered.len().saturating_sub(1))).cloned()
                    {
                        palette = None;
                        match id.as_str() {
                            "quit" => break Ok(()),
                            "new" => {
                                if let Ok(t) = spawn_term(&root, "shell") {
                                    terms.push(t);
                                    active = terms.len() - 1;
                                    center = Center::Term;
                                    last_size = (0, 0);
                                }
                            }
                            "close" => {
                                if terms.len() > 1 {
                                    let mut t = terms.remove(active);
                                    let _ = t.child.kill();
                                    active = active.min(terms.len() - 1);
                                }
                            }
                            "term" => center = Center::Term,
                            "bakeoff" => bakeoff = Some(new_bakeoff()),
                            "recap" => {
                                let since = crate::now_ms() - 30 * 60_000;
                                let events = db.events_since_ts(since).unwrap_or_default();
                                let r = synapse_core::recap::build(&events, 30);
                                editor_title = "Recap (30m)".into();
                                editor = r.lines.iter().map(|l| Line::from(l.clone())).collect();
                                editor_scroll = 0;
                                center = Center::Editor;
                                focus = Focus::Center;
                            }
                            t if t.starts_with("theme:") => {
                                let name = t[6..].to_string();
                                if let Some(th) = crate::theme::by_name(&name) {
                                    ui_theme = th;
                                    syn_theme = pick_syn(&ts, ui_theme.syntect);
                                    crate::theme::save_name(&name);
                                    status_msg = format!("theme: {name}");
                                }
                            }
                            agent => {
                                // Launch an agent in a fresh terminal tab.
                                if let Ok(t) = spawn_term(&root, agent) {
                                    terms.push(t);
                                    active = terms.len() - 1;
                                    center = Center::Term;
                                    last_size = (0, 0);
                                    let _ = terms[active].writer.write_all(format!("{agent}\r").as_bytes());
                                    let _ = terms[active].writer.flush();
                                    status_msg = format!("launched {agent}");
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            continue;
        }

        // Bake-off agent picker captures everything while open.
        if let Some(bo) = bakeoff.as_mut() {
            match k.code {
                KeyCode::Esc => bakeoff = None,
                KeyCode::Up => bo.sel = bo.sel.saturating_sub(1),
                KeyCode::Down => {
                    if bo.sel + 1 < bo.agents.len() {
                        bo.sel += 1;
                    }
                }
                KeyCode::Char(' ') => {
                    let s = &mut bo.agents[bo.sel];
                    s.1 = !s.1;
                }
                KeyCode::Enter => {
                    let chosen: Vec<&'static str> =
                        bo.agents.iter().filter(|(_, on)| *on).map(|(a, _)| *a).collect();
                    bakeoff = None;
                    for a in &chosen {
                        let wt = create_worktree(&root, a);
                        if let Ok(t) = spawn_term(&wt, a) {
                            terms.push(t);
                            active = terms.len() - 1;
                            center = Center::Term;
                            last_size = (0, 0);
                            let _ = terms[active].writer.write_all(format!("{a}\r").as_bytes());
                            let _ = terms[active].writer.flush();
                        }
                    }
                    if !chosen.is_empty() {
                        status_msg = format!("bake-off: {} agents launched in worktrees", chosen.len());
                    }
                }
                _ => {}
            }
            continue;
        }

        // Quick-open (fuzzy file finder) captures everything while open.
        if let Some(qo) = quick.as_mut() {
            match k.code {
                KeyCode::Esc => quick = None,
                KeyCode::Up => qo.sel = qo.sel.saturating_sub(1),
                KeyCode::Down => qo.sel += 1,
                KeyCode::Backspace => {
                    qo.query.pop();
                    qo.results = fuzzy(&qo.all, &qo.query);
                    qo.sel = 0;
                }
                KeyCode::Char(c) => {
                    qo.query.push(c);
                    qo.results = fuzzy(&qo.all, &qo.query);
                    qo.sel = 0;
                }
                KeyCode::Enter => {
                    if let Some((rel, abs)) =
                        qo.results.get(qo.sel.min(qo.results.len().saturating_sub(1))).cloned()
                    {
                        quick = None;
                        editor_title = rel.clone();
                        editor = open_file(&abs, &rel, &ps, &syn_theme);
                        editor_scroll = 0;
                        center = Center::Editor;
                        focus = Focus::Center;
                    }
                }
                _ => {}
            }
            continue;
        }

        // Globals.
        if k.code == KeyCode::Char('q') && k.modifiers.contains(KeyModifiers::CONTROL) {
            break Ok(());
        }
        // Ctrl+P → fuzzy quick-open (VS Code style).
        if k.code == KeyCode::Char('p')
            && k.modifiers.contains(KeyModifiers::CONTROL)
            && !k.modifiers.contains(KeyModifiers::SHIFT)
        {
            let all = files.lock().unwrap().clone();
            let results = fuzzy(&all, "");
            quick = Some(QuickOpen { query: String::new(), sel: 0, all, results });
            continue;
        }
        let palette_combo = k.code == KeyCode::F(1)
            || (k.code == KeyCode::Char('p')
                && k.modifiers.contains(KeyModifiers::CONTROL)
                && k.modifiers.contains(KeyModifiers::SHIFT));
        if palette_combo {
            palette = Some(Palette { query: String::new(), sel: 0 });
            continue;
        }
        if k.code == KeyCode::F(6) {
            focus = match focus {
                Focus::Explorer => Focus::Center,
                Focus::Center => Focus::Git,
                Focus::Git => Focus::Activity,
                Focus::Activity => Focus::Explorer,
            };
            continue;
        }
        if k.code == KeyCode::F(2) {
            if let Ok(t) = spawn_term(&root, "shell") {
                terms.push(t);
                active = terms.len() - 1;
                center = Center::Term;
                last_size = (0, 0);
            }
            continue;
        }
        // F7 / F8 — previous / next terminal tab (reliable everywhere).
        if k.code == KeyCode::F(7) && !terms.is_empty() {
            active = (active + terms.len() - 1) % terms.len();
            center = Center::Term;
            last_size = (0, 0);
            continue;
        }
        if k.code == KeyCode::F(8) && !terms.is_empty() {
            active = (active + 1) % terms.len();
            center = Center::Term;
            last_size = (0, 0);
            continue;
        }
        // Alt+number switches terminal tabs.
        if k.modifiers.contains(KeyModifiers::ALT) {
            if let KeyCode::Char(c) = k.code {
                if let Some(d) = c.to_digit(10) {
                    let idx = (d as usize).wrapping_sub(1);
                    if idx < terms.len() {
                        active = idx;
                        center = Center::Term;
                        last_size = (0, 0);
                    }
                    continue;
                }
            }
        }

        match focus {
            Focus::Center if center == Center::Term => {
                if let Some(bytes) = key_to_bytes(k.code, k.modifiers) {
                    let _ = terms[active].writer.write_all(&bytes);
                    let _ = terms[active].writer.flush();
                }
            }
            Focus::Center => match k.code {
                KeyCode::Up => {
                    let s = if center == Center::Editor { &mut editor_scroll } else { &mut diff_scroll };
                    *s = s.saturating_sub(1);
                }
                KeyCode::Down => {
                    let s = if center == Center::Editor { &mut editor_scroll } else { &mut diff_scroll };
                    *s += 1;
                }
                KeyCode::Esc => center = Center::Term,
                _ => {}
            },
            Focus::Explorer => match k.code {
                KeyCode::Up => exp_sel = exp_sel.saturating_sub(1),
                KeyCode::Down => {
                    if exp_sel + 1 < rows.len() {
                        exp_sel += 1;
                    }
                }
                // Copy the selected path to the clipboard (y = yank, c = copy).
                KeyCode::Char('y') | KeyCode::Char('c') => {
                    if let Some(r) = rows.get(exp_sel) {
                        let path = clean_cwd(&r.path);
                        status_msg = if copy_to_clipboard(&path) {
                            format!("copied {path}")
                        } else {
                            "clipboard unavailable".into()
                        };
                    }
                }
                KeyCode::Enter | KeyCode::Right | KeyCode::Left => {
                    if let Some(r) = rows.get(exp_sel) {
                        if r.is_dir {
                            toggle(&mut tree, &r.path);
                            rows.clear();
                            flatten(&tree, 0, &mut rows);
                            exp_sel = exp_sel.min(rows.len().saturating_sub(1));
                        } else {
                            editor_title = r.name.clone();
                            editor = open_file(&r.path, &r.name, &ps, &syn_theme);
                            editor_scroll = 0;
                            center = Center::Editor;
                            focus = Focus::Center;
                        }
                    }
                }
                _ => {}
            },
            Focus::Git => match k.code {
                KeyCode::Up => gitst.sel = gitst.sel.saturating_sub(1),
                KeyCode::Down => {
                    if gitst.sel + 1 < gitst.changes.len() {
                        gitst.sel += 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some((_, path)) = gitst.changes.get(gitst.sel) {
                        diff_title = path.clone();
                        diff = git(&root, &["diff", "--", path])
                            .map(|s| s.lines().map(|l| l.to_string()).collect())
                            .filter(|v: &Vec<String>| !v.is_empty())
                            .unwrap_or_else(|| vec!["(no unstaged diff — maybe staged or new file)".into()]);
                        diff_scroll = 0;
                        center = Center::Diff;
                        focus = Focus::Center;
                    }
                }
                _ => {}
            },
            Focus::Activity => {}
        }
    };

    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    stop.store(true, Ordering::Relaxed);
    for t in terms.iter_mut() {
        let _ = t.child.kill();
    }
    res
}

fn filter_actions(q: &str) -> Vec<(String, String)> {
    let ql = q.to_lowercase();
    let mut all: Vec<(String, String)> =
        ACTIONS.iter().map(|(l, id)| (l.to_string(), id.to_string())).collect();
    for t in crate::theme::builtins() {
        all.push((format!("Theme: {}", t.label), format!("theme:{}", t.name)));
    }
    all.into_iter()
        .filter(|(label, _)| ql.is_empty() || label.to_lowercase().contains(&ql))
        .collect()
}

fn border(theme: &Theme, on: bool) -> Style {
    if on {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

#[allow(non_snake_case)]
fn explorer_list(theme: &Theme, rows: &[FlatRow], exp_sel: usize, focus: Focus) -> List<'static> {
    let (PLASMA, SEL) = (theme.accent, theme.selection);
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let arrow = if row.is_dir {
                if row.expanded { "▾ " } else { "▸ " }
            } else {
                "  "
            };
            let pad = "  ".repeat(row.depth);
            let mut st = Style::default().fg(if row.is_dir { PLASMA } else { theme.fg });
            if i == exp_sel && focus == Focus::Explorer {
                st = st.bg(SEL).add_modifier(Modifier::BOLD);
            }
            ListItem::new(Line::from(Span::styled(format!("{pad}{arrow}{}", row.name), st)))
        })
        .collect();
    List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(border(theme, focus == Focus::Explorer)).title(" Explorer  (y: copy path) "))
}

#[allow(non_snake_case)]
fn git_list(theme: &Theme, gitst: &GitState, focus: Focus) -> List<'static> {
    let (CYAN, EMERALD, ROSE, MUTED, SEL) =
        (theme.accent2, theme.ok, theme.err, theme.muted, theme.selection);
    let mut lines: Vec<ListItem> = vec![ListItem::new(Line::from(vec![
        Span::styled("⎇ ", Style::default().fg(MUTED)),
        Span::styled(gitst.branch.clone(), Style::default().fg(CYAN)),
        Span::styled(format!("   {} changed", gitst.changes.len()), Style::default().fg(MUTED)),
    ]))];
    for (i, (stt, path)) in gitst.changes.iter().enumerate() {
        let mut st = Style::default().fg(theme.fg);
        if i == gitst.sel && focus == Focus::Git {
            st = st.bg(SEL).add_modifier(Modifier::BOLD);
        }
        let color = if stt.contains('M') {
            CYAN
        } else if stt.contains('A') || stt == "??" {
            EMERALD
        } else if stt.contains('D') {
            ROSE
        } else {
            MUTED
        };
        lines.push(ListItem::new(Line::from(vec![
            Span::styled(format!(" {stt:<2} "), Style::default().fg(color)),
            Span::styled(path.clone(), st),
        ])));
    }
    List::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(border(theme, focus == Focus::Git)).title(" Git  (Enter = diff) "))
}

fn activity_list(theme: &Theme, recent: &[serde_json::Value], focus: Focus) -> List<'static> {
    let items: Vec<ListItem> = recent
        .iter()
        .map(|v| {
            let ts = v.get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
            let agent = v.get("agent").and_then(|a| a.as_str()).unwrap_or("?");
            let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("");
            ListItem::new(Line::from(vec![
                Span::styled(clock(ts), Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(format!("{agent:>6}"), Style::default().fg(theme.accent)),
                Span::raw(" "),
                Span::styled(title.to_string(), Style::default().fg(theme.fg)),
            ]))
        })
        .collect();
    List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(border(theme, focus == Focus::Activity)).title(" Live Activity "))
}

#[allow(clippy::too_many_arguments)]
#[allow(non_snake_case)]
fn ui(
    f: &mut Frame,
    theme: &Theme,
    panes: &Panes,
    terms: &[Term],
    active: usize,
    center: Center,
    rows: &[FlatRow],
    exp_sel: usize,
    recent: &[serde_json::Value],
    _agent_count: usize,
    gitst: &GitState,
    focus: Focus,
    editor: &[Line<'static>],
    editor_scroll: usize,
    editor_title: &str,
    diff: &[String],
    diff_scroll: usize,
    diff_title: &str,
    palette: &Option<Palette>,
    quick: &Option<QuickOpen>,
    bakeoff: &Option<BakeOff>,
    sel: &Option<Sel>,
    intro: bool,
    root: &PathBuf,
    status_msg: &str,
) {
    let r = layout(f.area(), panes);
    let (PLASMA, CYAN, EMERALD, ROSE, MUTED) =
        (theme.accent, theme.accent2, theme.ok, theme.err, theme.muted);

    let name = root.to_string_lossy().replace('\\', "/");
    let name = name.trim_end_matches('/').rsplit('/').next().unwrap_or("project");
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" SYNAPSE ", Style::default().fg(Color::Black).bg(PLASMA).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {name}"), Style::default().fg(CYAN)),
        ])),
        r.top,
    );

    // Explorer
    f.render_widget(explorer_list(theme, rows, exp_sel, focus), r.explorer);

    // Center tab bar
    let mut tabs: Vec<Span> = Vec::new();
    for (i, t) in terms.iter().enumerate() {
        let on = center == Center::Term && i == active;
        let st = if on { Style::default().fg(Color::Black).bg(CYAN) } else { Style::default().fg(MUTED) };
        tabs.push(Span::styled(format!(" {}:{} ", i + 1, t.title), st));
    }
    if center == Center::Editor {
        tabs.push(Span::styled(format!(" ✎ {editor_title} "), Style::default().fg(Color::Black).bg(EMERALD)));
    }
    if center == Center::Diff {
        tabs.push(Span::styled(format!(" ± {diff_title} "), Style::default().fg(Color::Black).bg(Color::Rgb(251, 191, 36))));
    }
    f.render_widget(Paragraph::new(Line::from(tabs)), r.tabs);

    // Center content
    let in_term = center == Center::Term;
    let cblock = Block::default()
        .borders(Borders::ALL)
        .border_style(border(theme, focus == Focus::Center))
        .title(match center {
            Center::Term => " Terminal ",
            Center::Editor => " Preview ",
            Center::Diff => " Diff ",
        });
    let inner = cblock.inner(r.content);
    f.render_widget(cblock, r.content);
    match center {
        Center::Term => {
            let t = &terms[active];
            let sel_rel = (*sel).and_then(|s| norm_sel(inner, s));
            f.render_widget(Paragraph::new(screen_to_text(t.parser.screen(), sel_rel)), inner);
            if focus == Focus::Center && sel_rel.is_none() {
                let (cy, cx) = t.parser.screen().cursor_position();
                f.set_cursor_position(Position::new(inner.x + cx, inner.y + cy));
            }
        }
        Center::Editor => {
            let lines: Vec<Line> = editor
                .iter()
                .enumerate()
                .skip(editor_scroll)
                .take(inner.height as usize)
                .map(|(i, l)| {
                    let mut spans = vec![Span::styled(
                        format!("{:>4} ", i + 1),
                        Style::default().fg(Color::DarkGray),
                    )];
                    spans.extend(l.spans.iter().cloned());
                    Line::from(spans)
                })
                .collect();
            f.render_widget(Paragraph::new(lines), inner);
        }
        Center::Diff => {
            let lines: Vec<Line> = diff
                .iter()
                .skip(diff_scroll)
                .take(inner.height as usize)
                .map(|l| {
                    let c = l.chars().next().unwrap_or(' ');
                    let color = match c {
                        '+' => EMERALD,
                        '-' => ROSE,
                        '@' => CYAN,
                        _ => Color::Gray,
                    };
                    Line::from(Span::styled(l.clone(), Style::default().fg(color)))
                })
                .collect();
            f.render_widget(Paragraph::new(lines), inner);
        }
    }
    let _ = in_term;

    // Git panel
    f.render_widget(git_list(theme, gitst, focus), r.git);

    // Activity
    f.render_widget(activity_list(theme, recent, focus), r.activity);

    // Status bar
    let fname = match focus {
        Focus::Explorer => "Explorer",
        Focus::Center => "Center",
        Focus::Git => "Git",
        Focus::Activity => "Activity",
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {fname} "), Style::default().fg(Color::Black).bg(CYAN)),
            Span::styled("  F1 palette  Ctrl+P open  F2 new  F6 pane  drag borders to resize  Ctrl+Q quit  ", Style::default().fg(MUTED)),
            Span::styled(status_msg.to_string(), Style::default().fg(EMERALD)),
        ])),
        r.status,
    );

    // Command palette overlay
    if let Some(p) = palette {
        let area = centered(60, 50, f.area());
        f.render_widget(Clear, area);
        let filtered = filter_actions(&p.query);
        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled(format!("> {}", p.query), Style::default().fg(Color::White))),
            Line::from(""),
        ];
        for (i, (label, _)) in filtered.iter().enumerate() {
            let st = if i == p.sel.min(filtered.len().saturating_sub(1)) {
                Style::default().fg(Color::Black).bg(PLASMA).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(Span::styled(format!("  {label}"), st)));
        }
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default().borders(Borders::ALL).border_style(Style::default().fg(PLASMA)).title(" Command Palette "),
            ),
            area,
        );
    }

    // Quick-open overlay (fuzzy file finder)
    if let Some(qo) = quick {
        let area = centered(64, 60, f.area());
        f.render_widget(Clear, area);
        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled(format!("> {}", qo.query), Style::default().fg(Color::White))),
            Line::from(""),
        ];
        let cap = area.height.saturating_sub(4) as usize;
        for (i, (rel, _)) in qo.results.iter().take(cap).enumerate() {
            let st = if i == qo.sel.min(qo.results.len().saturating_sub(1)) {
                Style::default().fg(Color::Black).bg(CYAN).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(Span::styled(format!("  {rel}"), st)));
        }
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default().borders(Borders::ALL).border_style(Style::default().fg(CYAN)).title(" Quick Open  (Ctrl+P) "),
            ),
            area,
        );
    }

    // Bake-off agent picker overlay
    if let Some(bo) = bakeoff {
        let area = centered(50, 45, f.area());
        f.render_widget(Clear, area);
        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled("Race the same task across agents:", Style::default().fg(Color::White))),
            Line::from(Span::styled("Space toggles · Enter launches · Esc cancels", Style::default().fg(MUTED))),
            Line::from(""),
        ];
        for (i, (agent, on)) in bo.agents.iter().enumerate() {
            let mark = if *on { "[x]" } else { "[ ]" };
            let st = if i == bo.sel {
                Style::default().fg(Color::Black).bg(EMERALD).add_modifier(Modifier::BOLD)
            } else if *on {
                Style::default().fg(EMERALD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(Span::styled(format!("  {mark} {agent}"), st)));
        }
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default().borders(Borders::ALL).border_style(Style::default().fg(EMERALD)).title(" Agent Bake-off "),
            ),
            area,
        );
    }

    // First-run usage guideline (shown once)
    if intro {
        let area = centered(66, 60, f.area());
        f.render_widget(Clear, area);
        let g = |s: &str| Line::from(Span::styled(s.to_string(), Style::default().fg(theme.fg)));
        let bullet = |s: &str| Line::from(Span::styled(format!("  • {s}"), Style::default().fg(theme.muted)));
        let lines = vec![
            Line::from(Span::styled("Welcome to Synapse", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))),
            Line::from(""),
            g("Synapse observes and records what AI agents change."),
            g("It does not verify correctness — you stay in control."),
            Line::from(""),
            Line::from(Span::styled("Before you commit or deploy:", Style::default().fg(theme.accent2).add_modifier(Modifier::BOLD))),
            bullet("Review changes in the Git panel / preview diff (F6 → Git)."),
            bullet("Use checkpoints + `synapse rewind` to undo (reversible)."),
            bullet("Secrets (.env, keys) are never stored; `synapse policy` shows guardrails."),
            bullet("Run tests/builds yourself — Synapse won't deploy for you."),
            Line::from(""),
            Line::from(Span::styled("Press any key to continue.", Style::default().fg(theme.muted))),
        ];
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.accent))
                    .title(" Guidelines "),
            ),
            area,
        );
    }
}

fn centered(pct_w: u16, pct_h: u16, area: Rect) -> Rect {
    let w = area.width * pct_w / 100;
    let h = area.height * pct_h / 100;
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_translate_to_pty_bytes() {
        assert_eq!(key_to_bytes(KeyCode::Enter, KeyModifiers::NONE), Some(vec![b'\r']));
        assert_eq!(key_to_bytes(KeyCode::Char('c'), KeyModifiers::CONTROL), Some(vec![3]));
        assert_eq!(key_to_bytes(KeyCode::Up, KeyModifiers::NONE), Some(b"\x1b[A".to_vec()));
    }

    #[test]
    fn palette_filters() {
        assert!(filter_actions("claude").iter().any(|(_, id)| id == "claude"));
        assert!(filter_actions("tokyo").iter().any(|(_, id)| id == "theme:tokyo-night"));
        assert_eq!(filter_actions("zzz").len(), 0);
        // empty query = all actions + one entry per theme
        assert_eq!(filter_actions("").len(), ACTIONS.len() + crate::theme::names().len());
    }

    fn render_to_string(widget: List<'static>, w: u16, h: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| f.render_widget(widget, f.area())).unwrap();
        term.backend().buffer().content.iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn explorer_renders_filenames() {
        let rows = vec![
            FlatRow { path: "/p/src".into(), name: "src".into(), is_dir: true, depth: 0, expanded: false },
            FlatRow { path: "/p/alpha.txt".into(), name: "alpha.txt".into(), is_dir: false, depth: 0, expanded: false },
        ];
        let th = crate::theme::default_theme();
        let out = render_to_string(explorer_list(&th, &rows, 0, Focus::Explorer), 30, 6);
        assert!(out.contains("alpha.txt"), "explorer should render file names: {out}");
        assert!(out.contains("Explorer"));
    }

    #[test]
    fn git_panel_renders_branch_and_changes() {
        let g = GitState {
            branch: "main".into(),
            changes: vec![("M".into(), "src/app.ts".into())],
            sel: 0,
        };
        let th = crate::theme::default_theme();
        let out = render_to_string(git_list(&th, &g, Focus::Git), 40, 6);
        assert!(out.contains("main"), "git panel should show branch: {out}");
        assert!(out.contains("app.ts"));
    }

    #[test]
    fn activity_renders_event_titles() {
        let recent = vec![serde_json::json!({
            "ts": 0, "agent": "claude", "title": "Created Navbar.tsx"
        })];
        let th = crate::theme::default_theme();
        let out = render_to_string(activity_list(&th, &recent, Focus::Activity), 44, 5);
        assert!(out.contains("Created Navbar.tsx"), "activity should show titles: {out}");
    }

    #[test]
    fn fuzzy_ranks_best_match_first() {
        let files = vec![
            ("src/components/app.ts".to_string(), "/a".to_string()),
            ("README.md".to_string(), "/b".to_string()),
            ("src/zebra.rs".to_string(), "/c".to_string()),
        ];
        let r = fuzzy(&files, "app");
        assert!(!r.is_empty());
        assert!(r[0].0.contains("app.ts"), "best match should be app.ts, got {:?}", r[0].0);
        assert!(fuzzy(&files, "zzzzz").is_empty());
        assert_eq!(fuzzy(&files, "").len(), 3); // empty query returns all (capped)
    }

    #[test]
    fn ext_detection() {
        assert_eq!(ext_of("Navbar.tsx"), "tsx");
        assert_eq!(ext_of("mod.rs"), "rs");
        assert_eq!(ext_of("Makefile"), "Makefile");
    }

    #[test]
    fn highlight_produces_lines() {
        let ps = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let theme = ts.themes.values().next().unwrap().clone();
        let out = highlight("fn main() {\n    let x = 1;\n}\n", "rs", &ps, &theme);
        assert_eq!(out.len(), 3, "three source lines → three styled lines");
        assert!(out[0].spans.iter().any(|s| s.content.contains("fn")));
    }

    #[test]
    fn tree_flatten_and_toggle() {
        let dir = std::env::temp_dir().join(format!("syn-tree-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        std::fs::write(dir.join("sub/b.txt"), "y").unwrap();
        let mut tree = read_dir_nodes(&dir.to_string_lossy());
        let mut rows = Vec::new();
        flatten(&tree, 0, &mut rows);
        let n0 = rows.len();
        assert!(n0 >= 2); // sub/ and a.txt
        let sub = rows.iter().find(|r| r.name == "sub").unwrap().path.clone();
        assert!(toggle(&mut tree, &sub));
        rows.clear();
        flatten(&tree, 0, &mut rows);
        assert!(rows.len() > n0); // expanding revealed b.txt
        std::fs::remove_dir_all(&dir).ok();
    }
}
