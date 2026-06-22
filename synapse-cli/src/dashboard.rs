//! Live terminal cockpit (`syn dashboard`) — the Synapse cockpit rendered with
//! ratatui. The watcher records events into the local store; the dashboard polls
//! it and paints the timeline, agent roster and health, fully offline.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use synapse_core::agents::AgentRegistry;
use synapse_core::events::SynapseEvent;
use synapse_core::watcher::{self, EventSink};

use crate::clock;

pub fn run(root: PathBuf) -> Result<(), String> {
    let (db, snaps) = crate::open(&root)?;
    let agents: AgentRegistry = Arc::new(Mutex::new(HashMap::new()));

    // The watcher persists to the store; the dashboard polls it. No-op sink.
    let sink: EventSink = Arc::new(|_e: &SynapseEvent| {});
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
    crate::spawn_scanner(root.to_string_lossy().to_string(), db.clone(), agents, stop.clone());

    let mut terminal = ratatui::init();
    let res = loop {
        if let Err(e) = terminal.draw(|f| ui(f, &db, &root)) {
            break Err(e.to_string());
        }
        match event::poll(Duration::from_millis(400)) {
            Ok(true) => {
                if let Ok(Event::Key(k)) = event::read() {
                    if matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) {
                        break Ok(());
                    }
                }
            }
            Ok(false) => {}
            Err(e) => break Err(e.to_string()),
        }
    };
    ratatui::restore();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    res
}

const PLASMA: Color = Color::Rgb(129, 140, 248);
const CYAN: Color = Color::Rgb(34, 211, 238);
const EMERALD: Color = Color::Rgb(52, 211, 153);
const ROSE: Color = Color::Rgb(251, 113, 133);
const AMBER: Color = Color::Rgb(251, 191, 36);
const MUTED: Color = Color::Rgb(124, 132, 153);

fn kind_color(kind: &str) -> Color {
    match kind {
        "created" => EMERALD,
        "modified" => CYAN,
        "deleted" => ROSE,
        "renamed" | "moved" => AMBER,
        "reasoning" => PLASMA,
        _ => MUTED,
    }
}

fn ui(f: &mut Frame, db: &synapse_core::store::Db, root: &PathBuf) {
    let events = db.recent_events(60).unwrap_or_default();
    let sessions = db.list_sessions().unwrap_or_default();
    let health = db.health().ok();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)])
        .split(f.area());

    // Header
    let header = Paragraph::new(Line::from(vec![
        Span::styled("◇ SYNAPSE", Style::default().fg(PLASMA).add_modifier(Modifier::BOLD)),
        Span::styled("  cockpit  ", Style::default().fg(MUTED)),
        Span::styled(root.display().to_string(), Style::default().fg(CYAN)),
        Span::styled("   ● LIVE", Style::default().fg(EMERALD)),
    ]))
    .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
    f.render_widget(header, outer[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(outer[1]);

    // Timeline
    let items: Vec<ListItem> = events
        .iter()
        .map(|v| {
            let ts = v.get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
            let agent = v.get("agent").and_then(|a| a.as_str()).unwrap_or("?");
            let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("");
            let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let added = v.get("added").and_then(|a| a.as_i64());
            let removed = v.get("removed").and_then(|a| a.as_i64());
            let mut spans = vec![
                Span::styled(clock(ts), Style::default().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::styled(format!("{agent:>7}"), Style::default().fg(PLASMA)),
                Span::raw("  "),
                Span::styled("●", Style::default().fg(kind_color(kind))),
                Span::raw(" "),
                Span::raw(title.to_string()),
            ];
            if !path.is_empty() {
                spans.push(Span::styled(format!("  {path}"), Style::default().fg(MUTED)));
            }
            if let Some(a) = added {
                spans.push(Span::styled(format!("  +{a}"), Style::default().fg(EMERALD)));
            }
            if let Some(r) = removed {
                if r > 0 {
                    spans.push(Span::styled(format!(" -{r}"), Style::default().fg(ROSE)));
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let timeline = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" Live Activity "),
    );
    f.render_widget(timeline, body[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(body[1]);

    // Agents
    let agent_items: Vec<ListItem> = if sessions.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  no agents detected yet",
            Style::default().fg(MUTED),
        )))]
    } else {
        sessions
            .iter()
            .map(|s| {
                let dot = if s.status == "active" { EMERALD } else { MUTED };
                ListItem::new(Line::from(vec![
                    Span::styled("● ", Style::default().fg(dot)),
                    Span::raw(format!("{:<16}", s.name)),
                    Span::styled(format!(" {}  ", s.status), Style::default().fg(MUTED)),
                    Span::styled(format!("{} files", s.files_touched), Style::default().fg(CYAN)),
                ]))
            })
            .collect()
    };
    let roster = List::new(agent_items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" Agent War Room "),
    );
    f.render_widget(roster, right[0]);

    // Health
    let h = health.unwrap_or_else(default_health);
    let bar = |v: i64| -> String {
        let n = (v.clamp(0, 100) / 5) as usize;
        format!("{}{}", "█".repeat(n), "░".repeat(20 - n))
    };
    let lines = vec![
        Line::from(vec![Span::styled("files modified  ", Style::default().fg(MUTED)), Span::raw(h.files_modified.to_string())]),
        Line::from(vec![Span::styled("build           ", Style::default().fg(MUTED)), Span::raw(h.build.clone())]),
        Line::from(vec![Span::styled("tests           ", Style::default().fg(MUTED)), Span::raw(h.tests.clone())]),
        Line::from(""),
        Line::from(vec![Span::styled("risk        ", Style::default().fg(MUTED)), Span::styled(bar(h.risk_score), Style::default().fg(ROSE))]),
        Line::from(vec![Span::styled("complexity  ", Style::default().fg(MUTED)), Span::styled(bar(h.complexity), Style::default().fg(AMBER))]),
    ];
    let health_w = Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Project Health "),
        );
    f.render_widget(health_w, right[1]);

    // Footer
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" q ", Style::default().fg(Color::Black).bg(PLASMA)),
        Span::styled(" quit   ", Style::default().fg(MUTED)),
        Span::styled("Synapse · Athreix", Style::default().fg(Color::DarkGray)),
    ]));
    f.render_widget(footer, outer[2]);
}

fn default_health() -> synapse_core::store::Health {
    synapse_core::store::Health {
        files_modified: 0,
        agents_running: 0,
        build: "unknown".into(),
        tests: "unknown".into(),
        risk_score: 0,
        coverage: 0,
        complexity: 0,
        tech_debt: 0,
        agent_efficiency: 0,
        cost_today: 0.0,
    }
}
