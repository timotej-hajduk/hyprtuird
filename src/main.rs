use std::{
    env, io,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, ListState, Paragraph, Row, Table},
};
use serde::Deserialize;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

mod theme {
    use ratatui::style::Color;

    pub const BG: Color = Color::Rgb(25, 27, 38);
    pub const FG: Color = Color::Rgb(192, 202, 245);
    pub const DIM: Color = Color::Rgb(86, 95, 137);
    pub const BORDER: Color = Color::Rgb(169, 177, 214);
    pub const BLUE: Color = Color::Rgb(122, 162, 247);
    pub const CYAN: Color = Color::Rgb(42, 195, 222);
    pub const GREEN: Color = Color::Rgb(158, 206, 106);
    pub const ORANGE: Color = Color::Rgb(224, 175, 104);
    pub const RED: Color = Color::Rgb(247, 118, 142);
    pub const ROW: Color = Color::Rgb(70, 151, 160);
    pub const ROW_FG: Color = Color::Rgb(25, 27, 38);
}

#[derive(Clone, Debug, Deserialize)]
struct Workspace {
    id: i32,
    name: String,
    monitor: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Monitor {
    id: i32,
    name: String,
    focused: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pane {
    Workspaces,
    Monitors,
}

#[derive(Debug)]
struct HyprlandClient {
    socket_path: PathBuf,
}

impl HyprlandClient {
    fn from_env() -> Result<Self> {
        let signature = env::var("HYPRLAND_INSTANCE_SIGNATURE").map_err(
            |_| "HYPRLAND_INSTANCE_SIGNATURE is not set. Run hyprtuird inside a Hyprland session.",
        )?;

        let mut candidates = Vec::new();
        if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR") {
            candidates.push(
                PathBuf::from(runtime_dir)
                    .join("hypr")
                    .join(&signature)
                    .join(".socket.sock"),
            );
        }
        candidates.push(
            PathBuf::from("/tmp")
                .join("hypr")
                .join(&signature)
                .join(".socket.sock"),
        );

        let socket_path = candidates
            .iter()
            .find(|path| path.exists())
            .cloned()
            .or_else(|| candidates.into_iter().next())
            .ok_or("Could not build a Hyprland IPC socket path.")?;

        Ok(Self { socket_path })
    }

    fn request(&self, command: &str) -> Result<String> {
        let mut stream = UnixStream::connect(&self.socket_path).map_err(|err| {
            format!(
                "Failed to connect to Hyprland socket {}: {err}",
                self.socket_path.display()
            )
        })?;

        stream.write_all(command.as_bytes())?;
        stream.shutdown(std::net::Shutdown::Write)?;

        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response)
    }

    fn workspaces(&self) -> Result<Vec<Workspace>> {
        let response = self.request("j/workspaces")?;
        let mut workspaces: Vec<Workspace> = serde_json::from_str(&response)?;
        workspaces.sort_by(|left, right| match (left.id >= 0, right.id >= 0) {
            (true, true) => left.id.cmp(&right.id),
            (false, false) => left.name.cmp(&right.name),
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
        });
        Ok(workspaces)
    }

    fn monitors(&self) -> Result<Vec<Monitor>> {
        let response = self.request("j/monitors")?;
        let mut monitors: Vec<Monitor> = serde_json::from_str(&response)?;
        monitors.sort_by_key(|monitor| monitor.id);
        Ok(monitors)
    }

    fn move_workspace_to_monitor(
        &self,
        workspace: &Workspace,
        monitor: &Monitor,
    ) -> Result<String> {
        let workspace_arg = shell_escape_arg(&workspace.name);
        let monitor_arg = shell_escape_arg(&monitor.name);
        let response = self.request(&format!(
            "dispatch moveworkspacetomonitor {workspace_arg} {monitor_arg}"
        ))?;

        if response.trim().eq_ignore_ascii_case("ok") {
            Ok(format!(
                "Moved workspace {} to {}",
                workspace_label(workspace),
                monitor.name
            ))
        } else {
            Err(format!("Hyprland rejected move: {}", response.trim()).into())
        }
    }
}

