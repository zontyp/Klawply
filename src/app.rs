//! UI-side state: the current screen, the chat transcript, the input buffer,
//! and the reducer that folds [`AgentEvent`]s into state. All network/browser
//! work happens on the agent thread; this file only ever touches memory.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::event::{AgentCommand, AgentEvent};
use crate::selection::Selection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Connecting the ChatGPT subscription.
    Connect,
    /// Pasting resume text (shown when `resume.txt` is blank).
    Resume,
    /// The main chat / apply loop.
    Chat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    User,
    Assistant,
    System,
    Error,
}

#[derive(Debug, Clone)]
pub struct Line {
    pub kind: LineKind,
    pub text: String,
}

pub struct App {
    pub should_quit: bool,
    pub screen: Screen,
    pub transcript: Vec<Line>,
    /// Chat input, or the resume editor buffer on the resume screen.
    pub input: String,
    /// Header status / spinner text.
    pub status: String,
    /// Authorize link shown on the connect screen while waiting.
    pub connect_url: Option<String>,
    pub email: String,
    pub plan: String,
    /// True while the agent is working — locks input and drives the spinner.
    pub busy: bool,
    /// True once the OAuth flow has begun, so the connect screen stops inviting
    /// the user to press Enter.
    pub connecting: bool,
    /// Lines scrolled up from the bottom of the transcript (0 = pinned).
    pub scroll: u16,
    /// When the last character was typed/pasted — used to tell a manual Enter
    /// (submit) apart from newlines arriving inside a fast paste burst.
    last_input: std::time::Instant,

    /// Active mouse text selection over the chat transcript, if any.
    selection: Option<Selection>,
    /// The exact wrapped visual lines of the transcript as last rendered, so
    /// mouse cells can be mapped back to text (and the selection extracted).
    pub chat_lines: Vec<String>,
    /// Inner rectangle those lines were drawn in.
    pub chat_area: Rect,
    /// Index of the first visible transcript line (top of the view).
    pub chat_top: usize,
    /// Text queued to be copied to the clipboard; drained by the event loop
    /// (which owns terminal IO) after each keypress.
    pending_copy: Option<String>,
    /// True while an assistant reply is streaming, so chunks append to the same
    /// transcript line instead of starting new ones.
    streaming: bool,

    cmd_tx: UnboundedSender<AgentCommand>,
    pub event_rx: UnboundedReceiver<AgentEvent>,
}

impl App {
    pub fn new(
        cmd_tx: UnboundedSender<AgentCommand>,
        event_rx: UnboundedReceiver<AgentEvent>,
    ) -> Self {
        Self {
            should_quit: false,
            screen: Screen::Connect,
            transcript: Vec::new(),
            input: String::new(),
            status: String::new(),
            connect_url: None,
            email: String::new(),
            plan: String::new(),
            busy: false,
            connecting: false,
            scroll: 0,
            last_input: std::time::Instant::now(),
            selection: None,
            chat_lines: Vec::new(),
            chat_area: Rect::default(),
            chat_top: 0,
            pending_copy: None,
            streaming: false,
            cmd_tx,
            event_rx,
        }
    }

    fn send(&self, cmd: AgentCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Kick off the connection at startup.
    pub fn start_connect(&mut self) {
        self.connecting = true;
        self.status = "connecting to ChatGPT…".into();
        self.send(AgentCommand::Connect);
    }

    fn push(&mut self, kind: LineKind, text: impl Into<String>) {
        self.transcript.push(Line {
            kind,
            text: text.into(),
        });
        self.scroll = 0; // pin to bottom on new content
        // New content shifts line indices, so any pending selection is stale.
        self.selection = None;
        // Any explicitly-pushed line ends the current streaming reply.
        self.streaming = false;
    }

    /// Append a streamed reply fragment: extend the current assistant line while
    /// streaming, otherwise start a fresh one.
    fn append_assistant_chunk(&mut self, delta: &str) {
        let extend = self.streaming
            && matches!(self.transcript.last().map(|l| l.kind), Some(LineKind::Assistant));
        if extend {
            if let Some(last) = self.transcript.last_mut() {
                last.text.push_str(delta);
            }
        } else {
            self.transcript.push(Line {
                kind: LineKind::Assistant,
                text: delta.to_string(),
            });
            self.streaming = true;
        }
        self.selection = None;
        self.scroll = 0;
    }

    // -- text selection (mouse drag → copy), ported from pekchat -----------

    /// Let the renderer publish the wrapped transcript lines and their text
    /// rectangle so mouse cells can be mapped back to content positions.
    pub fn set_chat_view(&mut self, lines: Vec<String>, area: Rect, top: usize) {
        self.chat_lines = lines;
        self.chat_area = area;
        self.chat_top = top;
    }

    /// The ordered selection range (content coordinates), for the highlighter.
    pub fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        self.selection.as_ref().and_then(Selection::range)
    }

