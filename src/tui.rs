use std::io;
use std::path::Path;

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::{DefaultTerminal, Frame};
use regex::Regex;

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use mvr::messages::Messages;
use mvr::{
    apply, compile_matchers, match_paths, parse_replacement, plan_outcome, planned_prunes,
    ApplyOutcome, Rename, Scanner,
};

const DIM: Style = Style::new().fg(Color::DarkGray);

pub fn run_interactive(keep_open: bool) -> io::Result<()> {
    let mut terminal = ratatui::init();
    ratatui::crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, keep_open);
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
    msg: &'static Messages,
    keep_open: bool,
    inputs: [Input; 2],
    focus: usize,
    case_insensitive: bool,
    scroll: usize,
    summary: Option<String>,
    case_button: Option<Rect>,
    input_rects: [Option<Rect>; 2],
    scrollbar: Option<Rect>,
    dragging: bool,
}

impl App {
    fn visible(&self) -> usize {
        self.scrollbar.map_or(1, |bar| bar.height as usize)
    }

    fn max_scroll(&self, view: &View) -> usize {
        view.body.len().saturating_sub(self.visible())
    }

    fn set_scroll(&mut self, value: usize, view: &View) {
        self.scroll = value.min(self.max_scroll(view));
    }
}

struct View {
    files: usize,
    match_count: usize,
    body: Vec<Line<'static>>,
    status: Option<String>,
    error: Option<String>,
    applyable: Option<Vec<Rename>>,
}

