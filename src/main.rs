mod claude;
mod claude_bridge;
mod component;
mod config;
mod core;
mod event_handler;
mod phase_dispatch;
mod image;
mod model_session;
mod parser;
mod phase;
mod preview;
mod prompt_builder;
mod render;
mod python;
mod reference;
mod reference_detect;
mod session_manager;
mod spec;
mod stl;
mod storage;
#[cfg(test)]
mod test_util;
mod tui;
mod usage;
mod viewer;

use crate::config::Config;
use crate::core::{AppCore, BusyState, CoreEvent, Phase, SwitchDenied};
use crate::storage::Project;
use crate::tui::Focus;
use crate::tui::layout::{LayoutConfig, PanelRects, compute_layout};
use crate::tui::input_bar::InputBar;
use crate::tui::conversation::ConversationPane;
use crate::tui::project_tree::ProjectTreePane;
use crate::tui::model_panel::ModelPanel;
use crate::tui::spec_panel::SpecPanel;
use crate::tui::right_panel::RightPanel;
use crate::viewer::Viewer;

use crossterm::event::{self, Event, KeyEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use std::path::PathBuf;
use std::time::Duration;

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

struct App<'a> {
    // Focus and state
    focus: Focus,
    layout_config: LayoutConfig,

    // Panes
    project_tree: ProjectTreePane,
    conversation: ConversationPane,
    model_panel: ModelPanel,
    input_bar: InputBar<'a>,
    spec_panel: SpecPanel,
    right_panel: RightPanel,

    // Backend
    viewer: Viewer,
    core: AppCore,

    // App state
    should_quit: bool,
    dirty: bool,
    spinner_frame: usize,
    /// Cached panel Rects for mouse hit-testing
    panel_rects: PanelRects,
    /// Timestamp of last Ctrl+C press (for double-tap quit)
    last_ctrl_c: Option<std::time::Instant>,

    /// Last session directory seen — used to keep `viewer`'s working
    /// directory in sync with `core`'s active session (core no longer
    /// touches the viewer directly).
    last_session_dir: Option<PathBuf>,
    /// Last `core.reset_generation()` observed — used to detect a wholesale
    /// conversation reset (new session / load session / open project) and
    /// rebuild the conversation pane instead of just draining new messages.
    last_seen_generation: u64,
}