    /// Record a selection anchor at a mouse cell. A click outside the chat text
    /// clears any selection.
    pub fn begin_selection(&mut self, col: u16, row: u16) {
        self.selection = self.content_pos(col, row, false).map(Selection::new);
    }

    /// Extend the active selection to a mouse cell while dragging.
    pub fn update_selection(&mut self, col: u16, row: u16) {
        if let Some(pos) = self.content_pos(col, row, true) {
            if let Some(selection) = self.selection.as_mut() {
                selection.drag(pos);
            }
        }
    }

    /// Finish a selection (mouse release or Ctrl+C). Returns the selected text
    /// when a drag actually happened, else `None`. An active selection is kept
    /// highlighted after release (so Ctrl+C can copy it too, like pekchat); only
    /// a plain click (no drag) clears it.
    pub fn finish_selection(&mut self) -> Option<String> {
        let text = self
            .selection
            .as_ref()
            .filter(|selection| selection.is_active())
            .map(|selection| selection.extract(&self.chat_lines));
        if text.is_none() {
            self.selection = None;
        }
        text
    }

    /// Map a screen cell to a `(line, column)` content position. Initial clicks
    /// (`clamp == false`) must land inside the chat text area; drags
    /// (`clamp == true`) are clamped into it so selection can extend past edges.
    fn content_pos(&self, col: u16, row: u16, clamp: bool) -> Option<(usize, usize)> {
        let area = self.chat_area;
        if area.width == 0 || area.height == 0 || self.chat_lines.is_empty() {
            return None;
        }
        let inside = col >= area.x
            && col < area.x + area.width
            && row >= area.y
            && row < area.y + area.height;
        if !inside && !clamp {
            return None;
        }
        let clamped_row = row.clamp(area.y, area.y + area.height - 1);
        let clamped_col = col.clamp(area.x, area.x + area.width - 1);
        let line =
            (self.chat_top + (clamped_row - area.y) as usize).min(self.chat_lines.len() - 1);
        let column = (clamped_col - area.x) as usize;
        let line_len = self.chat_lines[line].chars().count();
        Some((line, column.min(line_len)))
    }

    // -- agent events ------------------------------------------------------

