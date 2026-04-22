use std::collections::HashMap;
use std::io::{self, Write};

use crossterm::{
    event::{self, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    style::{Attribute, SetAttribute, SetForegroundColor, Color as CtColor, ResetColor},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame, Terminal,
};
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

impl Attention {
    #[allow(dead_code)]
    fn gap_name(&self) -> &str {
        match self {
            Self::Approval { gap_name, .. } => gap_name,
            Self::Question { gap_name, .. } => gap_name,
        }
    }

    fn is_approval(&self) -> bool {
        matches!(self, Self::Approval { .. })
    }
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
    query:             String,
    phase:             String,
    snapshot:          Option<BlackboardSnapshot>,
    gap_order:         Vec<String>,
    progress:          HashMap<String, ProgressEntry>,
    evidence_count:    usize,
    final_response:    Option<String>,
    attention_queue:   Vec<Attention>,
    attention_idx:     usize,
    input_buf:         String,
    frame:             u32,
}

impl UiState {
    fn new(query: String) -> Self {
        Self {
            query,
            phase:             "starting".to_string(),
            snapshot:          None,
            gap_order:         Vec::new(),
            progress:          HashMap::new(),
            evidence_count:    0,
            final_response:    None,
            attention_queue:   Vec::new(),
            attention_idx:     0,
            input_buf:         String::new(),
            frame:             0,
        }
    }

    fn tick_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// Get the currently selected attention item, if any.
    fn current_attention(&self) -> Option<&Attention> {
        if self.attention_idx < self.attention_queue.len() {
            Some(&self.attention_queue[self.attention_idx])
        } else {
            None
        }
    }

    /// Get the currently selected attention item mutably, if any.
    #[allow(dead_code)]
    fn current_attention_mut(&mut self) -> Option<&mut Attention> {
        if self.attention_idx < self.attention_queue.len() {
            Some(&mut self.attention_queue[self.attention_idx])
        } else {
            None
        }
    }

    /// Move to the next attention item.
    fn attention_next(&mut self) {
        if !self.attention_queue.is_empty() && self.attention_idx + 1 < self.attention_queue.len() {
            self.attention_idx += 1;
            self.input_buf.clear();
        }
    }

    /// Move to the previous attention item.
    fn attention_prev(&mut self) {
        if self.attention_idx > 0 {
            self.attention_idx -= 1;
            self.input_buf.clear();
        }
    }

    /// Remove the currently selected attention item and move to the next, or the previous if at end.
    fn attention_pop_current(&mut self) -> Option<Attention> {
        if self.attention_idx < self.attention_queue.len() {
            let item = self.attention_queue.remove(self.attention_idx);
            if self.attention_idx >= self.attention_queue.len() && self.attention_idx > 0 {
                self.attention_idx -= 1;
            }
            self.input_buf.clear();
            Some(item)
        } else {
            None
        }
    }

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::OrchestratorProgress { phase } => {
                self.phase = phase.to_string();
            }
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
                self.phase = "solving gaps".to_string();
                let gap_name = gap_name.to_string();
                if !self.gap_order.contains(&gap_name) {
                    self.gap_order.push(gap_name.clone());
                }
                self.progress.insert(gap_name, ProgressEntry {
                    iteration, max_iterations, step, last_result,
                });
            }
            Event::Approval { gap_name, reason, tx, .. } => {
                self.attention_queue.push(Attention::Approval {
                    gap_name: gap_name.to_string(),
                    reason:   reason.to_string(),
                    tx,
                });
                self.input_buf.clear();
            }
            Event::Question { gap_name, question, tx, .. } => {
                self.attention_queue.push(Attention::Question {
                    gap_name: gap_name.to_string(),
                    question: question.to_string(),
                    tx,
                });
                self.input_buf.clear();
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.attention_queue.is_empty() { return; }
        if matches!(key.kind, KeyEventKind::Release) { return; }
        match key.code {
            KeyCode::Up => { self.attention_prev(); }
            KeyCode::Down => { self.attention_next(); }
            KeyCode::Backspace => { self.input_buf.pop(); }
            KeyCode::Char(c)   => { self.input_buf.push(c); }
            KeyCode::Enter     => {
                let buf = self.input_buf.drain(..).collect::<String>();
                if let Some(item) = self.attention_pop_current() {
                    match item {
                        Attention::Approval { tx, .. } => { let _ = tx.send(buf.trim().eq_ignore_ascii_case("y")); }
                        Attention::Question { tx, .. } => { let _ = tx.send(buf.trim().to_string()); }
                    }
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
        self.print_welcome()?;

        let stdin = tokio::io::stdin();
        let mut lines = tokio::io::BufReader::new(stdin).lines();
        loop {
            self.print_prompt()?;
            match lines.next_line().await? {
                Some(raw) => self.handle_input(raw.trim_end()).await?,
                None      => break,
            }
        }
        Ok(())
    }

    fn print_welcome(&self) -> io::Result<()> {
        let width = 80;
        let border = CtColor::Rgb { r: 88, g: 88, b: 88 };
        let title = CtColor::Rgb { r: 138, g: 180, b: 248 };
        let dim = CtColor::Rgb { r: 120, g: 120, b: 120 };
        let green = CtColor::Rgb { r: 152, g: 195, b: 121 };

        // ASCII art logo
        let logo = [
            r"  ███╗   ███╗ ",
            r"  ████╗ ████║ ",
            r"  ██╔████╔██║ ",
            r"  ██║╚██╔╝██║ ",
            r"  ██║ ╚═╝ ██║ ",
            r"  ╚═╝     ╚═╝ ",
        ];

        println!();
        // Top border
        execute!(io::stdout(), SetForegroundColor(border))?;
        print!("╭───");
        execute!(io::stdout(), SetForegroundColor(title), SetAttribute(Attribute::Bold))?;
        print!(" Moss v0.1.0 ");
        execute!(io::stdout(), ResetColor, SetAttribute(Attribute::Reset), SetForegroundColor(border))?;
        println!("{}╮", "─".repeat(width - 18));

        // Logo lines
        for line in &logo {
            execute!(io::stdout(), SetForegroundColor(border))?;
            print!("│");
            execute!(io::stdout(), SetForegroundColor(green), SetAttribute(Attribute::Bold))?;
            print!("{}", line);
            execute!(io::stdout(), ResetColor, SetAttribute(Attribute::Reset), SetForegroundColor(border))?;
            println!("{}│", " ".repeat(width - 2 - line.chars().count()));
        }

        // Tagline
        execute!(io::stdout(), SetForegroundColor(border))?;
        print!("│");
        execute!(io::stdout(), SetForegroundColor(dim))?;
        let tagline = "  Local-first AI Operating System";
        print!("{}", tagline);
        execute!(io::stdout(), SetForegroundColor(border))?;
        println!("{}│", " ".repeat(width - 2 - tagline.len()));

        // Empty line
        execute!(io::stdout(), SetForegroundColor(border))?;
        println!("│{}│", " ".repeat(width - 2));

        // Bottom border
        execute!(io::stdout(), SetForegroundColor(border))?;
        println!("╰{}╯", "─".repeat(width - 2));
        execute!(io::stdout(), ResetColor)?;
        println!();

        Ok(())
    }

    fn print_prompt(&self) -> io::Result<()> {
        let dim = CtColor::Rgb { r: 88, g: 88, b: 88 };
        let cyan = CtColor::Rgb { r: 138, g: 180, b: 248 };

        // Separator line
        execute!(io::stdout(), SetForegroundColor(dim))?;
        println!("{}", "─".repeat(80));

        // Prompt
        execute!(io::stdout(), SetForegroundColor(cyan), SetAttribute(Attribute::Bold))?;
        print!("❯ ");
        execute!(io::stdout(), ResetColor, SetAttribute(Attribute::Reset))?;
        io::stdout().flush()?;
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
                    self.state.tick_frame();
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
                        _ = tokio::time::sleep(tokio::time::Duration::from_millis(50)) => {
                            // Keep frame animating even when no events arrive
                        }
                    }
                };

                // Drain any remaining events to show final state
                while let Ok(ev) = self.rx.try_recv() {
                    self.state.apply(ev);
                }
                self.state.final_response = match &result {
                    Ok(response) if !response.is_empty() => Some(response.clone()),
                    _ => None,
                };
                self.state.tick_frame();
                terminal.draw(|f| render_with_footer(f, &self.state, " Press Enter to return... "))?;

                // Wait for Enter before leaving.
                loop {
                    match key_rx.recv().await {
                        Some(key) => {
                            if key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                break;
                            }
                            if matches!(key.code, KeyCode::Enter) {
                                break;
                            }
                        }
                        None => break, // channel closed
                    }
                }

                drop(_guard); // leave alternate screen before printing
                match result {
                    Ok(response) if !response.is_empty() => {
                        print_final_response(&response)?;
                    }
                    Ok(_)  => {}
                    Err(e) => {
                        execute!(
                            io::stdout(),
                            SetForegroundColor(CtColor::Rgb { r: 190, g: 80, b: 70 }),
                            SetAttribute(Attribute::Bold)
                        )?;
                        eprint!("\n✘ Error: ");
                        execute!(io::stdout(), ResetColor, SetAttribute(Attribute::Reset))?;
                        eprintln!("{e}\n");
                    }
                }
            }
        }
        Ok(())
    }
}

