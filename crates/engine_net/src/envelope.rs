//! Versioned message envelopes.
//!
//! Every protocol message travels inside an [`Envelope`]. The envelope carries
//! the protocol version, a sender-generated ID for deduplication and replies, a
//! namespaced kind, an optional correlation ID, and a JSON body. Arbitrary JSON
//! is bounded so that a malicious or buggy peer cannot exhaust the process.

use std::fmt;
use std::str::FromStr;

use engine_core::error::{EngineError, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::protocol::{PROTOCOL_MAJOR, protocol_version};

/// Maximum encoded envelope size in bytes.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Maximum container nesting depth of an envelope body.
pub const MAX_JSON_DEPTH: usize = 16;

/// Maximum length of any JSON string or object key, in bytes.
pub const MAX_STRING_BYTES: usize = 8 * 1024;

/// Maximum length of a message or correlation ID.
pub const MAX_ID_LEN: usize = 64;

/// Maximum length of a message kind.
pub const MAX_KIND_LEN: usize = 64;

/// A validated sender-generated message ID, for deduplication and replies.
///
/// IDs begin with an ASCII letter and contain only letters, digits, `_`, and
/// `-`. Examples: `req_01J8Z3K4Q7Y2N5V6B8C0D1E2F3`, `evt-42`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MessageId(String);

impl MessageId {
    /// Validate and create a message ID.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when the ID is empty, too long,
    /// uses unsupported characters, or does not start with a letter.
    pub fn parse(input: &str) -> Result<Self, EngineError> {
        let valid = !input.is_empty()
            && input.len() <= MAX_ID_LEN
            && input
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic())
            && input
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            && !input.ends_with('_')
            && !input.ends_with('-');
        if !valid {
            return Err(
                EngineError::new(ErrorCode::InvalidRequest, "invalid message id")
                    .with_detail("id", summarized(input)),
            );
        }
        Ok(Self(input.to_string()))
    }

    /// The ID text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MessageId {
    type Err = EngineError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for MessageId {
    type Error = EngineError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<MessageId> for String {
    fn from(value: MessageId) -> Self {
        value.0
    }
}

/// A validated, namespaced message kind such as `action.request`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MessageKind(String);

impl MessageKind {
    /// Validate and create a message kind.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when the kind is empty, too long,
    /// uses uppercase or unsupported characters, lacks a namespace dot, or has
    /// empty segments.
    pub fn parse(input: &str) -> Result<Self, EngineError> {
        let valid = !input.is_empty()
            && input.len() <= MAX_KIND_LEN
            && input.contains('.')
            && !input.starts_with('.')
            && !input.ends_with('.')
            && !input.contains("..")
            && input.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
            });
        if !valid {
            return Err(
                EngineError::new(ErrorCode::InvalidRequest, "invalid message kind")
                    .with_detail("kind", summarized(input)),
            );
        }
        Ok(Self(input.to_string()))
    }

    /// The kind text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The namespace before the first dot, for example `action`.
    #[must_use]
    pub fn namespace(&self) -> &str {
        self.0.split('.').next().unwrap_or(&self.0)
    }
}

impl fmt::Display for MessageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MessageKind {
    type Err = EngineError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for MessageKind {
    type Error = EngineError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<MessageKind> for String {
    fn from(value: MessageKind) -> Self {
        value.0
    }
}

/// The wire representation of one protocol message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Protocol version of the sender.
    #[serde(rename = "v")]
    pub version: semver::Version,
    /// Sender-generated message ID.
    pub id: MessageId,
    /// Namespaced message kind.
    pub kind: MessageKind,
    /// Message body. Defaults to an empty object when omitted.
    #[serde(default = "empty_body")]
    pub body: Value,
    /// Groups a reply or effect with the request that caused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation: Option<MessageId>,
}

impl Envelope {
    /// Create an envelope with the current protocol version.
    #[must_use]
    pub fn new(id: MessageId, kind: MessageKind, body: Value) -> Self {
        Self {
            version: protocol_version(),
            id,
            kind,
            body,
            correlation: None,
        }
    }

