//! Versioned protocol envelopes for Lynx.
//!
//! [`Envelope`] frames every message, [`protocol`] negotiates versions and
//! features at handshake, and [`replication`] classifies authoritative state
//! changes by delivery guarantee and audience.

pub mod envelope;
pub mod protocol;
pub mod replication;

pub use envelope::{
    Envelope, MAX_ID_LEN, MAX_JSON_DEPTH, MAX_KIND_LEN, MAX_MESSAGE_BYTES, MAX_STRING_BYTES,
    MessageId, MessageKind,
};
pub use protocol::{
    ErrorPayload, Feature, FeatureSet, Handshake, LocalizedNotice, Negotiated, PROTOCOL_MAJOR,
    PROTOCOL_MINOR, PROTOCOL_PATCH, PROTOCOL_VERSION, negotiate, protocol_version,
};
pub use replication::{
    Delivery, MAX_UPDATES_PER_BATCH, Recipient, ReplicationBatch, ReplicationCategory,
    ReplicationUpdate,
};
