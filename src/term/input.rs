//! Native console input: a reader thread turning `INPUT_RECORD`s into [`Event`]s.

use std::thread;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{
    DOUBLE_CLICK, FROM_LEFT_1ST_BUTTON_PRESSED, FROM_LEFT_2ND_BUTTON_PRESSED, INPUT_RECORD,
    KEY_EVENT, LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED, MOUSE_EVENT, MOUSE_HWHEELED, MOUSE_MOVED,
    MOUSE_WHEELED, ReadConsoleInputW, RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED,
    RIGHTMOST_BUTTON_PRESSED, SHIFT_PRESSED, WINDOW_BUFFER_SIZE_EVENT,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Esc,
    Tab,
    Backspace,
    Delete,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Down(Button),
    Up(Button),
    DoubleClick(Button),
    /// Mouse moved while `Button` is held.
    Drag(Button),
    Move,
    /// Vertical wheel. Positive is "up" (away from the user), in notches.
    Wheel(i32),
    /// Horizontal wheel. Positive is "right", in notches.
    HWheel(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub x: u16,
    pub y: u16,
    pub ctrl: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize,
}

// Virtual key codes (winuser.h); hard-coded to avoid pulling in the UI feature set.
const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_ESCAPE: u16 = 0x1B;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_DELETE: u16 = 0x2E;
const VK_F1: u16 = 0x70;
const VK_F24: u16 = 0x87;

/// Spawn the input reader thread. `sink` returns `false` to stop the thread.
pub fn spawn_reader<F>(stdin: HANDLE, sink: F)
where
    F: FnMut(Event) -> bool + Send + 'static,
{
    let stdin = stdin as usize;
    thread::Builder::new()
        .name("console-input".into())
        .spawn(move || reader_loop(stdin as HANDLE, sink))
        .expect("spawn input thread");
}

fn reader_loop<F>(stdin: HANDLE, mut sink: F)
where
    F: FnMut(Event) -> bool,
{
    let mut records: [INPUT_RECORD; 64] = [INPUT_RECORD::default(); 64];
    let mut buttons: u32 = 0;
    let mut pending_surrogate: Option<u16> = None;
    loop {
        let mut read = 0u32;
        // SAFETY: buffer is a valid array of INPUT_RECORD with the given capacity.
        let ok = unsafe {
            ReadConsoleInputW(stdin, records.as_mut_ptr(), records.len() as u32, &mut read)
        };
        if ok == 0 {
            return;
        }
        for rec in &records[..read as usize] {
            let events = translate(rec, &mut buttons, &mut pending_surrogate);
            for ev in events {
                if !sink(ev) {
                    return;
                }
            }
        }
    }
}

fn translate(
    rec: &INPUT_RECORD,
    buttons: &mut u32,
    pending_surrogate: &mut Option<u16>,
) -> Vec<Event> {
    let mut out = Vec::new();
    match rec.EventType as u32 {
        KEY_EVENT => {
            // SAFETY: EventType says this is a key record.
            let k = unsafe { rec.Event.KeyEvent };
            if k.bKeyDown == 0 {
                return out;
            }
            let state = k.dwControlKeyState;
            let ctrl = state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0;
            let alt = state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0;
            let shift = state & SHIFT_PRESSED != 0;
            // SAFETY: union read of the UTF-16 code unit.
            let unit = unsafe { k.uChar.UnicodeChar };
            let vk = k.wVirtualKeyCode;
            let code = match vk {
                VK_UP => Some(KeyCode::Up),
                VK_DOWN => Some(KeyCode::Down),
                VK_LEFT => Some(KeyCode::Left),
                VK_RIGHT => Some(KeyCode::Right),
                VK_PRIOR => Some(KeyCode::PageUp),
                VK_NEXT => Some(KeyCode::PageDown),
                VK_HOME => Some(KeyCode::Home),
                VK_END => Some(KeyCode::End),
                VK_RETURN => Some(KeyCode::Enter),
                VK_ESCAPE => Some(KeyCode::Esc),
                VK_TAB => Some(KeyCode::Tab),
                VK_BACK => Some(KeyCode::Backspace),
                VK_DELETE => Some(KeyCode::Delete),
                VK_F1..=VK_F24 => Some(KeyCode::F((vk - VK_F1 + 1) as u8)),
                _ => {
                    if ctrl && (0x41..=0x5A).contains(&vk) {
                        // Ctrl+letter arrives as a control character; report the letter.
                        Some(KeyCode::Char((vk as u8 + 0x20) as char))
                    } else if unit == 0 {
                        None
                    } else if (0xD800..0xDC00).contains(&unit) {
                        *pending_surrogate = Some(unit);
                        None
                    } else if (0xDC00..0xE000).contains(&unit) {
                        let hi = pending_surrogate.take();
                        hi.and_then(|hi| char::decode_utf16([hi, unit]).next()?.ok())
                            .map(KeyCode::Char)
                    } else if unit < 0x20 {
                        // Other control characters: Ctrl+<something> we don't name.
                        None
                    } else {
                        char::from_u32(unit as u32).map(KeyCode::Char)
                    }
                }
            };
            if let Some(code) = code {
                let ev = Event::Key(KeyEvent {
                    code,
                    ctrl,
                    shift,
                    alt,
                });
                let repeat = (k.wRepeatCount.max(1) as usize).min(32);
                out.extend(std::iter::repeat_n(ev, repeat));
            }
        }
        MOUSE_EVENT => {
            // SAFETY: EventType says this is a mouse record.
            let m = unsafe { rec.Event.MouseEvent };
            let x = m.dwMousePosition.X.max(0) as u16;
            let y = m.dwMousePosition.Y.max(0) as u16;
            let ctrl = m.dwControlKeyState & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0;
            let shift = m.dwControlKeyState & SHIFT_PRESSED != 0;
            let mk = |kind| {
                Event::Mouse(MouseEvent {
                    kind,
                    x,
                    y,
                    ctrl,
                    shift,
                })
            };
            let flags = m.dwEventFlags;
            if flags & MOUSE_WHEELED != 0 {
                let delta = (m.dwButtonState >> 16) as u16 as i16 as i32;
                out.push(mk(MouseKind::Wheel(delta / 120)));
            } else if flags & MOUSE_HWHEELED != 0 {
                let delta = (m.dwButtonState >> 16) as u16 as i16 as i32;
                out.push(mk(MouseKind::HWheel(delta / 120)));
            } else if flags & MOUSE_MOVED != 0 {
                let held = m.dwButtonState & 0xFFFF;
                if let Some(b) = primary_button(held) {
                    out.push(mk(MouseKind::Drag(b)));
                } else {
                    out.push(mk(MouseKind::Move));
                }
            } else {
                let now = m.dwButtonState & 0xFFFF;
                let pressed = now & !*buttons;
                let released = *buttons & !now;
                *buttons = now;
                if flags & DOUBLE_CLICK != 0 {
                    if let Some(b) = primary_button(now) {
                        out.push(mk(MouseKind::DoubleClick(b)));
                    }
                } else {
                    for (bit, b) in BUTTON_BITS {
                        if pressed & bit != 0 {
                            out.push(mk(MouseKind::Down(b)));
                        }
                    }
                    for (bit, b) in BUTTON_BITS {
                        if released & bit != 0 {
                            out.push(mk(MouseKind::Up(b)));
                        }
                    }
                }
            }
        }
        WINDOW_BUFFER_SIZE_EVENT => out.push(Event::Resize),
        _ => {}
    }
    out
}

const BUTTON_BITS: [(u32, Button); 3] = [
    (FROM_LEFT_1ST_BUTTON_PRESSED, Button::Left),
    (RIGHTMOST_BUTTON_PRESSED, Button::Right),
    (FROM_LEFT_2ND_BUTTON_PRESSED, Button::Middle),
];

fn primary_button(state: u32) -> Option<Button> {
    BUTTON_BITS
        .iter()
        .find(|(bit, _)| state & bit != 0)
        .map(|(_, b)| *b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Console::{
        COORD, INPUT_RECORD_0, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0, MOUSE_EVENT_RECORD,
    };

    fn key(vk: u16, unit: u16, state: u32, down: bool) -> INPUT_RECORD {
        INPUT_RECORD {
            EventType: KEY_EVENT as u16,
            Event: INPUT_RECORD_0 {
                KeyEvent: KEY_EVENT_RECORD {
                    bKeyDown: down as i32,
                    wRepeatCount: 1,
                    wVirtualKeyCode: vk,
                    wVirtualScanCode: 0,
                    uChar: KEY_EVENT_RECORD_0 { UnicodeChar: unit },
                    dwControlKeyState: state,
                },
            },
        }
    }

    fn mouse(x: i16, y: i16, buttons: u32, flags: u32) -> INPUT_RECORD {
        INPUT_RECORD {
            EventType: MOUSE_EVENT as u16,
            Event: INPUT_RECORD_0 {
                MouseEvent: MOUSE_EVENT_RECORD {
                    dwMousePosition: COORD { X: x, Y: y },
                    dwButtonState: buttons,
                    dwControlKeyState: 0,
                    dwEventFlags: flags,
                },
            },
        }
    }

    fn run(records: &[INPUT_RECORD]) -> Vec<Event> {
        let mut buttons = 0;
        let mut surrogate = None;
        records
            .iter()
            .flat_map(|r| translate(r, &mut buttons, &mut surrogate))
            .collect()
    }

    #[test]
    fn keys_and_modifiers() {
        let ev = run(&[
            key(0x51, b'q' as u16, 0, true),
            key(0x51, b'q' as u16, 0, false), // key-up ignored
            key(VK_UP, 0, 0, true),
            key(0x43, 0x03, LEFT_CTRL_PRESSED, true), // Ctrl+C arrives as 0x03
            key(VK_F1 + 4, 0, SHIFT_PRESSED, true),
        ]);
        assert_eq!(ev.len(), 4);
        assert_eq!(
            ev[0],
            Event::Key(KeyEvent {
                code: KeyCode::Char('q'),
                ctrl: false,
                shift: false,
                alt: false
            })
        );
        assert!(matches!(ev[1], Event::Key(KeyEvent { code: KeyCode::Up, .. })));
        assert!(matches!(
            ev[2],
            Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                ctrl: true,
                ..
            })
        ));
        assert!(matches!(
            ev[3],
            Event::Key(KeyEvent {
                code: KeyCode::F(5),
                shift: true,
                ..
            })
        ));
    }

    #[test]
    fn surrogate_pairs_become_one_char() {
        let ev = run(&[key(0, 0xD83D, 0, true), key(0, 0xDE00, 0, true)]);
        assert_eq!(ev.len(), 1);
        assert!(matches!(
            ev[0],
            Event::Key(KeyEvent {
                code: KeyCode::Char('😀'),
                ..
            })
        ));
    }

    #[test]
    fn mouse_click_drag_release_and_wheel() {
        let ev = run(&[
            mouse(5, 7, FROM_LEFT_1ST_BUTTON_PRESSED, 0),
            mouse(6, 7, FROM_LEFT_1ST_BUTTON_PRESSED, MOUSE_MOVED),
            mouse(6, 8, 0, 0),
            mouse(6, 8, 0, MOUSE_MOVED),
            mouse(1, 1, (120u32) << 16, MOUSE_WHEELED),
            mouse(1, 1, ((-240i16) as u16 as u32) << 16, MOUSE_WHEELED),
            mouse(2, 2, FROM_LEFT_1ST_BUTTON_PRESSED, DOUBLE_CLICK),
        ]);
        let kinds: Vec<MouseKind> = ev
            .iter()
            .map(|e| match e {
                Event::Mouse(m) => m.kind,
                _ => panic!("expected mouse"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                MouseKind::Down(Button::Left),
                MouseKind::Drag(Button::Left),
                MouseKind::Up(Button::Left),
                MouseKind::Move,
                MouseKind::Wheel(1),
                MouseKind::Wheel(-2),
                MouseKind::DoubleClick(Button::Left),
            ]
        );
        if let Event::Mouse(m) = ev[0] {
            assert_eq!((m.x, m.y), (5, 7));
        }
    }

    #[test]
    fn resize_record() {
        let rec = INPUT_RECORD {
            EventType: WINDOW_BUFFER_SIZE_EVENT as u16,
            Event: INPUT_RECORD_0::default(),
        };
        assert_eq!(run(&[rec]), vec![Event::Resize]);
    }
}