impl<'a> App<'a> {
    fn new(config: Config, briefing: Option<String>) -> Result<Self, String> {
        let core = AppCore::new(config.clone(), briefing)?;

        let mut project_tree = ProjectTreePane::new();
        project_tree.refresh(core.projects());

        let mut viewer = Viewer::new(&config.viewer.command);
        let last_session_dir = core.session_dir().map(|p| p.to_path_buf());
        if let Some(ref dir) = last_session_dir {
            viewer.set_working_dir(dir);
        }

        Ok(App {
            focus: Focus::ProjectTree,
            layout_config: LayoutConfig::default(),
            project_tree,
            conversation: ConversationPane::new(),
            model_panel: ModelPanel::new(),
            input_bar: InputBar::new(),
            spec_panel: SpecPanel::new(),
            right_panel: RightPanel::new(),
            viewer,
            core,
            should_quit: false,
            dirty: true,
            spinner_frame: 0,
            panel_rects: PanelRects::default(),
            last_ctrl_c: None,
            last_session_dir,
            last_seen_generation: 0,
        })
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        // Show message if terminal is too narrow
        if area.width < 40 {
            let msg = Paragraph::new("Terminal too narrow.\nPlease resize to at least 40 columns.")
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(msg, area);
            return;
        }

        // Keep layout phase in sync with app phase
        self.layout_config.phase = self.core.phase();

        // Dynamic input bar height: grows with line count (1 line per row + 2 for border).
        let line_count = self.input_bar.textarea.lines().len();
        self.layout_config.input_height = (line_count as u16 + 2).clamp(5, 9);

        // Phase-aware placeholder text
        let placeholder = match self.core.phase() {
            Phase::Spec => "Describe what you want to build...",
            Phase::Build => "Build instructions, feedback, or 'approve'...",
            Phase::Refine => "Aesthetic changes: chamfers, fillets, finish...",
        };
        self.input_bar.set_placeholder(placeholder);

        let panes = compute_layout(area, &self.layout_config);

        // Cache panel Rects for mouse hit-testing
        self.panel_rects.project_tree = panes.left_panel.unwrap_or_default();
        self.panel_rects.conversation = panes.conversation;
        self.panel_rects.right_panel = panes.right_panel.unwrap_or_default();
        self.panel_rects.input = panes.input_bar;

        // Render left panel — always show project tree (components visible inside session)
        if let Some(left_area) = panes.left_panel {
            self.project_tree.render(frame, left_area, self.focus == Focus::ProjectTree);
        }

        // Render conversation with spinner if busy
        let conv_area = panes.conversation;
        let mut conv = ConversationPane {
            entries: self.conversation.entries.clone(),
            // When auto-scrolling, use MAX so render clamps to actual bottom
            scroll_offset: if self.conversation.auto_scroll { u16::MAX } else { self.conversation.scroll_offset },
            auto_scroll: self.conversation.auto_scroll,
        };
        // Show streaming text or spinner when busy
        if self.core.busy() != BusyState::Idle {
            let spinner_char = SPINNER[self.spinner_frame % SPINNER.len()];
            let streaming_text = self.core.streaming_text();
            let msg = match self.core.busy() {
                BusyState::Thinking => {
                    if streaming_text.is_empty() {
                        format!("{spinner_char} Thinking...")
                    } else {
                        format!("{spinner_char} {}", streaming_text)
                    }
                }
                BusyState::Building => format!("{spinner_char} Building..."),
                BusyState::Idle => unreachable!(),
            };
            conv.entries.push(crate::tui::conversation::ConversationEntry {
                role: if streaming_text.is_empty() { "system" } else { "assistant" }.to_string(),
                content: msg,
            });
        }
        let max_scroll = conv.render(frame, conv_area, self.focus == Focus::Conversation);
        // Write the clamped scroll back so scroll_up() works from a real position
        self.conversation.scroll_offset = self.conversation.scroll_offset.min(max_scroll);

        // Render right panel (unified tabbed panel)
        if let Some(right_area) = panes.right_panel {
            self.right_panel.render(frame, right_area, self.focus == Focus::RightPanel);
        }

        // Render input bar with status indicators
        let bar_area = panes.input_bar;
        let input_focused = self.focus == Focus::Input;
        let border_color = if input_focused {
            Color::Rgb(137, 180, 250)
        } else {
            Color::Rgb(49, 50, 68)
        };

        // Build input bar title with status indicators
        let mut title_spans: Vec<Span> = vec![Span::raw(" Input ")];

        // Attachment indicators — separate images from PDFs
        let pending_images = self.core.pending_images();
        if !pending_images.is_empty() {
            let img_count = pending_images.iter()
                .filter(|p| image::is_image(p))
                .count();
            let pdf_count = pending_images.iter()
                .filter(|p| image::is_pdf(p))
                .count();
            if img_count > 0 {
                title_spans.push(Span::styled(
                    format!(" {img_count} img "),
                    Style::default().fg(Color::Rgb(30, 30, 46)).bg(Color::Rgb(148, 226, 213)),
                ));
                title_spans.push(Span::raw(" "));
            }
            if pdf_count > 0 {
                title_spans.push(Span::styled(
                    format!(" {pdf_count} pdf "),
                    Style::default().fg(Color::Rgb(30, 30, 46)).bg(Color::Rgb(249, 226, 175)),
                ));
                title_spans.push(Span::raw(" "));
            }
        }

        // Busy indicator
        if self.core.busy() != BusyState::Idle {
            let spinner_char = SPINNER[self.spinner_frame % SPINNER.len()];
            let (label, fg, bg) = match self.core.busy() {
                BusyState::Thinking => ("Thinking", Color::Rgb(30, 30, 46), Color::Rgb(203, 166, 247)),
                BusyState::Building => ("Building", Color::Rgb(30, 30, 46), Color::Rgb(249, 226, 175)),
                BusyState::Idle => unreachable!(),
            };
            title_spans.push(Span::styled(
                format!(" {spinner_char} {label} "),
                Style::default().fg(fg).bg(bg),
            ));
        }

        self.input_bar.textarea.set_block(
            ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(border_color))
                .title(Line::from(title_spans))
                .title_style(Style::default().fg(Color::Rgb(147, 153, 178)))
        );
        frame.render_widget(&self.input_bar.textarea, bar_area);