fn final_response_output(response: &str) -> Vec<String> {
    vec![
        String::new(),
        "─".repeat(80),
        "✔ Moss".to_string(),
        response.to_string(),
    ]
}

fn print_final_response(response: &str) -> io::Result<()> {
    execute!(
        io::stdout(),
        SetForegroundColor(CtColor::Rgb { r: 152, g: 195, b: 121 }),
        SetAttribute(Attribute::Bold)
    )?;

    let lines = final_response_output(response);
    println!();
    execute!(io::stdout(), SetForegroundColor(CtColor::Rgb { r: 88, g: 88, b: 88 }))?;
    println!("{}", lines[1]);
    execute!(
        io::stdout(),
        SetForegroundColor(CtColor::Rgb { r: 152, g: 195, b: 121 }),
        SetAttribute(Attribute::Bold)
    )?;
    println!("{}", lines[2]);
    execute!(io::stdout(), ResetColor, SetAttribute(Attribute::Reset))?;
    execute!(io::stdout(), SetForegroundColor(CtColor::Rgb { r: 220, g: 220, b: 220 }))?;
    println!("{}", lines[3]);
    execute!(io::stdout(), ResetColor)?;
    Ok(())
}

// ── Rendering ─────────────────────────────────────────────────────────────────

const CARD_INNER: usize = 25;
const CARD_TOTAL: usize = CARD_INNER + 2; // + 2 border chars

