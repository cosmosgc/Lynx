//! Local chat room for the in-memory adapter.

use game_api::{
    ApiError, ChatMessage, ChatService, SendChatMessage, SessionToken, validate_channel_id,
    validate_page,
};

use crate::MemoryBackend;

const MAX_LOCAL_MESSAGES: usize = 500;

impl ChatService for MemoryBackend {
    fn channels(&self, token: &SessionToken) -> Result<Vec<game_api::ChatChannel>, ApiError> {
        let state = self.lock_state()?;
        Self::account_for_token(&state, token)?;
        let member_count = u32::try_from(state.accounts.len()).unwrap_or(u32::MAX);
        Ok(state
            .channels
            .values()
            .cloned()
            .map(|mut channel| {
                channel.member_count = member_count;
                channel
            })
            .collect())
    }

    fn messages(
        &self,
        token: &SessionToken,
        channel_id: &str,
        limit: u16,
    ) -> Result<Vec<ChatMessage>, ApiError> {
        validate_channel_id(channel_id)?;
        validate_page(limit)?;
        let state = self.lock_state()?;
        Self::account_for_token(&state, token)?;
        let history = state
            .messages
            .get(channel_id)
            .ok_or_else(|| ApiError::NotFound("chat channel not found".into()))?;
        let start = history.len().saturating_sub(usize::from(limit));
        Ok(history.iter().skip(start).cloned().collect())
    }

    fn send_message(
        &self,
        token: &SessionToken,
        request: &SendChatMessage,
    ) -> Result<ChatMessage, ApiError> {
        request.validate()?;
        let mut state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        if !state.channels.contains_key(&request.channel_id) {
            return Err(ApiError::NotFound("chat channel not found".into()));
        }

        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or_else(|| ApiError::Unavailable("in-memory ID space exhausted".into()))?;
        let sender_name = state.accounts.get(&account_id).and_then(|account| {
            account
                .username
                .clone()
                .or_else(|| account.display_name.clone())
        });
        let message = ChatMessage {
            id: format!("memory-message-{}", state.next_id),
            channel_id: request.channel_id.clone(),
            sender_id: account_id,
            sender_name,
            content: request.content.trim().to_owned(),
            created_at: None,
        };

        let history = state
            .messages
            .entry(request.channel_id.clone())
            .or_default();
        if history.len() >= MAX_LOCAL_MESSAGES {
            history.pop_front();
        }
        history.push_back(message.clone());
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use game_api::AuthService;

    use super::*;
    use crate::LOCAL_CHANNEL_ID;
    use crate::test_support::login;

    fn account(backend: &MemoryBackend, device: &str) -> game_api::Session {
        backend.authenticate_device(&login(device, true)).unwrap()
    }

    #[test]
    fn messages_are_ordered_and_attributed_to_the_caller() {
        let backend = MemoryBackend::new();
        let first = account(&backend, "device-1");
        let second = account(&backend, "device-2");

        let one = backend
            .send_message(
                &first.token,
                &SendChatMessage {
                    channel_id: LOCAL_CHANNEL_ID.into(),
                    content: "  hello  ".into(),
                },
            )
            .unwrap();
        let two = backend
            .send_message(
                &second.token,
                &SendChatMessage {
                    channel_id: LOCAL_CHANNEL_ID.into(),
                    content: "hi".into(),
                },
            )
            .unwrap();
        assert_eq!(one.content, "hello");
        assert_eq!(one.sender_id, first.account_id);
        assert_eq!(two.sender_id, second.account_id);

        let history = backend
            .messages(&first.token, LOCAL_CHANNEL_ID, 10)
            .unwrap();
        assert_eq!(history, vec![one, two]);
        assert_eq!(
            backend
                .messages(&first.token, LOCAL_CHANNEL_ID, 1)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn channels_report_local_membership() {
        let backend = MemoryBackend::new();
        let first = account(&backend, "device-1");
        let _second = account(&backend, "device-2");
        let channels = backend.channels(&first.token).unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].id, LOCAL_CHANNEL_ID);
        assert_eq!(channels[0].member_count, 2);
    }

    #[test]
    fn invalid_channels_and_pages_are_rejected() {
        let backend = MemoryBackend::new();
        let session = account(&backend, "device-1");
        assert!(matches!(
            backend.send_message(
                &session.token,
                &SendChatMessage {
                    channel_id: "missing".into(),
                    content: "hello".into(),
                }
            ),
            Err(ApiError::NotFound(_))
        ));
        assert!(matches!(
            backend.messages(&session.token, LOCAL_CHANNEL_ID, 0),
            Err(ApiError::InvalidRequest(_))
        ));
        assert!(matches!(
            backend.send_message(
                &session.token,
                &SendChatMessage {
                    channel_id: LOCAL_CHANNEL_ID.into(),
                    content: "   ".into(),
                }
            ),
            Err(ApiError::InvalidRequest(_))
        ));
    }
}