    pub fn handle_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Status(s) => self.status = s,
            AgentEvent::Busy(on) => {
                self.busy = on;
                if !on {
                    self.status.clear();
                }
            }
            AgentEvent::ConnectPrompt { url } => {
                // Also drop it into the transcript so it's visible on any screen
                // (e.g. when triggered by /login from the chat window).
                self.push(
                    LineKind::System,
                    format!("Log in to ChatGPT in your browser. If it didn't open: {url}"),
                );
                self.connect_url = Some(url);
                self.status = "waiting for you to authorize in the browser…".into();
            }
            AgentEvent::Connected { email, plan } => {
                self.email = email;
                self.plan = plan;
                self.connect_url = None;
                self.status = "connected".into();
            }
            AgentEvent::NeedResume => {
                self.screen = Screen::Resume;
                self.push(
                    LineKind::System,
                    "Paste your resume text above, then press Enter (or Ctrl+D) to save it. \
                     klawply will read it and fill in your application fields.",
                );
            }
            AgentEvent::Ready => self.screen = Screen::Chat,
            AgentEvent::AssistantChunk(delta) => self.append_assistant_chunk(&delta),
            AgentEvent::System(text) => self.push(LineKind::System, text),
            AgentEvent::Error(text) => self.push(LineKind::Error, text),
            AgentEvent::FieldsUpdated { changed } => {
                if !changed.is_empty() {
                    self.push(
                        LineKind::System,
                        format!("Updated fields.json: {}", changed.join(", ")),
                    );
                }
            }
            AgentEvent::Applied {
                url,
                filled,
                failed,
            } => {
                let mut msg = format!("Filled {filled} field(s) on {url}.");
                if !failed.is_empty() {
                    msg.push_str(&format!(
                        "\nCould not fill {}: {}",
                        failed.len(),
                        failed.join("; ")
                    ));
                }
                msg.push_str("\nReview the page in Chrome and submit when you're happy.");
                self.push(LineKind::System, msg);
            }
        }
    }

    // -- input -------------------------------------------------------------

    pub fn on_paste(&mut self, text: &str) {
        if matches!(self.screen, Screen::Resume | Screen::Chat) {
            self.input.push_str(text);
            self.last_input = std::time::Instant::now();
        }
    }

    fn submit_resume(&mut self) {
        if !self.input.trim().is_empty() && !self.busy {
            let text = std::mem::take(&mut self.input);
            self.send(AgentCommand::SubmitResume(text));
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Global quit.
        if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C')) {
            self.quit();
            return;
        }

        match self.screen {
            Screen::Connect => self.on_key_connect(key),
            Screen::Resume => self.on_key_resume(key, ctrl),
            Screen::Chat => self.on_key_chat(key, ctrl),
        }
    }

    fn on_key_connect(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => self.quit(),
            // Copy the authorize link to the clipboard so it can be pasted into
            // any browser (useful on WSL/headless where auto-open fails).
            KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(url) = &self.connect_url {
                    self.pending_copy = Some(url.clone());
                }
            }
            KeyCode::Enter if !self.busy => {
                // Retry / (re)start the connection.
                self.start_connect();
            }
            _ => {}
        }
    }

    /// Take any text queued for the clipboard (the event loop performs the copy).
    pub fn take_pending_copy(&mut self) -> Option<String> {
        self.pending_copy.take()
    }

    fn on_key_resume(&mut self, key: KeyEvent, ctrl: bool) {
        // Ctrl+D / Ctrl+S always submit. (Ctrl+S can be swallowed by terminal
        // flow-control, which is why Enter submits too — see below.)
        if ctrl
            && matches!(
                key.code,
                KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Char('d') | KeyCode::Char('D')
            )
        {
            self.submit_resume();
            return;
        }
        match key.code {
            KeyCode::Char(c) => {
                self.input.push(c);
                self.last_input = std::time::Instant::now();
            }
            KeyCode::Enter => {
                // A bracketed paste delivers its newlines via on_paste, so a real
                // Enter key means "submit". But if the terminal lacks bracketed
                // paste, pasted newlines arrive as Enter keys in a fast burst —
                // treat those as literal newlines instead of submitting early.
                if self.last_input.elapsed() < std::time::Duration::from_millis(40) {
                    self.input.push('\n');
                    self.last_input = std::time::Instant::now();
                } else {
                    self.submit_resume();
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::PageUp => self.scroll_up(5),
            KeyCode::PageDown => self.scroll_down(5),
            _ => {}
        }
    }

    fn on_key_chat(&mut self, key: KeyEvent, ctrl: bool) {
        if ctrl && matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R')) {
            self.send(AgentCommand::SyncFromBrowser);
            return;
        }
        match key.code {
            KeyCode::Char(c) => self.input.push(c),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Enter => {
                let text = self.input.trim().to_string();
                if !text.is_empty() && !self.busy {
                    self.input.clear();
                    self.push(LineKind::User, text.clone());
                    self.send(AgentCommand::UserMessage(text));
                }
            }
            KeyCode::PageUp => self.scroll_up(5),
            KeyCode::PageDown => self.scroll_down(5),
            KeyCode::Home => self.scroll = u16::MAX / 2,
            KeyCode::End => self.scroll = 0,
            _ => {}
        }
    }

    pub fn scroll_up(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_add(n);
    }

    pub fn scroll_down(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    fn quit(&mut self) {
        self.send(AgentCommand::Quit);
        self.should_quit = true;
    }
}