fn render(frame: &mut Frame, state: &UiState) {
    let w = frame.area().width as usize;
    frame.render_widget(Paragraph::new(build_lines(state, w, None)), frame.area());
}

fn render_with_footer(frame: &mut Frame, state: &UiState, footer: &str) {
    let w = frame.area().width as usize;
    frame.render_widget(Paragraph::new(build_lines(state, w, Some(footer))), frame.area());
}

fn spinner_frame(frame_counter: u32) -> &'static str {
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[(frame_counter as usize) % FRAMES.len()]
}

fn phase_display(phase: &str, frame: u32) -> String {
    if phase_is_complete(phase) {
        format!("✓ {phase}")
    } else {
        format!("{} {phase}", spinner_frame(frame))
    }
}

fn phase_is_complete(phase: &str) -> bool {
    phase.eq_ignore_ascii_case("complete") || phase.eq_ignore_ascii_case("completed")
}

// ── Color palette (Claude Code inspired) ──────────────────────────────────────

const CLR_BORDER:   Color = Color::Rgb(88, 88, 88);    // dim gray
const CLR_TITLE:    Color = Color::Rgb(138, 180, 248); // soft blue
const CLR_SECTION:  Color = Color::Rgb(198, 120, 221); // purple
const CLR_LABEL:    Color = Color::Rgb(150, 150, 150); // muted
const CLR_VALUE:    Color = Color::Rgb(220, 220, 220); // bright text
const CLR_DONE:     Color = Color::Rgb(152, 195, 121); // green
const CLR_RUNNING:  Color = Color::Rgb(229, 192, 123); // yellow/amber
const CLR_READY:    Color = Color::Rgb(97, 175, 239);  // blue
const CLR_BLOCKED:  Color = Color::Rgb(190, 80,  70);  // red
const CLR_WARN:     Color = Color::Rgb(255, 170,  66); // orange
const CLR_FOOTER:   Color = Color::Rgb(120, 120, 120); // dim

