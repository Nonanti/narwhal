//! Translate `crossterm` key events into the back-end-agnostic
//! representation consumed by [`narwhal_vim`].

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_vim::{Key, KeyCode, KeyMod};

pub const fn translate_key_event(event: KeyEvent) -> Option<Key> {
    let code = match event.code {
        CtKey::Char(c) => KeyCode::Char(c),
        CtKey::Enter => KeyCode::Enter,
        CtKey::Esc => KeyCode::Esc,
        CtKey::Backspace => KeyCode::Backspace,
        CtKey::Tab => KeyCode::Tab,
        CtKey::Up => KeyCode::Up,
        CtKey::Down => KeyCode::Down,
        CtKey::Left => KeyCode::Left,
        CtKey::Right => KeyCode::Right,
        CtKey::Home => KeyCode::Home,
        CtKey::End => KeyCode::End,
        CtKey::PageUp => KeyCode::PageUp,
        CtKey::PageDown => KeyCode::PageDown,
        _ => return None,
    };

    let mut mods = KeyMod::NONE;
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        mods = mods.with(KeyMod::CTRL);
    }
    if event.modifiers.contains(KeyModifiers::ALT) {
        mods = mods.with(KeyMod::ALT);
    }
    if event.modifiers.contains(KeyModifiers::SHIFT) {
        mods = mods.with(KeyMod::SHIFT);
    }
    Some(Key { code, mods })
}
