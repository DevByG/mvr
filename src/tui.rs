use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use mvr::{
    apply, compile_matchers, match_paths, parse_replacement, plan_outcome, planned_prunes,
    ApplyOutcome, Rename, Scanner,
};

const DIM: Style = Style::new().fg(Color::DarkGray);

pub fn run_interactive() -> io::Result<()> {
    let mut terminal = ratatui::init();
    ratatui::crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal);
    ratatui::crossterm::execute!(std::io::stdout(), DisableMouseCapture)?;
    ratatui::restore();
    result
}

struct Input {
    text: String,
    cursor: usize,
}

impl Input {
    fn new() -> Self {
        Input {
            text: String::new(),
            cursor: 0,
        }
    }

    fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            let start = prev_boundary(&self.text, self.cursor);
            self.text.drain(start..self.cursor);
            self.cursor = start;
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.text.len() {
            let end = next_boundary(&self.text, self.cursor);
            self.text.drain(self.cursor..end);
        }
    }

    fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor = prev_boundary(&self.text, self.cursor);
        }
    }

    fn right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor = next_boundary(&self.text, self.cursor);
        }
    }

    fn home(&mut self) {
        self.cursor = 0;
    }

    fn end(&mut self) {
        self.cursor = self.text.len();
    }
}

fn prev_boundary(text: &str, index: usize) -> usize {
    let mut i = index - 1;
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_boundary(text: &str, index: usize) -> usize {
    let mut i = index + 1;
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

struct App {
    inputs: [Input; 2],
    focus: usize,
    case_insensitive: bool,
    scroll: u16,
    paths: Vec<String>,
    summary: Option<String>,
    case_button: Option<Rect>,
    input_rects: [Option<Rect>; 2],
}

struct View {
    match_count: usize,
    body: Vec<Line<'static>>,
    status: Option<String>,
    error: Option<String>,
    applyable: Option<Vec<Rename>>,
}

fn run(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut app = App {
        inputs: [Input::new(), Input::new()],
        focus: 0,
        case_insensitive: true,
        scroll: 0,
        paths: Vec::new(),
        summary: None,
        case_button: None,
        input_rects: [None, None],
    };

    let mut scanner = Scanner::new(Path::new("."));
    let mut scan_key = String::new();
    let mut force_scan = false;
    let mut last_scan = Instant::now();
    let mut view: Option<View> = None;
    let mut rebuild = true;
    loop {
        // the walk only enters directories matched by earlier pattern
        // segments and caches every directory it has read: pattern edits
        // reuse the cache, only F5 and applying re-read the filesystem;
        // the flat top-level list is cheap and stays fresh on a tick
        let pattern_depth = match compile_matchers(&app.inputs[0].text, app.case_insensitive) {
            Ok(matchers) => matchers.len(),
            Err(_) => 1,
        };
        let key = format!("{}|{}", app.case_insensitive, app.inputs[0].text);
        if force_scan
            || key != scan_key
            || (pattern_depth == 1 && last_scan.elapsed() > Duration::from_secs(2))
        {
            let matchers = compile_matchers(&app.inputs[0].text, app.case_insensitive);
            let scanned = match &matchers {
                Ok(matchers) => scanner.scan(matchers, force_scan)?,
                Err(_) => scanner.scan(&[], force_scan)?,
            };
            scan_key = key;
            force_scan = false;
            last_scan = Instant::now();
            app.paths = scanned;
            rebuild = true;
        }
        if rebuild || view.is_none() {
            view = Some(build_view(&app));
            rebuild = false;
        }
        terminal.draw(|frame| render(frame, &mut app, view.as_ref().unwrap()))?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let event = event::read()?;
        if let Event::Mouse(mouse) = &event {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                let click = Position {
                    x: mouse.column,
                    y: mouse.row,
                };
                if app.case_button.is_some_and(|rect| rect.contains(click)) {
                    app.case_insensitive = !app.case_insensitive;
                    app.scroll = 0;
                } else if app.input_rects[0].is_some_and(|rect| rect.contains(click)) {
                    app.focus = 0;
                } else if app.input_rects[1].is_some_and(|rect| rect.contains(click)) {
                    app.focus = 1;
                }
            }
            rebuild = true;
            continue;
        }
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if app.summary.is_some() {
            break;
        }

        let handled_edit = match key.code {
            KeyCode::F(5) => {
                force_scan = true;
                false
            }
            KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.case_insensitive = !app.case_insensitive;
                app.scroll = 0;
                false
            }
            KeyCode::Tab => {
                app.focus = 1 - app.focus;
                false
            }
            KeyCode::BackTab => {
                app.focus = 1 - app.focus;
                false
            }
            KeyCode::Enter if app.focus == 0 => {
                app.focus = 1;
                false
            }
            KeyCode::Enter => {
                if let Some(renames) = &view.as_ref().unwrap().applyable {
                    let outcome = apply(Path::new("."), renames);
                    app.summary = Some(summarize(&outcome, renames.len()));
                    force_scan = true;
                }
                false
            }
            KeyCode::Up => {
                app.scroll = app.scroll.saturating_sub(1);
                false
            }
            KeyCode::Down => {
                app.scroll = app.scroll.saturating_add(1);
                false
            }
            KeyCode::PageUp => {
                app.scroll = app.scroll.saturating_sub(10);
                false
            }
            KeyCode::PageDown => {
                app.scroll = app.scroll.saturating_add(10);
                false
            }
            KeyCode::Backspace => {
                app.inputs[app.focus].backspace();
                true
            }
            KeyCode::Delete => {
                app.inputs[app.focus].delete();
                true
            }
            KeyCode::Left => {
                app.inputs[app.focus].left();
                false
            }
            KeyCode::Right => {
                app.inputs[app.focus].right();
                false
            }
            KeyCode::Home => {
                app.inputs[app.focus].home();
                false
            }
            KeyCode::End => {
                app.inputs[app.focus].end();
                false
            }
            KeyCode::Char(c) => {
                app.inputs[app.focus].insert(c);
                true
            }
            _ => false,
        };
        if handled_edit {
            app.scroll = 0;
        }
        rebuild = true;
    }
    Ok(())
}