fn build_lines(state: &UiState, w: usize, footer: Option<&str>) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(styled_box_top(w, " Moss ", &state.query));

    // Blackboard summary
    let snap = state.snapshot.as_ref();
    let intent = snap.and_then(|s| s.intent()).unwrap_or("(pending)");
    let phase = phase_display(&state.phase, state.frame);
    lines.push(styled_row_phase(w, &phase, phase_is_complete(&state.phase)));
    lines.push(styled_box_row_empty(w));
    lines.push(styled_row_kv(w, "intent", intent));
    lines.push(styled_row_kv(w, "gaps", &format!("{} total", state.gap_order.len())));

    let gaps_snap = snap.map(|s| s.gaps().collect::<Vec<_>>()).unwrap_or_default();
    let blocked  = gaps_snap.iter().filter(|g| *g.state() == GapState::Blocked).count();
    let ready    = gaps_snap.iter().filter(|g| *g.state() == GapState::Ready).count();
    let assigned = gaps_snap.iter().filter(|g| *g.state() == GapState::Assigned).count();
    let done     = gaps_snap.iter().filter(|g| *g.state() == GapState::Closed).count();
    lines.push(styled_row_states(w, blocked, ready, assigned, done));

    let pending_a = state.attention_queue.iter().filter(|a| a.is_approval()).count();
    let pending_q = state.attention_queue.len() - pending_a;
    lines.push(styled_row_kv(w, "pending", &format!(
        "{pending_a} approvals | {pending_q} questions | {} evidence",
        state.evidence_count
    )));

    // Live DAG
    lines.push(styled_box_sep(w, " Gaps "));
    let inner_w = w.saturating_sub(2);
    let per_row = ((inner_w + 1) / (CARD_TOTAL + 1)).max(1);

    if state.gap_order.is_empty() {
        lines.push(styled_row_dim(w, "(waiting for gaps\u{2026})"));
    } else {
        for chunk in state.gap_order.chunks(per_row) {
            for row in card_chunk_lines(chunk, snap, &state.progress, inner_w, state.frame) {
                lines.push(row);
            }
            lines.push(styled_box_row_empty(w));
        }
    }

    // Attention queue
    if !state.attention_queue.is_empty() {
        lines.push(styled_box_sep_warn(w, " Attention "));
        
        // Show all pending items as a numbered menu
        for (idx, attn) in state.attention_queue.iter().enumerate() {
            let is_selected = idx == state.attention_idx;
            let prefix = if is_selected { "►" } else { " " };
            let item_label = match attn {
                Attention::Approval { gap_name, .. } => format!("[{}] {gap_name} — approval", idx + 1),
                Attention::Question { gap_name, .. } => format!("[{}] {gap_name} — question", idx + 1),
            };
            lines.push(styled_row_menu_item(w, prefix, &item_label, is_selected));
        }
        
        // Show details of the selected item
        if let Some(attn) = state.current_attention() {
            lines.push(styled_box_row_empty(w));
            match attn {
                Attention::Approval { gap_name, reason, .. } => {
                    lines.push(styled_row_warn(w, &format!("Approval needed: {gap_name}")));
                    lines.push(styled_row_kv(w, "reason", reason));
                    lines.push(styled_row_input(w, "approve? [y/N]", &state.input_buf));
                    lines.push(styled_row_dim(w, "↑/↓ to switch | Enter to submit"));
                }
                Attention::Question { gap_name, question, .. } => {
                    let inner = w.saturating_sub(2);
                    let prefix = format!(" {gap_name} asks: ");
                    let available = inner.saturating_sub(prefix.len());
                    
                    let wrapped = wrap_text(question, available);
                    for (line_idx, line_text) in wrapped.iter().enumerate() {
                        if line_idx == 0 {
                            lines.push(styled_row_warn(w, &format!("{gap_name} asks: {line_text}")));
                        } else {
                            lines.push(styled_row_warn(w, &format!("  {line_text}")));
                        }
                    }
                    lines.push(styled_row_input(w, "answer", &state.input_buf));
                    lines.push(styled_row_dim(w, "↑/↓ to switch | Enter to submit"));
                }
            }
        }
    }

    if let Some(response) = state.final_response.as_deref() {
        lines.push(styled_box_sep(w, " Response "));
        let inner = w.saturating_sub(2);
        for source_line in response.lines() {
            if source_line.is_empty() {
                lines.push(styled_box_row_empty(w));
                continue;
            }

            for wrapped in wrap_text(source_line, inner.saturating_sub(1)) {
                lines.push(styled_row_value(w, &format!(" {wrapped}")));
            }
        }
    }

    match footer {
        Some(msg) => lines.push(styled_box_bottom_msg(w, msg)),
        None      => lines.push(styled_box_bottom(w)),
    }
    lines
}