#[derive(Debug)]
struct App {
    client: HyprlandClient,
    workspaces: Vec<Workspace>,
    monitors: Vec<Monitor>,
    workspace_state: ListState,
    monitor_state: ListState,
    focus: Pane,
    status: String,
    should_quit: bool,
}

impl App {
    fn new(client: HyprlandClient) -> Self {
        let mut app = Self {
            client,
            workspaces: Vec::new(),
            monitors: Vec::new(),
            workspace_state: ListState::default(),
            monitor_state: ListState::default(),
            focus: Pane::Workspaces,
            status: String::from("Loading Hyprland state..."),
            should_quit: false,
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        match self.load_state() {
            Ok(()) => {
                self.status = format!(
                    "{} workspace(s), {} monitor(s)",
                    self.workspaces.len(),
                    self.monitors.len()
                );
            }
            Err(err) => {
                self.status = err.to_string();
            }
        }
    }

    fn load_state(&mut self) -> Result<()> {
        let previous_workspace = self
            .selected_workspace()
            .map(|workspace| workspace.name.clone());
        let previous_monitor = self.selected_monitor().map(|monitor| monitor.name.clone());

        self.workspaces = self.client.workspaces()?;
        self.monitors = self.client.monitors()?;

        select_by_name(
            &mut self.workspace_state,
            &self.workspaces,
            previous_workspace.as_deref(),
            |workspace| &workspace.name,
        );
        select_monitor(
            &mut self.monitor_state,
            &self.monitors,
            previous_monitor.as_deref(),
        );

        Ok(())
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Tab | KeyCode::BackTab => self.toggle_focus(),
            KeyCode::Down | KeyCode::Char('j') => self.next(),
            KeyCode::Up | KeyCode::Char('k') => self.previous(),
            KeyCode::Enter => self.move_selected_workspace(),
            _ => {}
        }
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Pane::Workspaces => Pane::Monitors,
            Pane::Monitors => Pane::Workspaces,
        };
    }

    fn next(&mut self) {
        match self.focus {
            Pane::Workspaces => select_next(&mut self.workspace_state, self.workspaces.len()),
            Pane::Monitors => select_next(&mut self.monitor_state, self.monitors.len()),
        }
    }

    fn previous(&mut self) {
        match self.focus {
            Pane::Workspaces => select_previous(&mut self.workspace_state, self.workspaces.len()),
            Pane::Monitors => select_previous(&mut self.monitor_state, self.monitors.len()),
        }
    }

    fn move_selected_workspace(&mut self) {
        let Some(workspace) = self.selected_workspace().cloned() else {
            self.status = String::from("No workspace selected");
            return;
        };
        let Some(monitor) = self.selected_monitor().cloned() else {
            self.status = String::from("No monitor selected");
            return;
        };

        if workspace.monitor == monitor.name {
            self.status = format!(
                "Workspace {} is already on {}",
                workspace_label(&workspace),
                monitor.name
            );
            return;
        }

        match self.client.move_workspace_to_monitor(&workspace, &monitor) {
            Ok(message) => {
                self.status = message;
                if let Err(err) = self.load_state() {
                    self.status = format!("Moved, but refresh failed: {err}");
                }
            }
            Err(err) => self.status = err.to_string(),
        }
    }

    fn selected_workspace(&self) -> Option<&Workspace> {
        self.workspace_state
            .selected()
            .and_then(|index| self.workspaces.get(index))
    }

    fn selected_monitor(&self) -> Option<&Monitor> {
        self.monitor_state
            .selected()
            .and_then(|index| self.monitors.get(index))
    }
}