fn build_view(app: &App) -> View {
    let empty = View {
        match_count: 0,
        body: Vec::new(),
        status: None,
        error: None,
        applyable: None,
    };

    let regex_text = app.inputs[0].text.as_str();
    if regex_text.trim().is_empty() {
        return View {
            body: vec![Line::styled(
                " type a regex - matches appear here live",
                DIM,
            )],
            ..empty
        };
    }
    let matchers = match compile_matchers(regex_text, app.case_insensitive) {
        Ok(matchers) => matchers,
        Err(err) => {
            return View {
                error: Some(format!("regex: {err}")),
                ..empty
            }
        }
    };

    let matched = match_paths(&app.paths, &matchers);
    let froms: Vec<Line<'static>> = matched
        .iter()
        .map(|file| Line::styled(file.path.clone(), DIM))
        .collect();
    if matched.is_empty() {
        return View {
            status: Some("nothing found".into()),
            ..empty
        };
    }

    let replacement = app.inputs[1].text.as_str();
    if replacement.is_empty() {
        return View {
            match_count: matched.len(),
            body: froms,
            ..empty
        };
    }

    let parts = parse_replacement(replacement);
    let exists = |path: &str| Path::new(path).exists();
    let outcome = plan_outcome(&app.paths, &matchers, &parts, &exists);
    if outcome.renames.is_empty() {
        return View {
            match_count: matched.len(),
            body: froms,
            status: outcome.error.is_none().then(|| "nothing to change".into()),
            error: outcome.error.map(|err| err.to_string()),
            ..empty
        };
    }

    let mut target_counts: HashMap<&str, usize> = HashMap::new();
    for rename in &outcome.renames {
        *target_counts.entry(rename.to.as_str()).or_default() += 1;
    }
    let conflicting =
        |to: &str| target_counts.get(to).is_some_and(|count| *count > 1) || exists(to);

    let mut body: Vec<Line<'static>> = outcome
        .renames
        .iter()
        .map(|rename| {
            let (from_style, arrow_style, to_style) = if conflicting(&rename.to) {
                (
                    Style::new().fg(Color::Red),
                    Style::new().fg(Color::Red),
                    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
                )
            } else {
                (
                    Style::new(),
                    Style::new().fg(Color::Yellow),
                    Style::new().fg(Color::Green),
                )
            };
            Line::from(vec![
                Span::styled(rename.from.clone(), from_style),
                Span::styled(" => ", arrow_style),
                Span::styled(rename.to.clone(), to_style),
            ])
        })
        .collect();
    if outcome.error.is_none() {
        let prunes = planned_prunes(&app.paths, &outcome.renames);
        if !prunes.is_empty() {
            body.push(Line::from(vec![
                Span::styled(
                    "will be emptied and removed: ",
                    Style::new().fg(Color::Yellow),
                ),
                Span::from(prunes.join(", ")),
            ]));
        }
    }
    let error = outcome.error.map(|err| err.to_string());
    View {
        match_count: matched.len(),
        body,
        status: None,
        error: error.clone(),
        applyable: (error.is_none()).then_some(outcome.renames),
    }
}

