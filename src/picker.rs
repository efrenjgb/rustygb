//! The ROM picker: a list of the games in the library, drawn in the emulator
//! window and controlled with the keyboard or a controller.

use crate::gamepad::Gamepads;
use crate::library::Game;
use crate::{PALETTE, WINDOW_HEIGHT, WINDOW_WIDTH};
use font8x8::{BASIC_FONTS, UnicodeFonts};
use minifb::{Key, KeyRepeat, Window};
use rustygb::joypad::Button;
use std::path::Path;

const BACKGROUND: u32 = PALETTE[0];
const TEXT: u32 = PALETTE[3];
const DIM_TEXT: u32 = PALETTE[2];

const MARGIN: usize = 16;
const LIST_TOP: usize = 72;
const ROW_HEIGHT: usize = 20;
const VISIBLE_ROWS: usize = (WINDOW_HEIGHT - LIST_TOP - 40) / ROW_HEIGHT;

/// While a controller direction is held, the selection moves again after
/// this many frames, then every few frames.
const REPEAT_DELAY_FRAMES: u32 = 18;
const REPEAT_INTERVAL_FRAMES: u32 = 4;

pub enum Choice {
    Play(usize),
    Quit,
}

pub struct Picker {
    selected: usize,
    /// The first game shown at the top of the list.
    scroll: usize,
    /// How many frames the controller has held up or down.
    held_frames: u32,
    message: Option<String>,
}

impl Picker {
    pub fn new() -> Self {
        Picker {
            selected: 0,
            scroll: 0,
            held_frames: 0,
            message: None,
        }
    }

    /// Shows the list until a game is chosen or the user quits.
    pub fn run(
        &mut self,
        window: &mut Window,
        gamepads: &mut Option<Gamepads>,
        games: &[Game],
        folder: &Path,
        pixels: &mut [u32],
    ) -> Choice {
        window.set_title("rustygb - library");
        window.set_target_fps(60);
        // Esc or a button still held from the game shouldn't immediately act
        // here, so these only count once they've been released and pressed again.
        let mut escape_was_down = true;
        let mut confirm_was_down = true;

        while window.is_open() {
            if let Some(pads) = gamepads.as_mut() {
                pads.update();
            }
            let pad = |button| gamepads.as_ref().is_some_and(|pads| pads.is_pressed(button));

            let escape_down = window.is_key_down(Key::Escape);
            if escape_down && !escape_was_down {
                return Choice::Quit;
            }
            escape_was_down = escape_down;

            let mut movement: isize = 0;
            for key in window.get_keys_pressed(KeyRepeat::Yes) {
                match key {
                    Key::Up => movement -= 1,
                    Key::Down => movement += 1,
                    Key::PageUp => movement -= VISIBLE_ROWS as isize,
                    Key::PageDown => movement += VISIBLE_ROWS as isize,
                    _ => {}
                }
            }
            movement += self.controller_movement(pad(Button::Up), pad(Button::Down));
            if movement != 0 && !games.is_empty() {
                let last = games.len() as isize - 1;
                self.selected = (self.selected as isize + movement).clamp(0, last) as usize;
                self.message = None;
            }

            let confirm_down =
                window.is_key_down(Key::Enter) || pad(Button::A) || pad(Button::Start);
            let confirmed = confirm_down && !confirm_was_down;
            confirm_was_down = confirm_down;
            if confirmed && let Some(game) = games.get(self.selected) {
                match &game.problem {
                    None => return Choice::Play(self.selected),
                    Some(problem) => self.message = Some(format!("Can't run this game: {problem}")),
                }
            }

            self.draw(games, folder, pixels);
            window
                .update_with_buffer(pixels, WINDOW_WIDTH, WINDOW_HEIGHT)
                .expect("could not draw frame");
        }
        Choice::Quit
    }

    /// Turns a held controller direction into single steps with key-like repeat.
    fn controller_movement(&mut self, up: bool, down: bool) -> isize {
        let direction = match (up, down) {
            (true, false) => -1,
            (false, true) => 1,
            _ => {
                self.held_frames = 0;
                return 0;
            }
        };
        self.held_frames += 1;
        let repeating = self.held_frames > REPEAT_DELAY_FRAMES
            && self.held_frames.is_multiple_of(REPEAT_INTERVAL_FRAMES);
        if self.held_frames == 1 || repeating {
            direction
        } else {
            0
        }
    }

