//! Native Nakama adapter. This is the only crate that knows Nakama's HTTP API.

pub mod character_store;
pub mod client;
pub mod identity_store;

pub use character_store::PostgresCharacterStore;
pub use client::{NakamaClient, NakamaConfig};
pub use identity_store::PostgresIdentityStore;
