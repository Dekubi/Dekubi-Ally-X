// Dekubi X, GPL-3.0-or-later. After Dekubi Ally (github.com/Dekubi/Dekubi-Ally,
// Fritchii): src/yuzu/dekubi/game_controls.cpp, by Dekubi.
//! Game controls: remap the keyboard's stand-in pad and the pad's own
//! buttons from a plain text file, read once at the first input poll:
//! `DEKUBI_CONTROLS=<file>`, else `controls.txt` next to `Dekubi X.exe`.
//!
//! ```text
//! # keyboard key = pad button (adds to or replaces the default for that key)
//! key Space = A
//! key Enter = Menu
//! # pad button = button the title sees (swap A and B)
//! pad A = B
//! pad B = A
//! ```
//! Buttons: A B X Y Menu View Up Down Left Right LB RB LS RS.
//! Keys: a letter or digit, Enter Esc Tab Back Space Shift Ctrl Alt,
//! Up Down Left Right, F1 to F12.

use std::sync::LazyLock;

use super::gameinput::{
    A, B, DOWN, FALLBACK_BUTTONS, LEFT, LEFT_SHOULDER, LEFT_STICK, MENU, RIGHT, RIGHT_SHOULDER,
    RIGHT_STICK, UP, VIEW, X, Y,
};

/// The parsed file: keyboard map and pad remap (from bit, to bit).
#[derive(Debug, Default, PartialEq)]
pub(super) struct Controls {
    pub keys: Vec<(u8, u32)>,
    pub pad: Vec<(u32, u32)>,
}

static CONTROLS: LazyLock<Controls> = LazyLock::new(|| {
    let path = std::env::var_os("DEKUBI_CONTROLS")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()?
                .parent()
                .map(|d| d.join("controls.txt"))
        });
    let text = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
    let controls = parse(text.as_deref().unwrap_or(""));
    if let Some(p) = path.filter(|_| text.is_some()) {
        tracing::info!(
            file = %p.display(),
            keys = controls.keys.len(),
            pad = controls.pad.len(),
            "game controls loaded"
        );
    }
    controls
});

/// Keyboard key to stand-in pad button, defaults with the file's changes.
pub(super) fn keyboard() -> &'static [(u8, u32)] {
    &CONTROLS.keys
}

/// The pad's buttons as the title should see them.
pub(super) fn remap_pad(buttons: u32) -> u32 {
    remap(&CONTROLS.pad, buttons)
}

fn remap(pad: &[(u32, u32)], buttons: u32) -> u32 {
    if pad.is_empty() {
        return buttons;
    }
    let moved: u32 = pad.iter().map(|(from, _)| from).fold(0, |a, b| a | b);
    pad.iter()
        .filter(|(from, _)| buttons & from != 0)
        .fold(buttons & !moved, |all, (_, to)| all | to)
}

fn button(name: &str) -> Option<u32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "a" => A,
        "b" => B,
        "x" => X,
        "y" => Y,
        "menu" | "start" => MENU,
        "view" | "back" | "select" => VIEW,
        "up" => UP,
        "down" => DOWN,
        "left" => LEFT,
        "right" => RIGHT,
        "lb" => LEFT_SHOULDER,
        "rb" => RIGHT_SHOULDER,
        "ls" => LEFT_STICK,
        "rs" => RIGHT_STICK,
        _ => return None,
    })
}

fn key(name: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "enter" => 0x0D,
        "esc" | "escape" => 0x1B,
        "tab" => 0x09,
        "back" | "backspace" => 0x08,
        "space" => 0x20,
        "shift" => 0xA0,
        "ctrl" => 0xA2,
        "alt" => 0xA4,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        k if k.len() == 1 && k.as_bytes()[0].is_ascii_alphanumeric() => {
            k.as_bytes()[0].to_ascii_uppercase()
        }
        k if k.starts_with('f') => {
            let n: u8 = k[1..].parse().ok()?;
            (1..=12).contains(&n).then_some(0x6F + n)?
        }
        _ => return None,
    })
}

fn parse(text: &str) -> Controls {
    let mut keys: Vec<(u8, u32)> = FALLBACK_BUTTONS.to_vec();
    let mut pad = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((left, right)) = line.split_once('=') else {
            continue;
        };
        let mut words = left.split_whitespace();
        let (Some(kind), Some(name), None) = (words.next(), words.next(), words.next()) else {
            tracing::warn!(line, "game controls: line not understood");
            continue;
        };
        let Some(to) = button(right.trim()) else {
            tracing::warn!(line, "game controls: unknown button");
            continue;
        };
        match kind.to_ascii_lowercase().as_str() {
            "key" => match key(name) {
                Some(vk) => {
                    keys.retain(|(k, _)| *k != vk);
                    keys.push((vk, to));
                }
                None => tracing::warn!(line, "game controls: unknown key"),
            },
            "pad" => match button(name) {
                Some(from) => {
                    pad.retain(|(f, _)| *f != from);
                    pad.push((from, to));
                }
                None => tracing::warn!(line, "game controls: unknown button"),
            },
            _ => tracing::warn!(line, "game controls: line not understood"),
        }
    }
    Controls { keys, pad }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_pad_buttons_are_remapped() {
        let c = parse("key Space = A\nkey Enter = Menu # pause\npad A = B\npad B = A\nbad line\n");
        assert!(c.keys.contains(&(0x20, A)));
        assert!(c.keys.contains(&(0x0D, MENU)));
        assert!(!c.keys.contains(&(0x0D, A)));
        assert_eq!(remap(&c.pad, A | UP), B | UP);
        assert_eq!(remap(&c.pad, A | B), A | B);
        assert_eq!(remap(&[], X), X);
        assert_eq!(key("F10"), Some(0x79));
        assert_eq!(parse("").keys, FALLBACK_BUTTONS.to_vec());
    }
}
