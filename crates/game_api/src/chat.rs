//! Chat channels and message history.
//!
//! Chat is a platform capability. Adapters that only expose HTTP history may
//! implement [`ChatService::messages`] without supporting live delivery; a
//! socket-capable adapter can implement the full contract.

use engine_core::time::Millis;
use serde::{Deserialize, Serialize};

use crate::service::{AccountId, ApiError, SessionToken};

/// Largest page returned by [`ChatService::messages`].
pub const MAX_MESSAGE_PAGE: u16 = 200;

/// Largest accepted chat message, in Unicode scalar values.
pub const MAX_MESSAGE_CHARS: usize = 2000;

/// The delivery model of a chat channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatChannelKind {
    /// A shared room that any admitted player may join.
    Room,
    /// A one-to-one conversation.
    Direct,
    /// A group-scoped channel.
    Group,
    /// A transient presence channel.
    Presence,
}

/// A chat channel visible to the authenticated account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatChannel {
    /// Adapter-issued channel identifier.
    pub id: String,
    /// Optional display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Delivery model.
    pub kind: ChatChannelKind,
    /// Number of members, when the adapter tracks it.
    #[serde(default)]
    pub member_count: u32,
}

/// One persisted chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Adapter-issued message identifier.
    pub id: String,
    /// Channel that contains the message.
    pub channel_id: String,
    /// Account that sent the message.
    pub sender_id: AccountId,
    /// Sender display name at the time of sending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    /// Message text.
    pub content: String,
    /// Creation time, when the adapter records one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Millis>,
}

/// A request to post a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendChatMessage {
    /// Target channel.
    pub channel_id: String,
    /// Message text.
    pub content: String,
}

impl SendChatMessage {
    /// Validate the channel and message before calling an adapter.
    pub fn validate(&self) -> Result<(), ApiError> {
        validate_channel_id(&self.channel_id)?;
        let content = self.content.trim();
        if content.is_empty() {
            return Err(ApiError::InvalidRequest(
                "chat message must not be empty".into(),
            ));
        }
        if content.chars().count() > MAX_MESSAGE_CHARS {
            return Err(ApiError::InvalidRequest(format!(
                "chat message must be at most {MAX_MESSAGE_CHARS} characters"
            )));
        }
        if content.chars().any(|ch| ch.is_control() && ch != '\n') {
            return Err(ApiError::InvalidRequest(
                "chat message contains an unsupported control character".into(),
            ));
        }
        Ok(())
    }
}

/// Validate an adapter-issued channel identifier.
pub fn validate_channel_id(value: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.len() > 128
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(ApiError::InvalidRequest(
            "invalid chat channel identifier".into(),
        ));
    }
    Ok(())
}

/// Validate a history page size.
pub fn validate_page(limit: u16) -> Result<(), ApiError> {
    if limit == 0 || limit > MAX_MESSAGE_PAGE {
        return Err(ApiError::InvalidRequest(format!(
            "chat history limit must be between 1 and {MAX_MESSAGE_PAGE}"
        )));
    }
    Ok(())
}

/// Channel discovery, history, and message posting.
pub trait ChatService {
    /// List channels the authenticated account may use.
    fn channels(&self, token: &SessionToken) -> Result<Vec<ChatChannel>, ApiError>;

    /// Read up to `limit` messages, oldest first.
    fn messages(
        &self,
        token: &SessionToken,
        channel_id: &str,
        limit: u16,
    ) -> Result<Vec<ChatMessage>, ApiError>;

    /// Post a message as the authenticated account.
    fn send_message(
        &self,
        token: &SessionToken,
        request: &SendChatMessage,
    ) -> Result<ChatMessage, ApiError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(content: &str) -> SendChatMessage {
        SendChatMessage {
            channel_id: "global".into(),
            content: content.into(),
        }
    }

    #[test]
    fn messages_are_trimmed_and_bounded() {
        assert!(request("hello").validate().is_ok());
        assert!(request("  ").validate().is_err());
        assert!(request("bad\u{0}byte").validate().is_err());
        assert!(
            request(&"x".repeat(MAX_MESSAGE_CHARS + 1))
                .validate()
                .is_err()
        );
        assert!(
            SendChatMessage {
                channel_id: "bad channel".into(),
                content: "hello".into(),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn history_pages_are_bounded() {
        assert!(validate_page(1).is_ok());
        assert!(validate_page(MAX_MESSAGE_PAGE).is_ok());
        assert!(validate_page(0).is_err());
        assert!(validate_page(MAX_MESSAGE_PAGE + 1).is_err());
    }
}