fn run(terminal: &mut DefaultTerminal, keep_open: bool) -> io::Result<()> {
    let mut app = App {
        msg: mvr::messages::detect(),
        keep_open,
        inputs: [Input::new(), Input::new()],
        focus: 0,
        case_insensitive: true,
        scroll: 0,
        summary: None,
        case_button: None,
        input_rects: [None, None],
        scrollbar: None,
        dragging: false,
    };

    // the preview worker owns the scanner and the whole view computation
    // (fs walk, matching, plan, script eval) so the UI thread only draws
    // and routes events; the poll timeout below is the result pump
    let (requests_out, requests_in) = mpsc::channel::<PreviewRequest>();
    let (views_out, views_in) = mpsc::channel::<View>();
    let msg = app.msg;
    thread::spawn(move || {
        preview_worker(requests_in, views_out, Scanner::new(Path::new(".")), msg)
    });

    let mut view = View {
        files: 0,
        match_count: 0,
        body: Vec::new(),
        status: None,
        error: None,
        applyable: None,
    };
    let mut request_pending = true;
    let mut force_scan = false;
    loop {
        if request_pending {
            if requests_out
                .send(PreviewRequest {
                    regex: app.inputs[0].text.clone(),
                    replacement: app.inputs[1].text.clone(),
                    case_insensitive: app.case_insensitive,
                    force: force_scan,
                })
                .is_err()
            {
                return Ok(());
            }
            force_scan = false;
            request_pending = false;
        }
        while let Ok(newer) = views_in.try_recv() {
            view = newer;
        }
        terminal.draw(|frame| render(frame, &mut app, &view))?;

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let event = event::read()?;
        // the summary line stays on screen until the user acts next;
        // a terminal resize is not an action
        if app.summary.is_some()
            && app.keep_open
            && matches!(event, Event::Key(_) | Event::Mouse(_))
        {
            app.summary = None;
        }
        if let Event::Mouse(mouse) = &event {
            if handle_mouse(&mut app, mouse, &view) {
                request_pending = true;
            }
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

        let mut state_changed = false;
        let handled_edit = match key.code {
            KeyCode::F(5) => {
                force_scan = true;
                state_changed = true;
                false
            }
            KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.case_insensitive = !app.case_insensitive;
                app.scroll = 0;
                state_changed = true;
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
                if let Some(renames) = &view.applyable {
                    let outcome = apply(Path::new("."), renames);
                    app.summary = Some(summarize(&outcome, renames.len(), app.msg));
                    force_scan = true;
                    state_changed = true;
                }
                false
            }
            KeyCode::Up => {
                app.set_scroll(app.scroll.saturating_sub(1), &view);
                false
            }
            KeyCode::Down => {
                app.set_scroll(app.scroll.saturating_add(1), &view);
                false
            }
            KeyCode::PageUp => {
                app.set_scroll(app.scroll.saturating_sub(10), &view);
                false
            }
            KeyCode::PageDown => {
                app.set_scroll(app.scroll.saturating_add(10), &view);
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
            state_changed = true;
        }
        if state_changed {
            request_pending = true;
        }
    }
    Ok(())
}

struct PreviewRequest {
    regex: String,
    replacement: String,
    case_insensitive: bool,
    force: bool,
}

// latest-wins (the tuimux pattern): rapid keystrokes queue many requests,
// only the final one is worth computing - everything queued behind it
// is drained here
fn preview_worker(
    requests: Receiver<PreviewRequest>,
    views: Sender<View>,
    mut scanner: Scanner,
    msg: &'static Messages,
) {
    let mut scan_key = String::new();
    let mut paths: Vec<String> = Vec::new();
    while let Ok(mut latest) = requests.recv() {
        while let Ok(newer) = requests.try_recv() {
            latest = newer;
        }
        // the walk only enters directories matched by earlier pattern
        // segments and caches every directory it has read: pattern edits
        // reuse the cache, only F5 and applying re-read the filesystem
        let key = format!("{}|{}", latest.case_insensitive, latest.regex);
        if latest.force || key != scan_key {
            scan_key = key;
            let matchers = compile_matchers(&latest.regex, latest.case_insensitive);
            paths = match &matchers {
                Ok(matchers) => scanner.scan(matchers, latest.force),
                Err(_) => scanner.scan(&[], latest.force),
            }
            .unwrap_or_default();
        }
        // build_view shows the type-a-regex hint for an empty field; the
        // compile check must not turn that state into an error first
        let view = if latest.regex.trim().is_empty() {
            build_view(&latest.regex, &latest.replacement, &paths, &[], msg)
        } else {
            match compile_matchers(&latest.regex, latest.case_insensitive) {
                Ok(matchers) => {
                    build_view(&latest.regex, &latest.replacement, &paths, &matchers, msg)
                }
                Err(err) => View {
                    files: paths.len(),
                    match_count: 0,
                    body: Vec::new(),
                    status: None,
                    error: Some(format!("regex: {err}")),
                    applyable: None,
                },
            }
        };
        if views.send(view).is_err() {
            return;
        }
    }
}

fn handle_mouse(app: &mut App, mouse: &MouseEvent, view: &View) -> bool {
    let position = Position {
        x: mouse.column,
        y: mouse.row,
    };
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if app.case_button.is_some_and(|rect| rect.contains(position)) {
                app.case_insensitive = !app.case_insensitive;
                app.scroll = 0;
                return true;
            } else if app.scrollbar.is_some_and(|rect| rect.contains(position)) {
                app.dragging = true;
                if let Some(bar) = app.scrollbar {
                    drag_scroll(app, mouse.row, bar, view);
                }
            } else if app.input_rects[0].is_some_and(|rect| rect.contains(position)) {
                app.focus = 0;
            } else if app.input_rects[1].is_some_and(|rect| rect.contains(position)) {
                app.focus = 1;
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.dragging {
                if let Some(bar) = app.scrollbar {
                    drag_scroll(app, mouse.row, bar, view);
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => app.dragging = false,
        MouseEventKind::ScrollUp => app.set_scroll(app.scroll.saturating_sub(3), view),
        MouseEventKind::ScrollDown => app.set_scroll(app.scroll.saturating_add(3), view),
        _ => {}
    }
    false
}

fn drag_scroll(app: &mut App, row: u16, bar: Rect, view: &View) {
    let visible = bar.height as usize;
    let content = view.body.len();
    let max_scroll = content.saturating_sub(visible);
    if max_scroll == 0 {
        app.scroll = 0;
        return;
    }
    // widget geometry: arrow rows at both ends, track between them;
    // invert the widget's thumb formula (part_lengths in ratatui),
    // then our stretch remap, so the thumb lands under the cursor
    let track_len = (bar.height as usize).saturating_sub(2).max(1);
    let track_offset = (row.saturating_sub(bar.y + 1) as usize).min(track_len - 1);
    let max_position = content - 1;
    let start = track_offset as f64 * (max_position + visible) as f64 / track_len as f64;
    let scroll = (start * max_scroll as f64 / max_position as f64).round() as usize;
    app.scroll = scroll.min(max_scroll);
}

fn build_view(
    regex_text: &str,
    replacement: &str,
    paths: &[String],
    matchers: &[Regex],
    msg: &Messages,
) -> View {
    let empty = View {
        files: paths.len(),
        match_count: 0,
        body: Vec::new(),
        status: None,
        error: None,
        applyable: None,
    };

    if regex_text.trim().is_empty() {
        return View {
            body: vec![Line::styled(msg.type_regex_hint, DIM)],
            ..empty
        };
    }

    let matched = match_paths(paths, matchers);
    let froms: Vec<Line<'static>> = matched
        .iter()
        .map(|file| Line::styled(file.path.clone(), DIM))
        .collect();
    if matched.is_empty() {
        return View {
            status: Some(msg.nothing_found.into()),
            ..empty
        };
    }

    if replacement.is_empty() {
        return View {
            match_count: matched.len(),
            body: froms,
            ..empty
        };
    }

    let parts = parse_replacement(replacement);
    let exists = |path: &str| Path::new(path).exists();
    let outcome = plan_outcome(&matched, &parts, &exists);
    if outcome.renames.is_empty() {
        return View {
            match_count: matched.len(),
            body: froms,
            status: outcome
                .error
                .is_none()
                .then(|| msg.nothing_to_change.into()),
            error: outcome.error.map(|err| err.to_string()),
            ..empty
        };
    }

    let conflicting = |to: &str| outcome.conflicting.iter().any(|bad| bad == to);

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
        let prunes = planned_prunes(paths, &outcome.renames);
        if !prunes.is_empty() {
            body.push(Line::from(vec![
                Span::styled(msg.emptied_removed, Style::new().fg(Color::Yellow)),
                Span::from(prunes.join(", ")),
            ]));
        }
    }
    let error = outcome.error.map(|err| err.to_string());
    View {
        files: paths.len(),
        match_count: matched.len(),
        body,
        status: None,
        error: error.clone(),
        applyable: (error.is_none()).then_some(outcome.renames),
    }
}

fn summarize(outcome: &ApplyOutcome, total: usize, msg: &Messages) -> String {
    let done = total - outcome.failures.len();
    let mut text = format!("{}{done}", msg.renamed_prefix);
    if !outcome.pruned_dirs.is_empty() {
        text.push_str(&format!(
            "{}{}",
            msg.removed_empty_dirs,
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

    let mut title_spans = vec![Span::styled(
        "mvr",
        Style::new().add_modifier(Modifier::BOLD),
    )];
    if app.keep_open {
        title_spans.push(Span::styled(
            " [interactive]",
            Style::new().fg(Color::Yellow),
        ));
    }
    title_spans.push(Span::styled(
        format!(
            "  ·  {}: {}  ·  {}: {}",
            app.msg.files, view.files, app.msg.matches, view.match_count
        ),
        DIM,
    ));
    title_spans.push(Span::styled(
        format!(
            "  ·  {}",
            if app.case_insensitive {
                app.msg.case_ignored
            } else {
                app.msg.case_exact
            }
        ),
        DIM,
    ));
    let title = Line::from(title_spans);
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
        app.msg.pattern_label,
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

    let help = if app.summary.is_some() && !app.keep_open {
        app.msg.any_key_quit
    } else {
        app.msg.help
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
        .title(Span::styled(app.msg.case_label, style));
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

fn render_list(frame: &mut Frame, area: Rect, app: &mut App, view: &View) {
    app.scrollbar = None;
    if view.body.is_empty() {
        return;
    }
    let [content_area, bar_area] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    app.scrollbar = Some(bar_area);

    let visible = content_area.height as usize;
    let max_scroll = view.body.len().saturating_sub(visible);
    app.scroll = app.scroll.min(max_scroll);
    let scroll = app.scroll;
    let mut shown: Vec<Line<'static>> = view.body[scroll..].to_vec();
    let hidden = shown.len().saturating_sub(visible);
    if hidden > 0 {
        let keep = visible.saturating_sub(1);
        shown.truncate(keep);
        shown.push(Line::styled(
            format!(" ... +{hidden}{}", app.msg.more_suffix),
            DIM,
        ));
    }
    frame.render_widget(Paragraph::new(shown), content_area);

    if view.body.len() > visible {
        // the widget's thumb only touches the bottom when the last line
        // sits at the top of the viewport; stretch our scroll range
        // (0 .. content-visible) onto the widget's (0 .. content-1)
        let content = view.body.len();
        let widget_position = scroll * (content - 1) / max_scroll;
        let mut scrollbar_state = ScrollbarState::new(content)
            .position(widget_position)
            .viewport_content_length(visible);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            bar_area,
            &mut scrollbar_state,
        );
    }
}
