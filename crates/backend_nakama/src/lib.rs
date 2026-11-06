//! Nakama REST and realtime adapters. This is the only crate that knows Nakama's
//! HTTP and socket protocols.

pub mod backend;
pub mod character_store;
pub mod client;
pub mod faction_calendar_store;
pub mod identity_store;
pub mod inventory_store;
pub mod postgres_connection;
pub mod postgres_pool;
pub mod realtime;
pub mod telemetry;

pub use backend::{BackendTelemetrySnapshot, NakamaPostgresBackend};
pub use character_store::PostgresCharacterStore;
pub use client::{NakamaClient, NakamaConfig};
pub use faction_calendar_store::PostgresFactionCalendarStore;
pub use identity_store::PostgresIdentityStore;
pub use inventory_store::PostgresInventoryStore;
pub use telemetry::{DURATION_BUCKETS_SECONDS, Telemetry, TelemetrySnapshot};