        // Render legend bar
        let legend_area = panes.legend;
        let phase_spans = render::phase_indicator_spans(self.core.phase());
        render::render_legend_bar(frame, legend_area, self.focus, phase_spans);

        // Render usage stats (right-aligned overlay on legend bar)
        let usage_stats = self.core.usage_stats();
        tui::status_bar::render_usage_bar(frame, legend_area, &usage_stats);
    }

    /// Pull whatever changed on `core` into the TUI's own panes.
    fn sync_from_core(&mut self) {
        // Conversation: rebuild wholesale on reset, else drain incrementally.
        let gen = self.core.reset_generation();
        if gen != self.last_seen_generation {
            self.last_seen_generation = gen;
            self.conversation.clear();
            // A wholesale conversation reset means "new session"/"project
            // opened"/"session loaded" — the model metadata shown alongside it
            // no longer applies (pre-refactor `self.model_panel.clear()`).
            self.model_panel.clear();
            for (role, content) in self.core.messages() {
                self.conversation.add(role, content);
            }
            let _ = self.core.take_new_messages(); // already included above
        } else {
            for (role, content) in self.core.take_new_messages() {
                self.conversation.add(&role, &content);
            }
        }

        // Right panel / spec panel mirrors.
        self.right_panel.set_spec(self.core.spec_content());
        if self.spec_panel.content() != self.core.spec_content() {
            self.spec_panel.set_content(self.core.spec_content());
        }
        self.right_panel.set_refs(self.core.refs_summary());
        if !self.core.model_summary().is_empty() {
            self.right_panel.set_model(self.core.model_summary());
        }

        // Keep the viewer's working directory synced with core's active session.
        let cur_dir = self.core.session_dir().map(|p| p.to_path_buf());
        if cur_dir != self.last_session_dir {
            if let Some(ref d) = cur_dir {
                self.viewer.set_working_dir(d);
            }
            self.last_session_dir = cur_dir;
        }

        // Refresh-only working-copy STL update queued by e.g. `undo_component`
        // (never auto-launches the viewer — matches pre-refactor behavior).
        if let Some(stl) = self.core.take_stl_refresh() {
            if let Err(e) = self.viewer.update_working_stl(&stl) {
                self.conversation.add("system", &format!("Warning: {e}"));
            }
        }
    }

    /// Submit a prompt to core and mirror the resulting state into the TUI
    /// panes. Prefer this over calling `core.submit_prompt` directly so the
    /// core->TUI sync is never accidentally skipped at a new call site.
    fn submit(&mut self, text: &str, part_refs: &[String], lib_refs: &[String]) {
        self.core.submit_prompt(text, part_refs, lib_refs);
        // Staged attachments were consumed by the submit (core drains
        // `pending_images`); drop the TUI-side chips too.
        self.model_panel.pending_files.clear();
        self.sync_from_core();
    }

    /// Open a project by index and mirror the resulting state into the TUI.
    fn open_project(&mut self, project_idx: usize) {
        self.core.open_project(project_idx);
        self.model_panel.clear();
        self.sync_from_core();
    }

    /// Load a session by name and mirror the resulting state into the TUI.
    fn load_session(&mut self, project_idx: usize, session_name: String) {
        self.core.load_session(project_idx, session_name.clone());
        self.sync_from_core();

        // The load can fail (core reports it as a system message); only mark
        // the sidebar/viewer when it actually took effect.
        if self.core.session_name() != Some(session_name.as_str()) {
            return;
        }
        let session_dir = self.core.session_dir().map(|p| p.to_path_buf());

        // Sidebar active markers (core can't touch the tree pane).
        self.project_tree.active_project = Some(project_idx);
        self.project_tree.active_session = Some(session_name);

        // Launch the viewer if the session already has a working-copy STL.
        // (`sync_from_core` has already pointed it at the session dir.)
        if let Some(dir) = session_dir {
            if dir.join("_buffer.stl").exists() && !self.viewer.is_running() {
                let _ = self.viewer.show();
            }
        }
    }

    fn handle_core_event(&mut self, event: CoreEvent) {
        match event {
            CoreEvent::StreamDelta(_) => {
                self.conversation.scroll_to_bottom();
            }
            CoreEvent::ToolCall { name, .. } => {
                // Pre-refactor `handle_tool_call` launched the viewer directly
                // for these two tool names; that logic now lives here since
                // core cannot touch the viewer. Behavior preserved verbatim:
                // `write_file` only shows if the working-copy STL already
                // exists; `open_viewer` shows unconditionally (no STL check).
                match name.as_str() {
                    "write_file" => {
                        let buffer_exists = self.core.session_dir()
                            .map(|d| d.join("_buffer.stl").exists())
                            .unwrap_or(false);
                        if buffer_exists && !self.viewer.is_running() {
                            let _ = self.viewer.show();
                        }
                    }
                    "open_viewer" => {
                        if !self.viewer.is_running() {
                            let _ = self.viewer.show();
                        }
                    }
                    _ => {}
                }
            }
            CoreEvent::ResponseDone => {}
            CoreEvent::BuildArtifact { stl } => {
                if let Err(e) = self.viewer.update_working_stl(&stl) {
                    self.conversation.add("system", &format!("Warning: {e}"));
                }
                if !self.viewer.is_running() {
                    let _ = self.viewer.show();
                }
                if let Some(meta) = self.core.model_metadata() {
                    self.model_panel.update(&meta, Some(&stl), self.core.iteration());
                }
            }
            CoreEvent::Error(msg) => {
                self.conversation.add("system", &msg);
            }
        }
    }

    /// Kill any running Claude subprocess on app exit.
    fn cleanup(&self) {
        self.core.cleanup();
    }
}