fn main() -> Result<()> {
    let client = HyprlandClient::from_env()?;
    let mut terminal = setup_terminal()?;
    let mut app = App::new(client);
    let run_result = run(&mut terminal, &mut app);
    restore_terminal(&mut terminal)?;
    run_result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| render(frame, app))?;

        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        {
            app.handle_key(key.code);
        }
    }

    Ok(())
}

fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BLUE))
            .style(Style::default().bg(theme::BG)),
        area,
    );

    let app_area = area.inner(Margin {
        horizontal: 3,
        vertical: 2,
    });
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(app_area);

    render_header(frame, app, vertical[0]);
    render_workspace_monitor_area(frame, app, vertical[2]);
    render_buttons(frame, vertical[4]);
}

fn render_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "hyprtuird",
            Style::default()
                .fg(theme::CYAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(app.status.as_str(), Style::default().fg(theme::DIM)),
    ]))
    .style(Style::default().fg(theme::FG).bg(theme::BG))
    .block(panel_block(None));

    frame.render_widget(header, area);
}

fn render_workspace_monitor_area(frame: &mut Frame<'_>, app: &App, area: Rect) {
    frame.render_widget(panel_block(Some("Hyprland")), area);

    let content = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(content);

    render_workspace_table(frame, app, columns[0]);
    render_monitor_table(frame, app, columns[1]);
}

fn render_workspace_table(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let selected_index = app.workspace_state.selected();
    let active = app.focus == Pane::Workspaces;
    let rows = app
        .workspaces
        .iter()
        .enumerate()
        .map(|(index, workspace)| {
            let selected = selected_index == Some(index);
            let marker = selection_marker(selected, active);
            let style = row_style(selected, active);

            Row::new([
                Cell::from(marker),
                Cell::from(workspace_label(workspace)),
                Cell::from(workspace.monitor.as_str()),
            ])
            .style(style)
        })
        .collect::<Vec<_>>();

    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Percentage(45),
            Constraint::Percentage(55),
        ],
    )
    .header(table_header(["State", "Workspace", "Monitor"]))
    .column_spacing(2)
    .style(Style::default().fg(theme::FG).bg(theme::BG));

    frame.render_widget(table, area);
}

fn render_monitor_table(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let selected_index = app.monitor_state.selected();
    let active = app.focus == Pane::Monitors;
    let rows = app
        .monitors
        .iter()
        .enumerate()
        .map(|(index, monitor)| {
            let selected = selected_index == Some(index);
            let focused = if monitor.focused { "yes" } else { "" };
            let style = row_style(selected, active);

            Row::new([
                Cell::from(selection_marker(selected, active)),
                Cell::from(monitor.name.as_str()),
                Cell::from(focused),
            ])
            .style(style)
        })
        .collect::<Vec<_>>();

    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Percentage(68),
            Constraint::Percentage(32),
        ],
    )
    .header(table_header(["State", "Monitor", "Focused"]))
    .column_spacing(2)
    .style(Style::default().fg(theme::FG).bg(theme::BG));

    frame.render_widget(table, area);
}

fn render_buttons(frame: &mut Frame<'_>, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(16),
            Constraint::Length(15),
            Constraint::Length(17),
            Constraint::Length(16),
            Constraint::Length(13),
            Constraint::Min(0),
        ])
        .split(area);

    render_button(frame, chunks[0], "k/j", "Select", theme::GREEN);
    render_button(frame, chunks[1], "tab", "Pane", theme::ORANGE);
    render_button(frame, chunks[2], "enter", "Move", theme::BLUE);
    render_button(frame, chunks[3], "r", "Refresh", theme::BLUE);
    render_button(frame, chunks[4], "q", "Quit", theme::RED);
}

fn render_button(
    frame: &mut Frame<'_>,
    area: Rect,
    key: &'static str,
    label: &'static str,
    color: Color,
) {
    if area.width < 3 || area.height < 3 {
        return;
    }

    let button_area = Rect {
        width: area.width.saturating_sub(1),
        ..area
    };
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color))
            .style(Style::default().bg(theme::BG)),
        button_area,
    );

    let inner = button_area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    let text = Paragraph::new(Line::from(vec![
        Span::styled(key, Style::default().fg(color).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(label, Style::default().fg(color)),
    ]))
    .alignment(Alignment::Center)
    .style(Style::default().bg(theme::BG));

    frame.render_widget(text, inner);
}

