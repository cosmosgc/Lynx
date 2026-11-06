//! Immediate-mode UI surfaces for the windowed client.
//!
//! Each panel owns its own state and is registered as a small Bevy plugin. The
//! shared [`window`] module supplies the floating-window chrome so new panels
//! get dragging, resizing, closing, and position memory without duplicating it.

#[cfg(feature = "window")]
pub mod chat;
#[cfg(feature = "window")]
pub mod window;
