//! Friends and blocks for the in-memory adapter.

use game_api::{AccountId, ApiError, Friend, FriendState, SessionToken, SocialService};

use crate::{MemoryBackend, State};

fn is_blocked(state: &State, first: &AccountId, second: &AccountId) -> bool {
    state
        .blocked
        .get(first)
        .is_some_and(|accounts| accounts.contains(second))
        || state
            .blocked
            .get(second)
            .is_some_and(|accounts| accounts.contains(first))
}

fn friend_from(state: &State, account_id: AccountId, status: FriendState) -> Friend {
    let account = state.accounts.get(&account_id);
    Friend {
        account_id,
        username: account.and_then(|account| account.username.clone()),
        display_name: account.and_then(|account| account.display_name.clone()),
        state: status,
    }
}

impl SocialService for MemoryBackend {
    fn friends(&self, token: &SessionToken) -> Result<Vec<Friend>, ApiError> {
        let state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        let mut friends = Vec::new();

        if let Some(relations) = state.friends.get(&account_id) {
            for other in relations.keys() {
                if is_blocked(&state, &account_id, other) {
                    continue;
                }
                friends.push(friend_from(&state, other.clone(), FriendState::Friend));
            }
        }
        if let Some(blocked) = state.blocked.get(&account_id) {
            for other in blocked {
                friends.push(friend_from(&state, other.clone(), FriendState::Blocked));
            }
        }

        friends.sort_by(|left, right| left.account_id.cmp(&right.account_id));
        Ok(friends)
    }

    fn add_friend(&self, token: &SessionToken, friend: &AccountId) -> Result<(), ApiError> {
        let mut state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        if account_id == *friend {
            return Err(ApiError::Conflict(
                "an account cannot befriend itself".into(),
            ));
        }
        if !state.accounts.contains_key(friend) {
            return Err(ApiError::NotFound("account not found".into()));
        }
        if is_blocked(&state, &account_id, friend) {
            return Err(ApiError::Conflict(
                "a block prevents this friendship".into(),
            ));
        }

        state
            .friends
            .entry(account_id.clone())
            .or_default()
            .insert(friend.clone(), FriendState::Friend);
        state
            .friends
            .entry(friend.clone())
            .or_default()
            .insert(account_id, FriendState::Friend);
        Ok(())
    }

    fn remove_friend(&self, token: &SessionToken, friend: &AccountId) -> Result<(), ApiError> {
        let mut state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        if let Some(relations) = state.friends.get_mut(&account_id) {
            relations.remove(friend);
        }
        if let Some(relations) = state.friends.get_mut(friend) {
            relations.remove(&account_id);
        }
        Ok(())
    }

    fn block_account(&self, token: &SessionToken, account: &AccountId) -> Result<(), ApiError> {
        let mut state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        if account_id == *account {
            return Err(ApiError::Conflict("an account cannot block itself".into()));
        }
        if !state.accounts.contains_key(account) {
            return Err(ApiError::NotFound("account not found".into()));
        }

        if let Some(relations) = state.friends.get_mut(&account_id) {
            relations.remove(account);
        }
        if let Some(relations) = state.friends.get_mut(account) {
            relations.remove(&account_id);
        }
        state
            .blocked
            .entry(account_id)
            .or_default()
            .insert(account.clone());
        Ok(())
    }

    fn unblock_account(&self, token: &SessionToken, account: &AccountId) -> Result<(), ApiError> {
        let mut state = self.lock_state()?;
        let account_id = Self::account_for_token(&state, token)?;
        if let Some(blocked) = state.blocked.get_mut(&account_id) {
            blocked.remove(account);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use game_api::AuthService;

    use super::*;
    use crate::test_support::login;

    fn account(backend: &MemoryBackend, device: &str) -> game_api::Session {
        backend.authenticate_device(&login(device, true)).unwrap()
    }

    #[test]
    fn friendships_are_mutual_and_removable() {
        let backend = MemoryBackend::new();
        let first = account(&backend, "device-1");
        let second = account(&backend, "device-2");
        backend
            .add_friend(&first.token, &second.account_id)
            .unwrap();
        assert_eq!(backend.friends(&first.token).unwrap().len(), 1);
        assert_eq!(backend.friends(&second.token).unwrap().len(), 1);
        assert_eq!(
            backend.friends(&first.token).unwrap()[0].state,
            FriendState::Friend
        );

        backend
            .remove_friend(&first.token, &second.account_id)
            .unwrap();
        assert_eq!(backend.friends(&first.token).unwrap(), Vec::new());
        assert_eq!(backend.friends(&second.token).unwrap(), Vec::new());
    }

    #[test]
    fn blocking_hides_the_account_and_prevents_friendship() {
        let backend = MemoryBackend::new();
        let first = account(&backend, "device-1");
        let second = account(&backend, "device-2");
        backend
            .add_friend(&first.token, &second.account_id)
            .unwrap();
        backend
            .block_account(&first.token, &second.account_id)
            .unwrap();

        let first_view = backend.friends(&first.token).unwrap();
        assert_eq!(first_view.len(), 1);
        assert_eq!(first_view[0].state, FriendState::Blocked);
        assert_eq!(backend.friends(&second.token).unwrap(), Vec::new());
        assert!(matches!(
            backend.add_friend(&first.token, &second.account_id),
            Err(ApiError::Conflict(_))
        ));
        assert!(matches!(
            backend.add_friend(&second.token, &first.account_id),
            Err(ApiError::Conflict(_))
        ));

        backend
            .unblock_account(&first.token, &second.account_id)
            .unwrap();
        assert_eq!(backend.friends(&first.token).unwrap(), Vec::new());
        assert!(backend.add_friend(&first.token, &second.account_id).is_ok());
    }

    #[test]
    fn unknown_and_self_accounts_are_rejected() {
        let backend = MemoryBackend::new();
        let first = account(&backend, "device-1");
        assert!(matches!(
            backend.add_friend(&first.token, &first.account_id),
            Err(ApiError::Conflict(_))
        ));
        let missing = AccountId::new("missing-account").unwrap();
        assert!(matches!(
            backend.add_friend(&first.token, &missing),
            Err(ApiError::NotFound(_))
        ));
        assert!(matches!(
            backend.block_account(&first.token, &missing),
            Err(ApiError::NotFound(_))
        ));
    }
}
