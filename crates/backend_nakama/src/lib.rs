//! Native Nakama adapter. This is the only crate that knows Nakama's HTTP API.

pub mod backend;
pub mod character_store;
pub mod client;
pub mod identity_store;
pub mod inventory_store;
pub mod telemetry;

pub use backend::{BackendTelemetrySnapshot, NakamaPostgresBackend};
pub use character_store::PostgresCharacterStore;
pub use client::{NakamaClient, NakamaConfig};
pub use identity_store::PostgresIdentityStore;
pub use inventory_store::PostgresInventoryStore;
pub use telemetry::{DURATION_BUCKETS_SECONDS, Telemetry, TelemetrySnapshot};
