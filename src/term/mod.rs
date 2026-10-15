//! Windows-Terminal-native terminal layer: console modes, input records, cell screen.

pub mod console;
pub mod input;
pub mod screen;

pub use console::Console;
pub use input::{Button, Event, KeyCode, KeyEvent, MouseEvent, MouseKind};
pub use screen::{Color, Rect, Screen, Style};
