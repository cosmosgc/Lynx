//! Runnable Lynx world service.
//!
//! The crate splits the service into five pieces:
//!
//! - [`auth`] maps hashed bearer tokens to trusted server-side identities;
//! - [`backend`] commits calendar and faction snapshots for the demo;
//! - [`demo`] builds the in-memory zone, calendar, and server handshake;
//! - [`persistence`] writes and loads durable whole-world snapshot files;
//! - [`service`] serializes admission, input, and ticks on one authority loop;
//! - [`transport`] exposes health endpoints and moves length-prefixed
//!   envelopes over WebSocket connections.
//!
//! `world_gateway` still owns authority, sessions, interest, and reconnect;
//! this crate adds only transport and process concerns. There is no database:
//! durable state is the snapshot file, and session state still resets on
//! restart.

pub mod auth;
pub mod backend;
pub mod config;
pub mod demo;
pub mod persistence;
pub mod service;
pub mod transport;