    /// Attach a correlation ID.
    #[must_use]
    pub fn with_correlation(mut self, correlation: MessageId) -> Self {
        self.correlation = Some(correlation);
        self
    }

    /// Create a reply to this envelope, correlating it with the request.
    #[must_use]
    pub fn reply(&self, id: MessageId, kind: MessageKind, body: Value) -> Self {
        Self::new(id, kind, body).with_correlation(self.id.clone())
    }

    /// Check that the envelope targets this protocol major version and that its
    /// body stays within the configured bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::VersionMismatch`] for an incompatible protocol and
    /// [`ErrorCode::CapacityExceeded`] when a body limit is exceeded.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.version.major != u64::from(PROTOCOL_MAJOR) {
            return Err(EngineError::new(
                ErrorCode::VersionMismatch,
                "envelope protocol major version is not supported",
            )
            .with_detail("received", self.version.to_string())
            .with_detail("supported_major", PROTOCOL_MAJOR.to_string()));
        }
        check_json_limits(&self.body, "body")
    }

    /// Validate and serialize the envelope as compact JSON.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Envelope::validate`] and
    /// [`ErrorCode::CapacityExceeded`] when the encoded form is too large.
    pub fn encode(&self) -> Result<String, EngineError> {
        self.validate()?;
        let text = serde_json::to_string(self).map_err(|error| {
            EngineError::new(ErrorCode::Internal, "failed to encode envelope")
                .with_detail("error", error.to_string())
        })?;
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(too_large(text.len()));
        }
        Ok(text)
    }

    /// Parse and validate an envelope from JSON.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::CapacityExceeded`] when the input is too large and
    /// [`ErrorCode::InvalidRequest`] when the JSON is malformed, does not match
    /// the envelope schema, or fails validation.
    pub fn decode(text: &str) -> Result<Self, EngineError> {
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(too_large(text.len()));
        }
        let envelope: Self = serde_json::from_str(text).map_err(|error| {
            EngineError::new(ErrorCode::InvalidRequest, "invalid envelope")
                .with_detail("error", error.to_string())
        })?;
        envelope.validate()?;
        Ok(envelope)
    }
}

fn empty_body() -> Value {
    Value::Object(Map::new())
}

fn summarized(input: &str) -> String {
    input.chars().take(80).collect()
}

fn too_large(size: usize) -> EngineError {
    EngineError::new(
        ErrorCode::CapacityExceeded,
        "message exceeds the size limit",
    )
    .with_detail("bytes", size.to_string())
    .with_detail("max_bytes", MAX_MESSAGE_BYTES.to_string())
}

/// Check the depth and string-length limits of a JSON value.
pub(crate) fn check_json_limits(value: &Value, path: &str) -> Result<(), EngineError> {
    let depth = value_depth(value);
    if depth > MAX_JSON_DEPTH {
        return Err(EngineError::new(
            ErrorCode::CapacityExceeded,
            "message body is nested too deeply",
        )
        .with_detail("path", path)
        .with_detail("depth", depth.to_string())
        .with_detail("max_depth", MAX_JSON_DEPTH.to_string()));
    }
    check_strings(value, path)
}