fn summarize(outcome: &ApplyOutcome, total: usize) -> String {
    let done = total - outcome.failures.len();
    let mut text = format!("renamed: {done}");
    if !outcome.pruned_dirs.is_empty() {
        text.push_str(&format!(
            "; removed empty dirs: {}",
            outcome.pruned_dirs.join(", ")
        ));
    }
    if !outcome.failures.is_empty() {
        text.push_str(&format!(
            "; errors: {} (first: {})",
            outcome.failures.len(),
            outcome.failures[0].1
        ));
    }
    text
}

fn render(frame: &mut Frame, app: &mut App, view: &View) {
    let status_height =
        u16::from(app.summary.is_some() || view.status.is_some() || view.error.is_some());
    let [title_area, inputs_area, list_area, status_area, help_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(7),
        Constraint::Min(3),
        Constraint::Length(status_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let title = Line::from(vec![
        Span::styled("mvr", Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(
                "  ·  files: {}  ·  matches: {}",
                app.paths.len(),
                view.match_count
            ),
            DIM,
        ),
        Span::styled(
            if app.case_insensitive {
                "  ·  case: ignored"
            } else {
                "  ·  case: exact"
            },
            DIM,
        ),
    ]);
    frame.render_widget(Paragraph::new(title), title_area);

    let [regex_row, pattern_area, _gap] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(inputs_area);
    let [regex_area, button_area] =
        Layout::horizontal([Constraint::Min(3), Constraint::Length(8)]).areas(regex_row);
    render_input(frame, regex_area, " regex ", &app.inputs[0], app.focus == 0);
    render_case_button(frame, button_area, app);
    app.case_button = Some(button_area);
    app.input_rects = [Some(regex_area), Some(pattern_area)];
    render_input(
        frame,
        pattern_area,
        " pattern ",
        &app.inputs[1],
        app.focus == 1,
    );

    render_list(frame, list_area, app, view);

    if let Some(summary) = &app.summary {
        frame.render_widget(
            Paragraph::new(Span::styled(summary, Style::new().fg(Color::Green))),
            status_area,
        );
    } else if let Some(error) = &view.error {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("error: {error}"),
                Style::new().fg(Color::Red),
            )),
            status_area,
        );
    } else if let Some(status) = &view.status {
        frame.render_widget(
            Paragraph::new(Span::styled(status, Style::new().fg(Color::Yellow))),
            status_area,
        );
    }

    let help = if app.summary.is_some() {
        " any key to quit"
    } else {
        " Tab fields · Enter next/apply · Ctrl+T case · F5 rescan · ↑↓ PgUp/PgDn scroll · Esc quit"
    };
    frame.render_widget(Paragraph::new(help).style(DIM), help_area);
}

fn render_case_button(frame: &mut Frame, area: Rect, app: &App) {
    let style = if app.case_insensitive {
        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let button = Block::new()
        .borders(Borders::ALL)
        .border_style(style)
        .title(Span::styled(" case ", style));
    frame.render_widget(
        Paragraph::new(Span::styled("[Tt]", style))
            .alignment(Alignment::Center)
            .block(button),
        area,
    );
}

fn render_input(frame: &mut Frame, area: Rect, label: &str, input: &Input, focused: bool) {
    let border = if focused {
        Style::new().fg(Color::Yellow)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Span::styled(label, border));

    let before = input.text[..input.cursor].to_string();
    let at = input.text[input.cursor..].chars().next();
    let after = match at {
        Some(c) => input.text[input.cursor + c.len_utf8()..].to_string(),
        None => String::new(),
    };
    let cursor_style = Style::new().add_modifier(Modifier::UNDERLINED);
    let line = Line::from(vec![
        Span::from(before),
        match at {
            Some(c) => Span::styled(c.to_string(), cursor_style),
            None => Span::styled(" ", cursor_style),
        },
        Span::from(after),
    ]);
    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn render_list(frame: &mut Frame, area: Rect, app: &App, view: &View) {
    if view.body.is_empty() {
        return;
    }
    let visible = area.height as usize;
    let max_scroll = (view.body.len() - 1) as u16;
    let scroll = app.scroll.min(max_scroll) as usize;
    let mut shown: Vec<Line<'static>> = view.body[scroll..].to_vec();
    let hidden = shown.len().saturating_sub(visible);
    if hidden > 0 {
        let keep = visible.saturating_sub(1);
        shown.truncate(keep);
        shown.push(Line::styled(format!(" ... +{hidden} more"), DIM));
    }
    frame.render_widget(Paragraph::new(shown), area);
}
