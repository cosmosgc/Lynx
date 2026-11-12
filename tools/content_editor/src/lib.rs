//! Standalone editor state and project operations.
//!
//! The editor owns authoring history and recovery state. Content parsing and
//! validation are delegated to `engine_content`, the same module used by the
//! project CLI; the player client and simulation crates are not dependencies.

mod session;

pub use session::{EditorSession, SessionError};