/// Build the 6 display lines (top border, 4 content, bottom border) for a row of gap cards.
fn card_chunk_lines(
    names:    &[String],
    snap:     Option<&BlackboardSnapshot>,
    progress: &HashMap<String, ProgressEntry>,
    inner_w:  usize,
    frame:    u32,
) -> Vec<Line<'static>> {
    let n = names.len();
    let total_cards_w = n * CARD_TOTAL + n.saturating_sub(1);
    let left_pad = (inner_w.saturating_sub(total_cards_w)) / 2;

    // Build each row as Vec<Span> to preserve styling
    let mut row_spans: Vec<Vec<Span<'static>>> = vec![Vec::new(); 6];

    let border = Style::default().fg(CLR_BORDER);
    let label_style = Style::default().fg(CLR_LABEL);

    for (i, name) in names.iter().enumerate() {
        let gap_state = snap.and_then(|s| s.gap_state(name));
        let state_label = gap_state.map(gap_state_label).unwrap_or("-");
        let state_color = gap_state.map(gap_state_color).unwrap_or(CLR_LABEL);

        // Add spinner animation for running and just-completed gaps
        let status_display = match gap_state {
            Some(&GapState::Assigned) => format!("{} {}", spinner_frame(frame), state_label),
            Some(&GapState::Closed) => format!("✓ {}", state_label),
            _ => state_label.to_string(),
        };

        let prog     = progress.get(name);
        let iter_str = prog.map_or("-".to_string(), |p| format!("{}/{}", p.iteration, p.max_iterations));
        let step_str = prog.map_or("-", |p| p.step.as_ref()).to_string();
        let last_str = prog.and_then(|p| p.last_result.as_deref()).unwrap_or("-").to_string();

        let spacer = if i == 0 { " ".repeat(left_pad) } else { " ".to_string() };

        let max_name_len = CARD_INNER.saturating_sub(2);
        let truncated_name: String = name.chars().take(max_name_len).collect();
        let title = format!(" {truncated_name} ");
        let top_dashes = CARD_INNER.saturating_sub(title.chars().count());

        // Row 0: top border with title
        row_spans[0].push(Span::raw(spacer.clone()));
        row_spans[0].push(Span::styled("╭", border));
        row_spans[0].push(Span::styled(title, Style::default().fg(state_color).add_modifier(Modifier::BOLD)));
        row_spans[0].push(Span::styled(format!("{}╮", "─".repeat(top_dashes)), border));

        // Rows 1-4: content
        let fields: [(_, _, Color); 4] = [
            ("status", status_display, state_color),
            ("iter",   iter_str,       CLR_VALUE),
            ("step",   step_str,       CLR_VALUE),
            ("last",   last_str,       CLR_LABEL),
        ];
        for (r, (lbl, val, clr)) in fields.into_iter().enumerate() {
            row_spans[r + 1].push(Span::raw(spacer.clone()));
            row_spans[r + 1].push(Span::styled("│", border));
            row_spans[r + 1].push(Span::styled(format!(" {:<6}  ", lbl), label_style));
            let val_str = pad_truncate(&val, CARD_INNER - 9);
            row_spans[r + 1].push(Span::styled(val_str, Style::default().fg(clr)));
            row_spans[r + 1].push(Span::styled("│", border));
        }

        // Row 5: bottom border
        row_spans[5].push(Span::raw(spacer));
        row_spans[5].push(Span::styled(format!("╰{}╯", "─".repeat(CARD_INNER)), border));
    }

    row_spans
        .into_iter()
        .map(|spans| styled_box_row_spans(inner_w, spans))
        .collect()
}

fn gap_state_label(state: &GapState) -> &'static str {
    match state {
        GapState::Blocked  => "blocked",
        GapState::Ready    => "ready",
        GapState::Assigned => "running",
        GapState::Closed   => "done",
    }
}

fn gap_state_color(state: &GapState) -> Color {
    match state {
        GapState::Blocked  => CLR_BLOCKED,
        GapState::Ready    => CLR_READY,
        GapState::Assigned => CLR_RUNNING,
        GapState::Closed   => CLR_DONE,
    }
}

// ── Styled box drawing helpers ────────────────────────────────────────────────

