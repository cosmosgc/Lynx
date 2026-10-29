//! Authenticated HTTP client for the Lynx character-management endpoints.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use game_api::{Character, CharacterCreationReceipt, CreateCharacter, SessionToken};
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
        if idempotency_key.is_empty()
            || idempotency_key.len() > 128
            || idempotency_key.chars().any(char::is_control)
        {
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
        if character_id.is_empty()
            || character_id.len() > 128
            || character_id
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
        {
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

#[derive(Deserialize)]
struct CharacterList {
    characters: Vec<Character>,
}

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
}