fn startup_checks(config: &Config) -> Result<(), String> {
    claude::check_claude()?;
    python::check_python(&config.python_path())?;
    if !which_exists(&config.viewer.command) {
        eprintln!("Warning: {} not found. Install for 3D preview.", config.viewer.command);
    }
    Ok(())
}

fn which_exists(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn make_fallback_app<'a>(config: Config, warn: &str) -> App<'a> {
    eprintln!("Warning: {warn}");
    // Even the fallback path needs a working AppCore; if construction fails
    // here there's truly nothing usable to fall back to.
    let core = AppCore::new(config.clone(), None)
        .unwrap_or_else(|e| panic!("Failed to construct fallback AppCore: {e}"));
    let projects: Vec<Project> = core.projects().to_vec();
    let mut pt = ProjectTreePane::new();
    pt.refresh(&projects);
    App {
        focus: Focus::ProjectTree,
        layout_config: LayoutConfig::default(),
        project_tree: pt,
        conversation: ConversationPane::new(),
        model_panel: ModelPanel::new(),
        input_bar: InputBar::new(),
        spec_panel: SpecPanel::new(),
        right_panel: RightPanel::new(),
        viewer: Viewer::new(&config.viewer.command),
        core,
        should_quit: false,
        dirty: true,
        spinner_frame: 0,
        panel_rects: PanelRects::default(),
        last_ctrl_c: None,
        last_session_dir: None,
        last_seen_generation: 0,
    }
}

fn main() {
    let config = Config::load();

    // Non-fatal startup checks — warn but continue
    if let Err(e) = startup_checks(&config) {
        eprintln!("Startup warning: {e}");
    }

    // Read piped stdin before TUI takes over
    use std::io::{IsTerminal, Read};
    let briefing: Option<String> = if !std::io::stdin().is_terminal() {
        let mut buf = Vec::new();
        let max_bytes: usize = 100 * 1024;
        std::io::stdin().lock().take(max_bytes as u64 + 1).read_to_end(&mut buf)
            .unwrap_or_else(|e| {
                eprintln!("Warning: failed to read piped input: {e}");
                0
            });
        let truncated = buf.len() > max_bytes;
        buf.truncate(max_bytes);
        let mut s = String::from_utf8_lossy(&buf).into_owned();
        if truncated {
            s.push_str("\n[...truncated at 100KB]");
        }
        if s.trim().is_empty() {
            None
        } else {
            // Reopen stdin from /dev/tty so crossterm/ratatui can initialize raw mode.
            // SAFETY: open/dup2/close are standard POSIX calls. We only dup2 onto stdin
            // (fd 0) which we've already consumed. The tty fd is closed immediately after.
            #[cfg(unix)]
            unsafe {
                let tty = libc::open(b"/dev/tty\0".as_ptr() as *const _, libc::O_RDWR);
                if tty >= 0 {
                    libc::dup2(tty, libc::STDIN_FILENO);
                    libc::close(tty);
                }
            }
            Some(s)
        }
    } else {
        None
    };

    let mut app = match App::new(config.clone(), briefing) {
        Ok(app) => app,
        Err(e) => make_fallback_app(config, &e),
    };

    // Initialize ratatui terminal
    let mut terminal = ratatui::init();

    // Enable bracketed paste for drag-and-drop file detection and mouse capture
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableMouseCapture,
    );

    // Run event loop
    let result = run_event_loop(&mut terminal, &mut app);

    // Kill any running Claude subprocess before exiting
    app.cleanup();

    // Disable bracketed paste and mouse capture before restoring terminal
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableMouseCapture,
    );

    // Restore terminal
    ratatui::restore();

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run_event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
) -> std::io::Result<()> {
    let mut tick_count: u64 = 0;

    loop {
        let events = app.core.poll_events();
        if !events.is_empty() {
            app.sync_from_core();
            for ev in events {
                app.handle_core_event(ev);
            }
            app.dirty = true;
        }
        // Core state changes that render but carry no event (busy-state
        // transitions, `.open_viewer` with no STL yet) still force a repaint.
        if app.core.take_repaint_request() {
            app.dirty = true;
        }

        // Auto-submit briefing prompt after first render (tick_count > 0 ensures one frame rendered first)
        if app.core.briefing_pending() && tick_count > 0 {
            app.core.clear_briefing_pending();
            app.focus = Focus::Input;
            let synthetic = "Please review the attached conversation and begin extracting spec fields.".to_string();
            app.submit(&synthetic, &[], &[]);
            app.dirty = true;
        }

        // Render only when dirty
        if app.dirty {
            terminal.draw(|f| app.render(f))?;
            app.dirty = false;
        }

        // Poll for events with 50ms timeout
        if crossterm::event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    app.handle_key(key);
                    app.dirty = true;
                }
                Event::Paste(text) => {
                    app.handle_paste(text);
                    app.dirty = true;
                }
                Event::Mouse(mouse) => {
                    use crossterm::event::{MouseEventKind, MouseButton};
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            let pos = ratatui::prelude::Position::new(mouse.column, mouse.row);
                            if app.panel_rects.project_tree.contains(pos) {
                                app.focus = Focus::ProjectTree;
                            } else if app.panel_rects.conversation.contains(pos) {
                                app.focus = Focus::Conversation;
                            } else if app.panel_rects.right_panel.contains(pos) {
                                app.focus = Focus::RightPanel;
                            } else if app.panel_rects.input.contains(pos) {
                                app.focus = Focus::Input;
                            }
                            app.dirty = true;
                        }
                        MouseEventKind::ScrollUp => {
                            let pos = ratatui::prelude::Position::new(mouse.column, mouse.row);
                            if app.panel_rects.conversation.contains(pos) {
                                app.conversation.scroll_up(3);
                            } else if app.panel_rects.right_panel.contains(pos) {
                                app.right_panel.scroll_up(3);
                            }
                            app.dirty = true;
                        }
                        MouseEventKind::ScrollDown => {
                            let pos = ratatui::prelude::Position::new(mouse.column, mouse.row);
                            if app.panel_rects.conversation.contains(pos) {
                                app.conversation.scroll_down(3);
                            } else if app.panel_rects.right_panel.contains(pos) {
                                app.right_panel.scroll_down(3);
                            }
                            app.dirty = true;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        // Advance spinner at ~10fps (every 5th loop at 50ms = 250ms period)
        if app.core.busy() != BusyState::Idle && tick_count % 5 == 0 {
            app.spinner_frame = app.spinner_frame.wrapping_add(1);
            app.dirty = true;
        }

        tick_count = tick_count.wrapping_add(1);

        if app.should_quit {
            break;
        }
    }

    Ok(())
}
