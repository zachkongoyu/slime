use std::collections::HashMap;
use std::io::{self, Write};

use crossterm::{
    event::{self, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, text::Line, widgets::Paragraph, Frame, Terminal};
use tokio::io::AsyncBufReadExt;
use tokio::sync::{mpsc, oneshot};

use crate::error::MossError;
use crate::moss::types::{BlackboardSnapshot, GapState};
use crate::moss::signal::Event;
use crate::Moss;

// ── Attention ─────────────────────────────────────────────────────────────────

enum Attention {
    Approval { gap_name: String, reason:   String, tx: oneshot::Sender<bool>   },
    Question { gap_name: String, question: String, tx: oneshot::Sender<String> },
}

// ── Progress entry (CLI concern) ──────────────────────────────────────────────

struct ProgressEntry {
    iteration:      u32,
    max_iterations: u32,
    step:           Box<str>,
    last_result:    Option<Box<str>>,
}

// ── UI state ──────────────────────────────────────────────────────────────────

struct UiState {
    query:          String,
    snapshot:       Option<BlackboardSnapshot>,
    gap_order:      Vec<String>,
    progress:       HashMap<String, ProgressEntry>,
    evidence_count: usize,
    attention:      Option<Attention>,
    input_buf:      String,
}

impl UiState {
    fn new(query: String) -> Self {
        Self {
            query,
            snapshot:       None,
            gap_order:      Vec::new(),
            progress:       HashMap::new(),
            evidence_count: 0,
            attention:      None,
            input_buf:      String::new(),
        }
    }

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::BlackboardSnapshot { intent, gaps, evidences } => {
                let snap = BlackboardSnapshot::new(intent, gaps, evidences);
                for gap in snap.gaps() {
                    let name = gap.name().to_string();
                    if !self.gap_order.contains(&name) {
                        self.gap_order.push(name);
                    }
                }
                self.evidence_count = snap.evidence_count();
                self.snapshot = Some(snap);
            }
            Event::SolverProgress { gap_name, iteration, max_iterations, step, last_result, .. } => {
                self.progress.insert(gap_name.to_string(), ProgressEntry {
                    iteration, max_iterations, step, last_result,
                });
            }
            Event::Approval { gap_name, reason, tx, .. } => {
                self.attention = Some(Attention::Approval {
                    gap_name: gap_name.to_string(),
                    reason:   reason.to_string(),
                    tx,
                });
                self.input_buf.clear();
            }
            Event::Question { gap_name, question, tx, .. } => {
                self.attention = Some(Attention::Question {
                    gap_name: gap_name.to_string(),
                    question: question.to_string(),
                    tx,
                });
                self.input_buf.clear();
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.attention.is_none() { return; }
        match key.code {
            KeyCode::Backspace => { self.input_buf.pop(); }
            KeyCode::Char(c)   => { self.input_buf.push(c); }
            KeyCode::Enter     => {
                let buf = self.input_buf.drain(..).collect::<String>();
                match self.attention.take() {
                    Some(Attention::Approval { tx, .. }) => { let _ = tx.send(buf.trim().eq_ignore_ascii_case("y")); }
                    Some(Attention::Question { tx, .. }) => { let _ = tx.send(buf.trim().to_string()); }
                    None => {}
                }
            }
            _ => {}
        }
    }
}

// ── Raw mode guard ────────────────────────────────────────────────────────────

struct RawGuard;

impl RawGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

// ── CLI ───────────────────────────────────────────────────────────────────────

pub struct Cli {
    moss:  Moss,
    rx:    mpsc::Receiver<Event>,
    state: UiState,
}

impl Cli {
    pub fn new(moss: Moss, rx: mpsc::Receiver<Event>) -> Self {
        Self { moss, rx, state: UiState::new(String::new()) }
    }

    pub async fn run(&mut self) -> Result<(), MossError> {
        let stdin = tokio::io::stdin();
        let mut lines = tokio::io::BufReader::new(stdin).lines();
        loop {
            print!("> ");
            io::stdout().flush()?;
            match lines.next_line().await? {
                Some(raw) => self.handle_input(raw.trim_end()).await?,
                None      => break,
            }
        }
        Ok(())
    }

    async fn handle_input(&mut self, input: &str) -> Result<(), MossError> {
        match input {
            "" => {}
            "exit" | "quit" => std::process::exit(0),
            query => {
                self.state = UiState::new(query.to_string());
                let _guard = RawGuard::enter()?;
                let backend = CrosstermBackend::new(io::stdout());
                let mut terminal = Terminal::new(backend)?;

                let (key_tx, mut key_rx) = mpsc::channel::<KeyEvent>(16);
                tokio::task::spawn_blocking(move || loop {
                    if event::poll(std::time::Duration::from_millis(50)).unwrap_or(false) {
                        if let Ok(event::Event::Key(key)) = event::read() {
                            if key_tx.blocking_send(key).is_err() { break; }
                        }
                    }
                });

                tokio::pin!(let fut = self.moss.run(query););
                let result = loop {
                    terminal.draw(|f| render(f, &self.state))?;
                    tokio::select! {
                        r    = &mut fut        => break r,
                        ev   = self.rx.recv()  => match ev {
                            Some(ev) => self.state.apply(ev),
                            None     => break Ok(String::new()),
                        },
                        key  = key_rx.recv()   => {
                            if let Some(key) = key {
                                if key.code == KeyCode::Char('c')
                                    && key.modifiers.contains(KeyModifiers::CONTROL)
                                {
                                    break Ok(String::new());
                                }
                                self.state.handle_key(key);
                            }
                        }
                    }
                };

                drop(_guard); // leave alternate screen before printing
                match result {
                    Ok(response) if !response.is_empty() => println!("{response}"),
                    Ok(_)  => {}
                    Err(e) => eprintln!("[moss] error: {e}"),
                }
            }
        }
        Ok(())
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

const CARD_INNER: usize = 25;
const CARD_TOTAL: usize = CARD_INNER + 2; // + 2 border chars

fn render(frame: &mut Frame, state: &UiState) {
    let w = frame.area().width as usize;
    frame.render_widget(Paragraph::new(build_lines(state, w)), frame.area());
}

fn build_lines(state: &UiState, w: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(box_top(w, &format!(" Moss — query: {} ", state.query)));

    // Blackboard summary
    let snap = state.snapshot.as_ref();
    let intent = snap.and_then(|s| s.intent()).unwrap_or("(pending)");
    lines.push(box_row(w, &format!(" intent: {intent}")));
    lines.push(box_row(w, &format!(" gaps: {} total", state.gap_order.len())));

    let gaps_snap = snap.map(|s| s.gaps().collect::<Vec<_>>()).unwrap_or_default();
    let blocked  = gaps_snap.iter().filter(|g| *g.state() == GapState::Blocked).count();
    let ready    = gaps_snap.iter().filter(|g| *g.state() == GapState::Ready).count();
    let assigned = gaps_snap.iter().filter(|g| *g.state() == GapState::Assigned).count();
    let done     = gaps_snap.iter().filter(|g| *g.state() == GapState::Closed).count();
    lines.push(box_row(w, &format!(" states: {blocked} blocked | {ready} ready | {assigned} running | {done} done")));

    let pending_a = usize::from(matches!(state.attention, Some(Attention::Approval { .. })));
    let pending_q = usize::from(matches!(state.attention, Some(Attention::Question { .. })));
    lines.push(box_row(w, &format!(
        " pending approvals: {pending_a} | pending questions: {pending_q} | evidence entries: {}",
        state.evidence_count
    )));

    // Live DAG
    lines.push(box_sep(w, " Live DAG "));
    let inner_w = w.saturating_sub(2);
    let per_row = ((inner_w + 1) / (CARD_TOTAL + 1)).max(1);

    if state.gap_order.is_empty() {
        lines.push(box_row(w, " (waiting for gaps\u{2026})"));
    } else {
        for chunk in state.gap_order.chunks(per_row) {
            for row in card_chunk_lines(chunk, snap, &state.progress, inner_w) {
                lines.push(row);
            }
            lines.push(box_row(w, ""));
        }
    }

    // Attention
    if let Some(ref attn) = state.attention {
        lines.push(box_sep(w, " Attention "));
        match attn {
            Attention::Approval { gap_name, reason, .. } => {
                lines.push(box_row(w, &format!(" Approval needed: {gap_name}")));
                lines.push(box_row(w, &format!("   reason: {reason}")));
                lines.push(box_row(w, &format!("   approve? [y/N] {}_", state.input_buf)));
            }
            Attention::Question { gap_name, question, .. } => {
                lines.push(box_row(w, &format!(" {gap_name} asks: {question}")));
                lines.push(box_row(w, &format!("   answer: {}_", state.input_buf)));
            }
        }
    }

    lines.push(box_bottom(w));
    lines
}

/// Build the 6 display lines (top border, 4 content, bottom border) for a row of gap cards.
fn card_chunk_lines(
    names:    &[String],
    snap:     Option<&BlackboardSnapshot>,
    progress: &HashMap<String, ProgressEntry>,
    inner_w:  usize,
) -> Vec<Line<'static>> {
    let n = names.len();
    let mut rows = vec![String::new(); 6];

    let total_cards_w = n * CARD_TOTAL + n.saturating_sub(1);
    let left_pad = (inner_w.saturating_sub(total_cards_w)) / 2;

    for (i, name) in names.iter().enumerate() {
        let state_label = snap
            .and_then(|s| s.gap_state(name))
            .map(gap_state_label)
            .unwrap_or("-");
        let prog     = progress.get(name);
        let iter_str = prog.map_or("-".to_string(), |p| format!("{}/{}", p.iteration, p.max_iterations));
        let step_str = prog.map_or("-", |p| p.step.as_ref()).to_string();
        let last_str = prog.and_then(|p| p.last_result.as_deref()).unwrap_or("-").to_string();

        let content = [
            card_field("status", state_label),
            card_field("iter",   &iter_str),
            card_field("step",   &step_str),
            card_field("last",   &last_str),
        ];
        let spacer = if i == 0 { " ".repeat(left_pad) } else { " ".to_string() };

        let title      = format!(" {name} ");
        let top_dashes = CARD_INNER.saturating_sub(title.chars().count());
        rows[0].push_str(&format!("{}┌{}{}┐", spacer, title, "─".repeat(top_dashes)));
        for (r, line) in content.iter().enumerate() {
            rows[r + 1].push_str(&format!("{}│{}│", spacer, line));
        }
        rows[5].push_str(&format!("{}└{}┘", spacer, "─".repeat(CARD_INNER)));
    }

    rows.into_iter().map(|r| box_row_owned(inner_w, r)).collect()
}

fn card_field(label: &str, value: &str) -> String {
    pad_truncate(&format!(" {:<6}  {}", label, value), CARD_INNER)
}

fn gap_state_label(state: &GapState) -> &'static str {
    match state {
        GapState::Blocked  => "blocked",
        GapState::Ready    => "ready",
        GapState::Assigned => "running",
        GapState::Closed   => "done",
    }
}

// ── Box drawing helpers ───────────────────────────────────────────────────────

fn box_top(w: usize, title: &str) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let right = inner.saturating_sub(title.chars().count());
    Line::from(format!("┌{}{}┐", title, "─".repeat(right)))
}

fn box_sep(w: usize, title: &str) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let right = inner.saturating_sub(title.chars().count());
    Line::from(format!("├{}{}┤", title, "─".repeat(right)))
}

fn box_row(w: usize, content: &str) -> Line<'static> {
    let inner = w.saturating_sub(2);
    Line::from(format!("│{}│", pad_truncate(content, inner)))
}

fn box_row_owned(inner_w: usize, content: String) -> Line<'static> {
    Line::from(format!("│{}│", pad_truncate(&content, inner_w)))
}

fn box_bottom(w: usize) -> Line<'static> {
    let inner = w.saturating_sub(2);
    Line::from(format!("└{}┘", "─".repeat(inner)))
}

/// Pad with spaces or truncate to exactly `width` terminal columns (char-count safe for ASCII/Latin).
fn pad_truncate(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        s.chars().take(width).collect()
    } else {
        format!("{}{}", s, " ".repeat(width - n))
    }
}
