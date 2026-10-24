//! Versioned protocol envelopes for Lynx.
//!
//! [`Envelope`] frames every message, [`protocol`] negotiates versions and
//! features at handshake, [`replication`] classifies authoritative state
//! changes by delivery guarantee and audience, and [`interest`] selects which
//! entities one session may receive.

pub mod envelope;
pub mod interest;
pub mod protocol;
pub mod replication;

pub use envelope::{
    Envelope, MAX_ID_LEN, MAX_JSON_DEPTH, MAX_KIND_LEN, MAX_MESSAGE_BYTES, MAX_STRING_BYTES,
    MessageId, MessageKind,
};
pub use interest::{
    DEFAULT_EXIT_MARGIN, DEFAULT_VIEW_RADIUS_M, InterestConfig, InterestEntity, InterestManager,
    InterestObserver, InterestSet, InterestUpdate, InterestWorld, MAX_INTEREST_ENTITIES,
    MAX_VIEW_RADIUS_M, ReplicationAudience, Visibility, filter_batch,
};
pub use protocol::{
    ErrorPayload, Feature, FeatureSet, Handshake, LocalizedNotice, Negotiated, PROTOCOL_MAJOR,
    PROTOCOL_MINOR, PROTOCOL_PATCH, PROTOCOL_VERSION, negotiate, protocol_version,
};
pub use replication::{
    Delivery, MAX_SESSION_ID_LEN, MAX_UPDATES_PER_BATCH, Recipient, ReplicationBatch,
    ReplicationCategory, ReplicationUpdate, SessionId,
};