    fn draw(&mut self, games: &[Game], folder: &Path, pixels: &mut [u32]) {
        pixels.fill(BACKGROUND);

        draw_text(pixels, MARGIN, 16, "ROM LIBRARY", 2, TEXT);
        let count = format!("{} games", games.len());
        draw_text(pixels, WINDOW_WIDTH - MARGIN - count.len() * 16, 16, &count, 2, DIM_TEXT);
        let folder_text = fit(&folder.display().to_string(), (WINDOW_WIDTH - 2 * MARGIN) / 8);
        draw_text(pixels, MARGIN, 42, &folder_text, 1, DIM_TEXT);
        fill_rect(pixels, MARGIN, 60, WINDOW_WIDTH - 2 * MARGIN, 2, DIM_TEXT);

        if games.is_empty() {
            draw_text(pixels, MARGIN, LIST_TOP + 8, "No .gb or .gbc files here", 2, TEXT);
        }

        // Keep the selected game visible.
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + VISIBLE_ROWS {
            self.scroll = self.selected + 1 - VISIBLE_ROWS;
        }

        let max_chars = (WINDOW_WIDTH - 2 * MARGIN) / 16;
        for (row, (index, game)) in games
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(VISIBLE_ROWS)
            .enumerate()
        {
            let y = LIST_TOP + row * ROW_HEIGHT;
            let selected = index == self.selected;
            let (text_color, tag_color) = match (selected, game.problem.is_some()) {
                (true, _) => (BACKGROUND, BACKGROUND),
                (false, false) => (TEXT, DIM_TEXT),
                (false, true) => (DIM_TEXT, DIM_TEXT),
            };
            if selected {
                fill_rect(pixels, MARGIN - 6, y - 2, WINDOW_WIDTH - 2 * MARGIN + 12, ROW_HEIGHT, TEXT);
            }

            let tag = game.problem.as_deref().unwrap_or("");
            let name_chars = max_chars - if tag.is_empty() { 0 } else { tag.len() / 2 + 2 };
            draw_text(pixels, MARGIN, y, &fit(&game.name, name_chars), 2, text_color);
            let tag_x = WINDOW_WIDTH - MARGIN - tag.len() * 8;
            draw_text(pixels, tag_x, y + 4, tag, 1, tag_color);
        }

        let footer_y = WINDOW_HEIGHT - 24;
        let footer = self
            .message
            .clone()
            .unwrap_or_else(|| "Enter/A: play   Arrows/D-pad: move   Esc: quit".to_string());
        draw_text(pixels, MARGIN, footer_y, &footer, 1, TEXT);
    }
}

/// Shortens text to at most `max_chars`, ending in ".." if it was cut.
fn fit(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars.saturating_sub(2)).collect();
    kept + ".."
}

/// Draws text with the 8x8 font, each font pixel `scale` screen pixels wide.
fn draw_text(pixels: &mut [u32], x: usize, y: usize, text: &str, scale: usize, color: u32) {
    for (i, character) in text.chars().enumerate() {
        let glyph = BASIC_FONTS.get(character).or_else(|| BASIC_FONTS.get('?'));
        let Some(glyph) = glyph else { continue };
        let glyph_x = x + i * 8 * scale;
        for (row, bits) in glyph.iter().enumerate() {
            for column in 0..8 {
                // Bit 0 is the leftmost pixel.
                if bits & (1 << column) != 0 {
                    let px = glyph_x + column * scale;
                    let py = y + row * scale;
                    fill_rect(pixels, px, py, scale, scale, color);
                }
            }
        }
    }
}

fn fill_rect(pixels: &mut [u32], x: usize, y: usize, width: usize, height: usize, color: u32) {
    for row in y..(y + height).min(WINDOW_HEIGHT) {
        let start = row * WINDOW_WIDTH + x.min(WINDOW_WIDTH);
        let end = row * WINDOW_WIDTH + (x + width).min(WINDOW_WIDTH);
        pixels[start..end].fill(color);
    }
}
