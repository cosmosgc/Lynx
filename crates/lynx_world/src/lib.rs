//! Runnable Lynx world service.
//!
//! The crate splits the service into six pieces:
//!
//! - [`auth`] maps hashed bearer tokens to trusted server-side identities;
//! - [`authority`] holds and renews the single-writer zone lease;
//! - [`backend`] commits calendar and faction snapshots for the demo;
//! - [`demo`] builds the in-memory zone, calendar, and server handshake;
//! - [`persistence`] writes and loads durable whole-world snapshot files;
//! - [`startup`] chooses between a saved world and the authored demo;
//! - [`service`] serializes admission, input, and ticks on one authority loop;
//! - [`transport`] exposes health endpoints and moves length-prefixed
//!   envelopes over WebSocket connections.
//!
//! `world_gateway` still owns authority, sessions, interest, and reconnect;
//! this crate adds only transport and process concerns. Durable state is a
//! snapshot file or PostgreSQL; session state still resets on restart.

pub mod auth;
pub mod authority;
pub mod backend;
pub mod config;
pub mod demo;
pub mod persistence;
pub mod rate_limit;
pub mod schema;
pub mod service;
pub mod startup;
pub mod transport;
