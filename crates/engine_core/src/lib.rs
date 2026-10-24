//! Platform-neutral domain types for Lynx.
//!
//! This crate contains no I/O, no Bevy, no database driver, and no clock reads.
//! Everything here is deterministic given explicit inputs, which makes it
//! usable from the authoritative server, the client, tools, and tests alike.

pub mod audit;
pub mod error;
pub mod faction;
pub mod i18n;
pub mod id;
pub mod inventory;
pub mod math;
pub mod schema;
pub mod time;

pub use error::{EngineError, ErrorCode};
pub use i18n::{
    CatalogSet, DEFAULT_LOCALE, Locale, MessageCatalog, MessageKey, ResolvedMessage, placeholders,
};
pub use id::{DefinitionId, IdGenerator, IdKind, StableId};