fn check_strings(value: &Value, path: &str) -> Result<(), EngineError> {
    match value {
        Value::String(text) if text.len() > MAX_STRING_BYTES => {
            Err(string_too_long(path, text.len()))
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                check_strings(item, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for (key, item) in map {
                if key.len() > MAX_STRING_BYTES {
                    return Err(string_too_long(&format!("{path}.<key>"), key.len()));
                }
                check_strings(item, &format!("{path}.{key}"))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn string_too_long(path: &str, size: usize) -> EngineError {
    EngineError::new(
        ErrorCode::CapacityExceeded,
        "message string exceeds the length limit",
    )
    .with_detail("path", path)
    .with_detail("bytes", size.to_string())
    .with_detail("max_bytes", MAX_STRING_BYTES.to_string())
}

fn value_depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(value_depth).max().unwrap_or(0),
        Value::Object(map) => 1 + map.values().map(value_depth).max().unwrap_or(0),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id(value: &str) -> MessageId {
        MessageId::parse(value).unwrap()
    }

    fn kind(value: &str) -> MessageKind {
        MessageKind::parse(value).unwrap()
    }

    #[test]
    fn envelopes_round_trip_through_json() {
        let envelope = Envelope::new(
            id("req_01J8Z3K4Q7Y2N5V6B8C0D1E2F3"),
            kind("action.request"),
            json!({ "verb": "inspect", "target": "ent_01J8Z3K4Q7Y2N5V6B8C0D1E2F3" }),
        );
        let text = envelope.encode().unwrap();
        assert!(text.contains("\"v\":\"1.0.0\""));
        assert!(text.contains("\"kind\":\"action.request\""));

        let decoded = Envelope::decode(&text).unwrap();
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn replies_correlate_with_the_request() {
        let request = Envelope::new(id("req_1"), kind("action.request"), json!({}));
        let reply = request.reply(id("res_1"), kind("action.result"), json!({ "ok": true }));
        assert_eq!(
            reply.correlation.as_ref().map(MessageId::as_str),
            Some("req_1")
        );
        assert_ne!(reply.id, request.id);
    }

    #[test]
    fn protocol_major_mismatches_are_rejected() {
        let mut envelope = Envelope::new(id("req_1"), kind("action.request"), json!({}));
        envelope.version = semver::Version::new(2, 0, 0);
        assert_eq!(
            envelope.encode().unwrap_err().code(),
            ErrorCode::VersionMismatch
        );
    }

    #[test]
    fn oversized_strings_are_rejected() {
        let envelope = Envelope::new(
            id("req_1"),
            kind("chat.send"),
            Value::String("x".repeat(MAX_STRING_BYTES + 1)),
        );
        let error = envelope.encode().unwrap_err();
        assert_eq!(error.code(), ErrorCode::CapacityExceeded);
        assert_eq!(error.details()["path"], "body");
    }

    #[test]
    fn oversized_messages_are_rejected_before_parsing() {
        let text = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert_eq!(
            Envelope::decode(&text).unwrap_err().code(),
            ErrorCode::CapacityExceeded
        );
    }

    #[test]
    fn deep_bodies_are_rejected() {
        let mut body = json!(0);
        for _ in 0..=MAX_JSON_DEPTH {
            body = Value::Array(vec![body]);
        }
        let envelope = Envelope::new(id("req_1"), kind("action.request"), body);
        assert_eq!(
            envelope.encode().unwrap_err().code(),
            ErrorCode::CapacityExceeded
        );
    }

    #[test]
    fn malformed_json_is_rejected() {
        assert_eq!(
            Envelope::decode("not json").unwrap_err().code(),
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            Envelope::decode(r#"{"v":"1.0.0","id":"req_1","kind":"Action.Request"}"#)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn omitted_body_defaults_to_an_empty_object() {
        let envelope =
            Envelope::decode(r#"{"v":"1.0.0","id":"req_1","kind":"action.request","body":null}"#)
                .unwrap();
        assert_eq!(envelope.body, Value::Null);

        let envelope =
            Envelope::decode(r#"{"v":"1.0.0","id":"req_1","kind":"action.request"}"#).unwrap();
        assert_eq!(envelope.body, json!({}));
    }

    #[test]
    fn ids_and_kinds_are_validated() {
        assert!(MessageId::parse("req_01J8Z3K4Q7Y2N5V6B8C0D1E2F3").is_ok());
        assert!(MessageId::parse("evt-42").is_ok());
        for input in ["", "1req", "_req", "req_", "req id", "req.id"] {
            assert!(
                MessageId::parse(input).is_err(),
                "{input} should be rejected as an id"
            );
        }

        assert!(MessageKind::parse("action.request").is_ok());
        assert_eq!(kind("action.request").namespace(), "action");
        for input in [
            "",
            "action",
            "Action.Request",
            ".action",
            "action.",
            "action..request",
        ] {
            assert!(
                MessageKind::parse(input).is_err(),
                "{input} should be rejected as a kind"
            );
        }
    }
}
