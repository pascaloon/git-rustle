#![allow(dead_code)] // Legacy adapters remain as regression-test references.
//! Windows-Terminal-native terminal layer: console modes, input records, cell screen.

pub mod console;
pub mod input;
pub mod screen;

pub use input::{Button, KeyCode, KeyEvent, MouseEvent, MouseKind};
pub use screen::{Color, Rect, Screen, Style};