fn styled_box_top(w: usize, label: &str, query: &str) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    // Content: " {label}" + "— " + query + " "
    let content_len = 1 + label.len() + 2 + query.chars().count() + 1;
    let dashes = inner.saturating_sub(content_len);
    Line::from(vec![
        Span::styled("╭", border),
        Span::styled(format!(" {label}"), Style::default().fg(CLR_TITLE).add_modifier(Modifier::BOLD)),
        Span::styled("— ", border),
        Span::styled(query.to_string(), Style::default().fg(CLR_VALUE)),
        Span::styled(" ", border),
        Span::styled(format!("{}╮", "─".repeat(dashes)), border),
    ])
}

fn styled_box_sep(w: usize, title: &str) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    let right = inner.saturating_sub(title.chars().count());
    Line::from(vec![
        Span::styled("├", border),
        Span::styled(title.to_string(), Style::default().fg(CLR_SECTION).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{}┤", "─".repeat(right)), border),
    ])
}

fn styled_box_sep_warn(w: usize, title: &str) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    let right = inner.saturating_sub(title.chars().count());
    Line::from(vec![
        Span::styled("├", border),
        Span::styled(title.to_string(), Style::default().fg(CLR_WARN).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{}┤", "─".repeat(right)), border),
    ])
}

fn styled_row_kv(w: usize, key: &str, value: &str) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    let prefix = format!(" {:<8} ", key);
    let val_len = inner.saturating_sub(prefix.len());
    Line::from(vec![
        Span::styled("│", border),
        Span::styled(prefix, Style::default().fg(CLR_LABEL)),
        Span::styled(pad_truncate(value, val_len), Style::default().fg(CLR_VALUE)),
        Span::styled("│", border),
    ])
}

fn styled_row_phase(w: usize, value: &str, is_complete: bool) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    let label = " phase ▶ ";
    let value_style = if is_complete {
        Style::default().fg(CLR_DONE).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(CLR_RUNNING).add_modifier(Modifier::BOLD)
    };

    let value_len = inner.saturating_sub(label.chars().count());
    Line::from(vec![
        Span::styled("│", border),
        Span::styled(label.to_string(), Style::default().fg(CLR_TITLE).add_modifier(Modifier::BOLD)),
        Span::styled(pad_truncate(value, value_len), value_style),
        Span::styled("│", border),
    ])
}

fn styled_row_states(w: usize, blocked: usize, ready: usize, running: usize, done: usize) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    let prefix = " states   ";

    // Build content spans first to calculate actual length
    let mut content_spans: Vec<Span<'static>> = vec![
        Span::styled(prefix.to_string(), Style::default().fg(CLR_LABEL)),
    ];
    let parts: [(usize, &str, Color); 4] = [
        (blocked, "blocked", CLR_BLOCKED),
        (ready,   "ready",   CLR_READY),
        (running, "running", CLR_RUNNING),
        (done,    "done",    CLR_DONE),
    ];
    for (i, (n, lbl, clr)) in parts.iter().enumerate() {
        if i > 0 { content_spans.push(Span::styled(" │ ", Style::default().fg(CLR_BORDER))); }
        content_spans.push(Span::styled(format!("{n}"), Style::default().fg(*clr).add_modifier(Modifier::BOLD)));
        content_spans.push(Span::styled(format!(" {lbl}"), Style::default().fg(CLR_LABEL)));
    }

    // Calculate actual content length
    let used: usize = content_spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = inner.saturating_sub(used);

    let mut spans = vec![Span::styled("│", border)];
    spans.extend(content_spans);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled("│", border));
    Line::from(spans)
}

fn styled_row_dim(w: usize, content: &str) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    Line::from(vec![
        Span::styled("│", border),
        Span::styled(pad_truncate(&format!(" {content}"), inner), Style::default().fg(CLR_LABEL)),
        Span::styled("│", border),
    ])
}

fn styled_row_warn(w: usize, content: &str) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    Line::from(vec![
        Span::styled("│", border),
        Span::styled(pad_truncate(&format!(" {content}"), inner), Style::default().fg(CLR_WARN)),
        Span::styled("│", border),
    ])
}

fn styled_row_input(w: usize, prompt: &str, buf: &str) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    let prefix = format!("   {prompt}: ");
    let input = format!("{buf}_");
    let val_len = inner.saturating_sub(prefix.len());
    Line::from(vec![
        Span::styled("│", border),
        Span::styled(prefix, Style::default().fg(CLR_LABEL)),
        Span::styled(pad_truncate(&input, val_len), Style::default().fg(CLR_VALUE)),
        Span::styled("│", border),
    ])
}