fn panel_block(title: Option<&'static str>) -> Block<'static> {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().fg(theme::FG).bg(theme::BG));

    match title {
        Some(title) => block.title(title),
        None => block,
    }
}

fn table_header<const N: usize>(columns: [&'static str; N]) -> Row<'static> {
    Row::new(columns.map(|column| {
        Cell::from(column).style(
            Style::default()
                .fg(theme::ORANGE)
                .add_modifier(Modifier::BOLD),
        )
    }))
}

fn selection_marker(selected: bool, active: bool) -> &'static str {
    match (selected, active) {
        (true, true) => ">>",
        (true, false) => ">",
        _ => "",
    }
}

fn row_style(selected: bool, active: bool) -> Style {
    match (selected, active) {
        (true, true) => Style::default()
            .fg(theme::ROW_FG)
            .bg(theme::ROW)
            .add_modifier(Modifier::BOLD),
        (true, false) => Style::default()
            .fg(theme::BLUE)
            .bg(theme::BG)
            .add_modifier(Modifier::BOLD),
        _ => Style::default().fg(theme::FG).bg(theme::BG),
    }
}

fn select_next(state: &mut ListState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }

    let next = match state.selected() {
        Some(index) if index + 1 < len => index + 1,
        _ => 0,
    };
    state.select(Some(next));
}

fn select_previous(state: &mut ListState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }

    let previous = match state.selected() {
        Some(0) | None => len - 1,
        Some(index) => index - 1,
    };
    state.select(Some(previous));
}

fn select_by_name<T>(
    state: &mut ListState,
    items: &[T],
    previous_name: Option<&str>,
    name: impl Fn(&T) -> &str,
) {
    if items.is_empty() {
        state.select(None);
        return;
    }

    let selected = previous_name
        .and_then(|target| items.iter().position(|item| name(item) == target))
        .unwrap_or(0);
    state.select(Some(selected));
}

fn select_monitor(state: &mut ListState, monitors: &[Monitor], previous_name: Option<&str>) {
    if monitors.is_empty() {
        state.select(None);
        return;
    }

    let selected = previous_name
        .and_then(|target| monitors.iter().position(|monitor| monitor.name == target))
        .or_else(|| monitors.iter().position(|monitor| monitor.focused))
        .unwrap_or(0);
    state.select(Some(selected));
}

fn workspace_label(workspace: &Workspace) -> String {
    if workspace.name == workspace.id.to_string() {
        workspace.id.to_string()
    } else {
        workspace.name.clone()
    }
}

fn shell_escape_arg(argument: &str) -> String {
    if argument
        .chars()
        .all(|char| char.is_ascii_alphanumeric() || matches!(char, '_' | '-' | '.' | ':'))
    {
        return argument.to_string();
    }

    format!("'{}'", argument.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_simple_arguments_without_quotes() {
        assert_eq!(shell_escape_arg("DP-1"), "DP-1");
        assert_eq!(shell_escape_arg("name:HDMI-A-1"), "name:HDMI-A-1");
    }

    #[test]
    fn escapes_arguments_with_spaces() {
        assert_eq!(shell_escape_arg("workspace 1"), "'workspace 1'");
    }

    #[test]
    fn escapes_single_quotes() {
        assert_eq!(shell_escape_arg("dev's"), "'dev'\"'\"'s'");
    }

    #[test]
    fn keeps_numeric_workspace_label_short() {
        let workspace = Workspace {
            id: 3,
            name: String::from("3"),
            monitor: String::from("DP-1"),
        };

        assert_eq!(workspace_label(&workspace), "3");
    }
}
