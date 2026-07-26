//! klawply — a terminal AI agent that applies to jobs for you, driven by your
//! ChatGPT subscription. Green-on-black, seven-segment styling; same TUI stack
//! as pekchat-tui (ratatui + tokio channels).

mod agent;
mod app;
mod auth;
mod browser;
mod config;
mod event;
mod fields;
mod llm;
mod log;
mod selection;
mod theme;
mod ui;

use ratatui::crossterm::event as term_event;
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use std::io::{Write, stdout};
use tokio::runtime::Handle;

use crate::app::App;

fn main() -> std::io::Result<()> {
    config::ensure_data_dir();
    log::init();
    // We need the IO/time drivers (loopback OAuth server, HTTPS to OpenAI, the
    // CDP websocket), so unlike pekchat this is a full multi-thread runtime.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let handle = runtime.handle().clone();
    runtime.block_on(run(handle))
}

async fn run(handle: Handle) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let _guard = TermGuard::enable()?;

    // Channels between the UI and the background agent thread.
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (evt_tx, evt_rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || agent::run(handle, cmd_rx, evt_tx));

    let mut app = App::new(cmd_tx, evt_rx);
    app.start_connect();

    // Terminal input on its own blocking thread, forwarded as events so the
    // loop can await input and agent events together.
    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    spawn_input_reader(input_tx);

    terminal.draw(|frame| ui::draw(frame, &mut app))?;

    while !app.should_quit {
        // While the agent is working, wake the loop periodically so the loading
        // animation advances even though no input/agent events are arriving.
        // When idle, this branch parks forever and we only wake on real events.
        let busy = app.busy;
        tokio::select! {
            maybe_input = input_rx.recv() => match maybe_input {
                Some(event) => handle_terminal_event(&mut app, event),
                None => break,
            },
            maybe_event = app.event_rx.recv() => {
                if let Some(event) = maybe_event {
                    app.handle_event(event);
                    // Collapse a burst of agent events into one redraw.
                    while let Ok(next) = app.event_rx.try_recv() {
                        app.handle_event(next);
                    }
                }
            }
            _ = async move {
                if busy {
                    tokio::time::sleep(std::time::Duration::from_millis(90)).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {}
        }
        terminal.draw(|frame| ui::draw(frame, &mut app))?;
    }

    ratatui::restore();
    Ok(())
}

fn handle_terminal_event(app: &mut App, event: Event) {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            // If text is selected, Ctrl+C copies it instead of quitting.
            let is_ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'));
            if is_ctrl_c {
                if let Some(text) = app.finish_selection() {
                    let _ = copy_to_clipboard(&text);
                    return;
                }
            }
            app.on_key(key);
            // A keypress may have queued text for the clipboard (e.g. "c" on the
            // connect screen copies the authorize link).
            if let Some(text) = app.take_pending_copy() {
                let _ = copy_to_clipboard(&text);
            }
        }
        Event::Paste(text) => app.on_paste(&text),
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => app.scroll_up(3),
            MouseEventKind::ScrollDown => app.scroll_down(3),
            MouseEventKind::Down(MouseButton::Left) => {
                app.begin_selection(mouse.column, mouse.row)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                app.update_selection(mouse.column, mouse.row)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(text) = app.finish_selection() {
                    let _ = copy_to_clipboard(&text);
                }
            }
            _ => {}
        },
        _ => {}
    }
}

/// Copy `text` to the system clipboard via the OSC 52 terminal escape. Works
/// over SSH and inside multiplexers when the terminal supports it, with no
/// external dependencies. (Ported from pekchat-tui.)
fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    let mut out = stdout();
    write!(out, "\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))?;
    out.flush()
}

/// Minimal standard base64 encoder for the OSC 52 payload.
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(triple >> 18 & 0x3f) as usize] as char);
        out.push(TABLE[(triple >> 12 & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(triple >> 6 & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn spawn_input_reader(tx: tokio::sync::mpsc::UnboundedSender<Event>) {
    std::thread::spawn(move || {
        loop {
            match term_event::read() {
                Ok(event) => {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

/// Enables bracketed paste (so multi-line resume/link pastes arrive intact) and
/// mouse capture, and restores both on drop.
struct TermGuard;

impl TermGuard {
    fn enable() -> std::io::Result<Self> {
        execute!(stdout(), EnableBracketedPaste, EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableBracketedPaste, DisableMouseCapture);
    }
}