fn styled_row_value(w: usize, content: &str) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    Line::from(vec![
        Span::styled("│", border),
        Span::styled(pad_truncate(content, inner), Style::default().fg(CLR_VALUE)),
        Span::styled("│", border),
    ])
}

fn styled_row_menu_item(w: usize, prefix: &str, label: &str, is_selected: bool) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    let item_color = if is_selected {
        Style::default().fg(CLR_WARN).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(CLR_LABEL)
    };
    let item_text = format!(" {prefix} {label}");
    Line::from(vec![
        Span::styled("│", border),
        Span::styled(pad_truncate(&item_text, inner), item_color),
        Span::styled("│", border),
    ])
}

fn styled_box_row_empty(w: usize) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    Line::from(vec![
        Span::styled("│", border),
        Span::raw(" ".repeat(inner)),
        Span::styled("│", border),
    ])
}

fn styled_box_row_spans(inner_w: usize, spans: Vec<Span<'static>>) -> Line<'static> {
    let border = Style::default().fg(CLR_BORDER);
    let content_len: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = inner_w.saturating_sub(content_len);
    let mut out = vec![Span::styled("│", border)];
    out.extend(spans);
    out.push(Span::raw(" ".repeat(pad)));
    out.push(Span::styled("│", border));
    Line::from(out)
}

fn styled_box_bottom(w: usize) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    Line::from(Span::styled(format!("╰{}╯", "─".repeat(inner)), border))
}

fn styled_box_bottom_msg(w: usize, msg: &str) -> Line<'static> {
    let inner = w.saturating_sub(2);
    let border = Style::default().fg(CLR_BORDER);
    let right = inner.saturating_sub(msg.chars().count());
    Line::from(vec![
        Span::styled("╰", border),
        Span::styled(msg.to_string(), Style::default().fg(CLR_FOOTER)),
        Span::styled(format!("{}╯", "─".repeat(right)), border),
    ])
}

#[cfg(test)]
fn welcome_banner_lines(width: usize) -> Vec<String> {
    let logo = [
        r"  ███╗   ███╗ ",
        r"  ████╗ ████║ ",
        r"  ██╔████╔██║ ",
        r"  ██║╚██╔╝██║ ",
        r"  ██║ ╚═╝ ██║ ",
        r"  ╚═╝     ╚═╝ ",
    ];
    let title = " Moss v0.1.0 ";
    let tagline = "  Local-first AI Operating System";

    let mut lines = Vec::with_capacity(logo.len() + 3);
    let top_prefix = format!("╭───{title}");
    let top_fill = width.saturating_sub(top_prefix.chars().count() + 1);
    lines.push(format!("{top_prefix}{}╮", "─".repeat(top_fill)));

    for line in logo {
        let right_pad = width.saturating_sub(2 + line.chars().count());
        lines.push(format!("│{line}{}│", " ".repeat(right_pad)));
    }

    let tagline_pad = width.saturating_sub(2 + tagline.chars().count());
    lines.push(format!("│{tagline}{}│", " ".repeat(tagline_pad)));
    lines.push(format!("│{}│", " ".repeat(width.saturating_sub(2))));
    lines.push(format!("╰{}╯", "─".repeat(width.saturating_sub(2))));
    lines
}

/// Pad with spaces or truncate to exactly `width` terminal columns.
fn pad_truncate(s: &str, width: usize) -> String {
    let normalized: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();

    let n = normalized.chars().count();
    if n >= width {
        normalized.chars().take(width).collect()
    } else {
        format!("{}{}", normalized, " ".repeat(width - n))
    }
}

