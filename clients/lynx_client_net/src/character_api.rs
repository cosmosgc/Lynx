//! Authenticated HTTP client for the Lynx character-management endpoints.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use game_api::{
    Character, CharacterCreationReceipt, ChatChannelTargetPage, CreateCharacter, CreateGroup,
    Friend, GroupId, GroupMember, GroupMembership, GroupPage, GroupSearch, SessionToken,
    SocialGroup, validate_group_page,
};
use serde::Deserialize;
use ureq::http::{HeaderValue, Response};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Coarse, redacted failure for character HTTP requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharacterApiFailure {
    InvalidConfiguration,
    InvalidRequest,
    Unauthenticated,
    NotFound,
    Conflict,
    RateLimited,
    Unavailable,
    Transport,
    Protocol,
}

/// Blocking HTTP client for the authenticated Lynx world-service APIs.
///
/// Use from a dedicated I/O worker, not from Bevy's frame schedule.
pub struct CharacterApiClient {
    agent: ureq::Agent,
    base_url: String,
}

impl fmt::Debug for CharacterApiClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CharacterApiClient")
            .field("base_url", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl CharacterApiClient {
    /// Validate an HTTP(S) origin (no path/query/credentials) and create client.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, CharacterApiFailure> {
        let base_url = base_url.as_ref().trim_end_matches('/');
        let uri: ureq::http::Uri = base_url
            .parse()
            .map_err(|_| CharacterApiFailure::InvalidConfiguration)?;
        let host = uri
            .host()
            .filter(|host| !host.is_empty())
            .ok_or(CharacterApiFailure::InvalidConfiguration)?;
        let loopback = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
        if !matches!(uri.scheme_str(), Some("http" | "https"))
            || uri
                .authority()
                .is_none_or(|authority| authority.as_str().contains('@'))
            || uri.query().is_some()
            || uri.path() != "/"
            || (uri.scheme_str() == Some("http") && !loopback)
        {
            return Err(CharacterApiFailure::InvalidConfiguration);
        }
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .build();
        Ok(Self {
            agent: ureq::Agent::new_with_config(config),
            base_url: base_url.to_owned(),
        })
    }

    /// List characters belonging to the authenticated Nakama account.
    pub fn characters(&self, token: &SessionToken) -> Result<Vec<Character>, CharacterApiFailure> {
        let response = self
            .agent
            .get(self.endpoint("/characters"))
            .header("Authorization", bearer(token)?)
            .call()
            .map_err(|_| CharacterApiFailure::Transport)?;
        let body = checked_body(response)?;
        let response: CharacterList =
            serde_json::from_str(&body).map_err(|_| CharacterApiFailure::Protocol)?;
        Ok(response.characters)
    }

    /// Create a character with a caller-supplied retry-stable idempotency key.
    pub fn create_character(
        &self,
        token: &SessionToken,
        idempotency_key: &str,
        request: &CreateCharacter,
    ) -> Result<CharacterCreationReceipt, CharacterApiFailure> {
        request
            .validate()
            .map_err(|_| CharacterApiFailure::InvalidRequest)?;
        if game_api::CharacterCreationKey::new(idempotency_key).is_err() {
            return Err(CharacterApiFailure::InvalidRequest);
        }
        let response = self
            .agent
            .post(self.endpoint("/characters"))
            .header("Authorization", bearer(token)?)
            .header("Idempotency-Key", idempotency_key)
            .header("Content-Type", "application/json")
            .send(serde_json::to_vec(request).map_err(|_| CharacterApiFailure::Protocol)?)
            .map_err(|_| CharacterApiFailure::Transport)?;
        serde_json::from_str(&checked_body(response)?).map_err(|_| CharacterApiFailure::Protocol)
    }

    /// Verify ownership and playability before presenting a character for world
    /// admission. This endpoint does not spawn an actor in the active world.
    pub fn select_character(
        &self,
        token: &SessionToken,
        character_id: &str,
    ) -> Result<Character, CharacterApiFailure> {
        if character_id.parse::<game_api::CharacterId>().is_err() {
            return Err(CharacterApiFailure::InvalidRequest);
        }
        let response = self
            .agent
            .post(self.endpoint(&format!("/characters/{character_id}/select")))
            .header("Authorization", bearer(token)?)
            .header("Content-Type", "application/json")
            .send(b"{}")
            .map_err(|_| CharacterApiFailure::Transport)?;
        serde_json::from_str(&checked_body(response)?).map_err(|_| CharacterApiFailure::Protocol)
    }

    /// List the authenticated account's friends, requests, and blocks.
    pub fn friends(&self, token: &SessionToken) -> Result<Vec<Friend>, CharacterApiFailure> {
        let response = self
            .agent
            .get(self.endpoint("/social/friends"))
            .header("Authorization", bearer(token)?)
            .call()
            .map_err(|_| CharacterApiFailure::Transport)?;
        let body = checked_body(response)?;
        let response: FriendList =
            serde_json::from_str(&body).map_err(|_| CharacterApiFailure::Protocol)?;
        Ok(response.friends)
    }

    /// Send a friend request to an account ID.
    pub fn request_friend(
        &self,
        token: &SessionToken,
        friend: &str,
    ) -> Result<(), CharacterApiFailure> {
        self.social_action(token, "/social/friends/request", friend)
    }

    /// Accept an incoming friend request.
    pub fn accept_friend_request(
        &self,
        token: &SessionToken,
        friend: &str,
    ) -> Result<(), CharacterApiFailure> {
        self.social_action(token, "/social/friends/accept", friend)
    }

    /// Dismiss an incoming request or cancel an outgoing request.
    pub fn dismiss_friend_request(
        &self,
        token: &SessionToken,
        friend: &str,
    ) -> Result<(), CharacterApiFailure> {
        self.social_action(token, "/social/friends/dismiss", friend)
    }

    /// Remove a mutual friend.
    pub fn remove_friend(
        &self,
        token: &SessionToken,
        friend: &str,
    ) -> Result<(), CharacterApiFailure> {
        self.social_action(token, "/social/friends/remove", friend)
    }

    /// Block an account.
    pub fn block_account(
        &self,
        token: &SessionToken,
        account: &str,
    ) -> Result<(), CharacterApiFailure> {
        self.social_action(token, "/social/blocks/block", account)
    }

    /// Remove a block without restoring any former friendship.
    pub fn unblock_account(
        &self,
        token: &SessionToken,
        account: &str,
    ) -> Result<(), CharacterApiFailure> {
        self.social_action(token, "/social/blocks/unblock", account)
    }

    /// Discover only direct/group chat targets authorized for this session.
    pub fn discover_chat_channels(
        &self,
        token: &SessionToken,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<ChatChannelTargetPage, CharacterApiFailure> {
        validate_group_page(limit, cursor).map_err(|_| CharacterApiFailure::InvalidRequest)?;
        let query = page_query(limit, cursor);
        let response = self
            .agent
            .get(self.endpoint(&format!("/social/chat/channels?{query}")))
            .header("Authorization", bearer(token)?)
            .call()
            .map_err(|_| CharacterApiFailure::Transport)?;
        serde_json::from_str(&checked_body(response)?).map_err(|_| CharacterApiFailure::Protocol)
    }

    /// List the authenticated account's group memberships and pending requests.
    pub fn my_groups(
        &self,
        token: &SessionToken,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<GroupPage<GroupMembership>, CharacterApiFailure> {
        validate_group_page(limit, cursor).map_err(|_| CharacterApiFailure::InvalidRequest)?;
        let query = page_query(limit, cursor);
        let response = self
            .agent
            .get(self.endpoint(&format!("/social/groups?{query}")))
            .header("Authorization", bearer(token)?)
            .call()
            .map_err(|_| CharacterApiFailure::Transport)?;
        serde_json::from_str(&checked_body(response)?).map_err(|_| CharacterApiFailure::Protocol)
    }

    /// Search public groups with validated filters and bounded pagination.
    pub fn discover_groups(
        &self,
        token: &SessionToken,
        search: &GroupSearch,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<GroupPage<SocialGroup>, CharacterApiFailure> {
        search
            .validate()
            .map_err(|_| CharacterApiFailure::InvalidRequest)?;
        validate_group_page(limit, cursor).map_err(|_| CharacterApiFailure::InvalidRequest)?;
        let mut query = page_query(limit, cursor);
        if let Some(name) = &search.name {
            query.push_str("&name=");
            query.push_str(&encode_query_component(name));
        }
        if let Some(open) = search.open {
            query.push_str(&format!("&open={open}"));
        }
        if let Some(members) = search.members {
            query.push_str(&format!("&members={members}"));
        }
        let response = self
            .agent
            .get(self.endpoint(&format!("/social/groups/discover?{query}")))
            .header("Authorization", bearer(token)?)
            .call()
            .map_err(|_| CharacterApiFailure::Transport)?;
        serde_json::from_str(&checked_body(response)?).map_err(|_| CharacterApiFailure::Protocol)
    }

    /// Create a persistent Nakama group.
    pub fn create_group(
        &self,
        token: &SessionToken,
        request: &CreateGroup,
    ) -> Result<SocialGroup, CharacterApiFailure> {
        request
            .validate()
            .map_err(|_| CharacterApiFailure::InvalidRequest)?;
        let body = serde_json::to_vec(request).map_err(|_| CharacterApiFailure::Protocol)?;
        let response = self
            .agent
            .post(self.endpoint("/social/groups"))
            .header("Authorization", bearer(token)?)
            .header("Content-Type", "application/json")
            .send(body)
            .map_err(|_| CharacterApiFailure::Transport)?;
        serde_json::from_str(&checked_body(response)?).map_err(|_| CharacterApiFailure::Protocol)
    }

    /// Join an open group or submit a join request for a closed group.
    pub fn request_group_join(
        &self,
        token: &SessionToken,
        group: &str,
    ) -> Result<(), CharacterApiFailure> {
        self.group_action(token, group, "join")
    }

    /// Leave a persistent group.
    pub fn leave_group(
        &self,
        token: &SessionToken,
        group: &str,
    ) -> Result<(), CharacterApiFailure> {
        self.group_action(token, group, "leave")
    }

    /// Read members and pending join requests for a group.
    pub fn group_members(
        &self,
        token: &SessionToken,
        group: &str,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<GroupPage<GroupMember>, CharacterApiFailure> {
        let group =
            GroupId::new(group.to_owned()).map_err(|_| CharacterApiFailure::InvalidRequest)?;
        validate_group_page(limit, cursor).map_err(|_| CharacterApiFailure::InvalidRequest)?;
        let query = page_query(limit, cursor);
        let response = self
            .agent
            .get(self.endpoint(&format!("/social/groups/{group}/members?{query}")))
            .header("Authorization", bearer(token)?)
            .call()
            .map_err(|_| CharacterApiFailure::Transport)?;
        serde_json::from_str(&checked_body(response)?).map_err(|_| CharacterApiFailure::Protocol)
    }

    /// Add accounts or approve their pending group join requests.
    pub fn add_group_members(
        &self,
        token: &SessionToken,
        group: &str,
        accounts: &[String],
    ) -> Result<(), CharacterApiFailure> {
        self.group_member_action(token, group, accounts, "add")
    }

    /// Remove members or decline their pending join requests.
    pub fn kick_group_members(
        &self,
        token: &SessionToken,
        group: &str,
        accounts: &[String],
    ) -> Result<(), CharacterApiFailure> {
        self.group_member_action(token, group, accounts, "kick")
    }

    /// Promote group members.
    pub fn promote_group_members(
        &self,
        token: &SessionToken,
        group: &str,
        accounts: &[String],
    ) -> Result<(), CharacterApiFailure> {
        self.group_member_action(token, group, accounts, "promote")
    }

    /// Demote group administrators.
    pub fn demote_group_members(
        &self,
        token: &SessionToken,
        group: &str,
        accounts: &[String],
    ) -> Result<(), CharacterApiFailure> {
        self.group_member_action(token, group, accounts, "demote")
    }

    fn group_action(
        &self,
        token: &SessionToken,
        group: &str,
        action: &str,
    ) -> Result<(), CharacterApiFailure> {
        let group =
            GroupId::new(group.to_owned()).map_err(|_| CharacterApiFailure::InvalidRequest)?;
        let response = self
            .agent
            .post(self.endpoint(&format!("/social/groups/{group}/{action}")))
            .header("Authorization", bearer(token)?)
            .send_empty()
            .map_err(|_| CharacterApiFailure::Transport)?;
        let body = checked_body(response)?;
        let response: SocialActionReceipt =
            serde_json::from_str(&body).map_err(|_| CharacterApiFailure::Protocol)?;
        if response.ok {
            Ok(())
        } else {
            Err(CharacterApiFailure::Protocol)
        }
    }

    fn group_member_action(
        &self,
        token: &SessionToken,
        group: &str,
        accounts: &[String],
        action: &str,
    ) -> Result<(), CharacterApiFailure> {
        let group =
            GroupId::new(group.to_owned()).map_err(|_| CharacterApiFailure::InvalidRequest)?;
        if accounts.is_empty()
            || accounts.len() > 100
            || accounts
                .iter()
                .any(|account| game_api::AccountId::new(account.clone()).is_err())
        {
            return Err(CharacterApiFailure::InvalidRequest);
        }
        let unique = accounts.iter().collect::<std::collections::BTreeSet<_>>();
        if unique.len() != accounts.len() {
            return Err(CharacterApiFailure::InvalidRequest);
        }
        let body = serde_json::to_vec(&serde_json::json!({"account_ids": accounts}))
            .map_err(|_| CharacterApiFailure::Protocol)?;
        let response = self
            .agent
            .post(self.endpoint(&format!("/social/groups/{group}/members/{action}")))
            .header("Authorization", bearer(token)?)
            .header("Content-Type", "application/json")
            .send(body)
            .map_err(|_| CharacterApiFailure::Transport)?;
        let body = checked_body(response)?;
        let response: SocialActionReceipt =
            serde_json::from_str(&body).map_err(|_| CharacterApiFailure::Protocol)?;
        if response.ok {
            Ok(())
        } else {
            Err(CharacterApiFailure::Protocol)
        }
    }

    fn social_action(
        &self,
        token: &SessionToken,
        path: &str,
        account_id: &str,
    ) -> Result<(), CharacterApiFailure> {
        game_api::AccountId::new(account_id.to_owned())
            .map_err(|_| CharacterApiFailure::InvalidRequest)?;
        let body = serde_json::to_vec(&serde_json::json!({"account_id": account_id}))
            .map_err(|_| CharacterApiFailure::Protocol)?;
        let response = self
            .agent
            .post(self.endpoint(path))
            .header("Authorization", bearer(token)?)
            .header("Content-Type", "application/json")
            .send(body)
            .map_err(|_| CharacterApiFailure::Transport)?;
        let body = checked_body(response)?;
        let response: SocialActionReceipt =
            serde_json::from_str(&body).map_err(|_| CharacterApiFailure::Protocol)?;
        if response.ok {
            Ok(())
        } else {
            Err(CharacterApiFailure::Protocol)
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

fn bearer(token: &SessionToken) -> Result<HeaderValue, CharacterApiFailure> {
    let mut value = HeaderValue::from_str(&format!("Bearer {}", token.expose_secret()))
        .map_err(|_| CharacterApiFailure::InvalidRequest)?;
    value.set_sensitive(true);
    Ok(value)
}

fn checked_body(mut response: Response<ureq::Body>) -> Result<String, CharacterApiFailure> {
    let status = response.status();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|_| CharacterApiFailure::Transport)?;
    if !status.is_success() {
        return Err(match status.as_u16() {
            400 | 422 => CharacterApiFailure::InvalidRequest,
            401 | 403 => CharacterApiFailure::Unauthenticated,
            404 => CharacterApiFailure::NotFound,
            409 => CharacterApiFailure::Conflict,
            429 => CharacterApiFailure::RateLimited,
            500 | 502..=504 => CharacterApiFailure::Unavailable,
            _ => CharacterApiFailure::Protocol,
        });
    }
    Ok(body)
}

fn page_query(limit: u16, cursor: Option<&str>) -> String {
    let mut query = format!("limit={limit}");
    if let Some(cursor) = cursor {
        query.push_str("&cursor=");
        query.push_str(&encode_query_component(cursor));
    }
    query
}

fn encode_query_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(encoded, "%{byte:02X}").expect("writing to a string cannot fail");
        }
    }
    encoded
}

#[derive(Deserialize)]
struct CharacterList {
    characters: Vec<Character>,
}

#[derive(Deserialize)]
struct FriendList {
    friends: Vec<Friend>,
}

#[derive(Deserialize)]
struct SocialActionReceipt {
    ok: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn selection_uses_sensitive_bearer_and_parses_owned_record() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let character_id =
            engine_core::StableId::from_parts(engine_core::IdKind::Character, 1, 123).to_string();
        let server_character_id = character_id.clone();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 1024];
            let request = loop {
                let length = stream.read(&mut chunk).unwrap();
                assert_ne!(length, 0, "client closed before complete request");
                bytes.extend_from_slice(&chunk[..length]);
                let Some(headers_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&bytes[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':').and_then(|(name, value)| {
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                    })
                    .expect("content length");
                if bytes.len() >= headers_end + 4 + content_length {
                    break String::from_utf8(bytes).unwrap();
                }
            };
            assert!(request.starts_with(&format!(
                "POST /characters/{server_character_id}/select HTTP/1.1"
            )));
            assert!(
                request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("authorization: Bearer access-secret"))
            );
            let body = json!({
                "id": server_character_id,
                "account_id": "account-1",
                "name": "Hero",
                "status": "active"
            });
            let body = serde_json::to_vec(&body).unwrap();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
            stream.write_all(&body).unwrap();
        });
        let client = CharacterApiClient::new(format!("http://{address}")).unwrap();
        let token = SessionToken::new("access-secret").unwrap();
        let character = client.select_character(&token, &character_id).unwrap();
        worker.join().unwrap();
        assert_eq!(character.name, "Hero");
        assert!(!format!("{client:?}").contains("access-secret"));
    }

    #[test]
    fn character_api_rejects_remote_plaintext_and_url_paths() {
        assert!(matches!(
            CharacterApiClient::new("http://example.test"),
            Err(CharacterApiFailure::InvalidConfiguration)
        ));
        assert!(matches!(
            CharacterApiClient::new("https://example.test/characters"),
            Err(CharacterApiFailure::InvalidConfiguration)
        ));
    }

    #[test]
    fn creation_retry_reuses_caller_intent_after_lost_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let id =
            engine_core::StableId::from_parts(engine_core::IdKind::Character, 1, 123).to_string();
        let expected_id = id.clone();
        let worker = std::thread::spawn(move || {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 1024];
                loop {
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let len = headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':').and_then(|(name, value)| {
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
                let request = String::from_utf8(bytes).unwrap();
                assert!(request.starts_with("POST /characters HTTP/1.1"));
                assert!(
                    request
                        .lines()
                        .any(|line| line.eq_ignore_ascii_case("idempotency-key: stable-intent"))
                );
                assert!(request.ends_with("{\"name\":\"Hero\"}"));
                if attempt == 1 {
                    let body = format!("{{\"character_id\":\"{id}\"}}");
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .unwrap();
                }
            }
        });
        let token = SessionToken::new("access-secret").unwrap();
        let request = CreateCharacter {
            name: "Hero".into(),
        };
        let first = CharacterApiClient::new(format!("http://{address}")).unwrap();
        assert!(
            first
                .create_character(&token, "stable-intent", &request)
                .is_err()
        );
        let restarted = CharacterApiClient::new(format!("http://{address}")).unwrap();
        let receipt = restarted
            .create_character(&token, "stable-intent", &request)
            .unwrap();
        assert_eq!(receipt.character_id.to_string(), expected_id);
        worker.join().unwrap();
    }
}
