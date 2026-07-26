//! The look: green phosphor on black, like a seven-segment watch display.

use ratatui::style::{Color, Modifier, Style};

/// Classic terminal-green. A touch brighter than pure lime so it glows.
pub const GREEN: Color = Color::Rgb(0, 255, 102);
/// Dimmer green for secondary text / inactive segments.
pub const GREEN_DIM: Color = Color::Rgb(0, 120, 48);
pub const BLACK: Color = Color::Rgb(0, 0, 0);
/// Amber accent for prompts / codes the user must act on.
pub const AMBER: Color = Color::Rgb(255, 176, 0);
/// Red for errors.
pub const RED: Color = Color::Rgb(255, 64, 64);

pub fn base() -> Style {
    Style::default().fg(GREEN).bg(BLACK)
}

pub fn dim() -> Style {
    Style::default().fg(GREEN_DIM).bg(BLACK)
}

pub fn bright() -> Style {
    Style::default()
        .fg(GREEN)
        .bg(BLACK)
        .add_modifier(Modifier::BOLD)
}

pub fn accent() -> Style {
    Style::default()
        .fg(AMBER)
        .bg(BLACK)
        .add_modifier(Modifier::BOLD)
}

pub fn error() -> Style {
    Style::default().fg(RED).bg(BLACK)
}