/// Wrap text to fit within a maximum width, breaking on word boundaries.
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width < 1 {
        return vec![text.to_string()];
    }
    
    let mut lines = Vec::new();
    let mut current_line = String::new();
    
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        let current_len = current_line.chars().count();
        
        if current_len == 0 {
            // First word on the line
            if word_len <= max_width {
                current_line.push_str(word);
            } else {
                // Word is too long, break it up
                lines.push(word.chars().take(max_width).collect());
                if word_len > max_width {
                    current_line = word.chars().skip(max_width).collect();
                }
            }
        } else if current_len + 1 + word_len <= max_width {
            // Word fits on current line with space
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            // Start a new line
            lines.push(current_line);
            current_line = word.to_string();
        }
    }
    
    if !current_line.is_empty() {
        lines.push(current_line);
    }
    
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn lines_to_text(lines: Vec<Line<'static>>) -> String {
        lines
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn ignores_release_events_while_collecting_answer_input() {
        let (tx, _rx) = oneshot::channel();
        let mut state = UiState::new("query".to_string());
        state.attention_queue.push(Attention::Question {
            gap_name: "gap".to_string(),
            question: "question".to_string(),
            tx,
        });

        state.handle_key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ));
        state.handle_key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));

        assert_eq!(state.input_buf, "a");
    }

    #[test]
    fn solver_progress_adds_gap_to_order_when_snapshot_is_missing() {
        let mut state = UiState::new("query".to_string());

        state.apply(Event::SolverProgress {
            gap_id: Uuid::new_v4(),
            gap_name: "parallel_gap".into(),
            iteration: 1,
            max_iterations: 10,
            step: "code: python3".into(),
            last_result: Some("exit 0".into()),
        });

        assert_eq!(state.gap_order, vec!["parallel_gap".to_string()]);
        assert!(state.progress.contains_key("parallel_gap"));
    }

    #[test]
    fn pad_truncate_normalizes_newlines_to_keep_single_line_layout() {
        let formatted = pad_truncate("line1\nline2\tline3", 16);
        assert!(!formatted.contains('\n'));
        assert_eq!(formatted.chars().count(), 16);
    }

    #[test]
    fn build_lines_includes_phase_row() {
        let state = UiState::new("query".to_string());
        let text = lines_to_text(build_lines(&state, 100, None));
        assert!(text.contains("phase"));
    }

    #[test]
    fn build_lines_places_phase_before_intent() {
        let state = UiState::new("query".to_string());
        let text = lines_to_text(build_lines(&state, 100, None));
        let phase_idx = text.find("phase").unwrap_or(usize::MAX);
        let intent_idx = text.find("intent").unwrap_or(usize::MAX);
        assert!(phase_idx < intent_idx, "phase should render before intent");
    }

    #[test]
    fn build_lines_adds_blank_row_after_phase() {
        let state = UiState::new("query".to_string());
        let lines = build_lines(&state, 100, None)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        let phase_idx = lines
            .iter()
            .position(|line| line.contains("phase ▶"))
            .unwrap_or(usize::MAX);
        assert!(phase_idx < lines.len().saturating_sub(1));

        let next_line = &lines[phase_idx + 1];
        assert!(!next_line.contains("intent"), "spacer row should be between phase and intent");
        assert_eq!(next_line.chars().filter(|c| !c.is_whitespace()).collect::<String>(), "││");
    }

    #[test]
    fn phase_row_preserves_full_box_width_with_unicode_label() {
        let width = 80;
        let row = styled_row_phase(width, "✓ complete", true);
        let text = row
            .spans
            .into_iter()
            .map(|span| span.content.to_string())
            .collect::<String>();

        assert_eq!(text.chars().count(), width);
    }

    #[test]
    fn welcome_banner_top_border_preserves_full_width() {
        let width = 80;
        let lines = welcome_banner_lines(width);

        assert_eq!(lines[0].chars().count(), width);
    }

    #[test]
    fn build_lines_shows_final_response_before_footer() {
        let mut state = UiState::new("query".to_string());
        state.final_response = Some("All gaps resolved.".to_string());

        let lines = build_lines(&state, 80, Some(" Press Enter to return... "))
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        let response_idx = lines
            .iter()
            .position(|line| line.contains("All gaps resolved."))
            .unwrap_or(usize::MAX);
        let footer_idx = lines
            .iter()
            .position(|line| line.contains("Press Enter to return"))
            .unwrap_or(usize::MAX);

        assert!(response_idx < footer_idx, "final response should render before the footer");
    }

    #[test]
    fn final_response_output_includes_chat_header_and_body() {
        let lines = final_response_output("All gaps resolved.");

        assert!(lines.iter().any(|line| line.contains("✔ Moss")));
        assert!(lines.iter().any(|line| line.contains("All gaps resolved.")));
    }

    #[test]
    fn phase_display_shows_spinner_for_active_phase() {
        let frame = 3;
        let display = phase_display("solving gaps", frame);
        assert_eq!(display, format!("{} solving gaps", spinner_frame(frame)));
    }

    #[test]
    fn phase_display_shows_checkmark_for_complete_phase() {
        assert_eq!(phase_display("complete", 0), "✓ complete");
        assert_eq!(phase_display("COMPLETED", 0), "✓ COMPLETED");
    }
}
