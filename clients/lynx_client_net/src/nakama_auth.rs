//! Narrow client-side Nakama session acquisition.
//!
//! This module obtains, refreshes, and revokes Nakama sessions. It deliberately
//! does not persist device identifiers or tokens, choose characters, or open a
//! world socket; those are application and UI responsibilities.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use game_api::DeviceLogin;
use serde::Deserialize;
use serde_json::json;
use ureq::http::Response;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Coarse, player-safe outcome from a Nakama client authentication request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NakamaAuthFailure {
    InvalidConfiguration,
    InvalidRequest,
    Authentication,
    RateLimited,
    Unavailable,
    Transport,
    Protocol,
}

/// Settings for Nakama's public HTTP(S) API.
pub struct NakamaAuthConfig {
    /// HTTP(S) base URL, without a path, query, or embedded credentials.
    pub base_url: String,
    /// Public Nakama server key used for client API Basic authentication.
    pub server_key: String,
    /// End-to-end request timeout.
    pub timeout: Duration,
}

impl NakamaAuthConfig {
    /// Create settings with the normal client request timeout.
    #[must_use]
    pub fn new(base_url: impl Into<String>, server_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            server_key: server_key.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl fmt::Debug for NakamaAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NakamaAuthConfig")
            .field("base_url", &"[redacted]")
            .field("server_key", &"[redacted]")
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Nakama access and refresh tokens. Neither token is rendered by `Debug`.
#[derive(PartialEq, Eq)]
pub struct NakamaSession {
    access_token: String,
    refresh_token: String,
    /// True when Nakama created a new account during device authentication.
    pub created: bool,
}

impl NakamaSession {
    /// Borrow the bearer token only when constructing an authenticated request.
    #[must_use]
    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    /// Borrow the refresh token only when requesting a replacement session.
    #[must_use]
    pub fn refresh_token(&self) -> &str {
        &self.refresh_token
    }
}

impl fmt::Debug for NakamaSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NakamaSession")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("created", &self.created)
            .finish()
    }
}

/// Blocking client for the public Nakama authentication endpoints.
///
/// Call it from a dedicated client I/O task, never from Bevy's frame schedule.
pub struct NakamaAuthClient {
    agent: ureq::Agent,
    base_url: String,
    authorization: String,
}

impl fmt::Debug for NakamaAuthClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NakamaAuthClient")
            .field("base_url", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl NakamaAuthClient {
    /// Validate configuration and construct a public-API client.
    pub fn new(config: NakamaAuthConfig) -> Result<Self, NakamaAuthFailure> {
        let NakamaAuthConfig {
            base_url,
            server_key,
            timeout,
        } = config;
        let base_url = validate_base_url(&base_url)?;
        if server_key.is_empty()
            || server_key.contains(':')
            || server_key.chars().any(char::is_control)
            || timeout.is_zero()
        {
            return Err(NakamaAuthFailure::InvalidConfiguration);
        }
        let agent_config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .build();
        Ok(Self {
            agent: ureq::Agent::new_with_config(agent_config),
            base_url,
            authorization: format!("Basic {}", BASE64.encode(format!("{server_key}:"))),
        })
    }

    /// Authenticate a device and return a Nakama access/refresh-token pair.
    pub fn authenticate_device(
        &self,
        request: &DeviceLogin,
    ) -> Result<NakamaSession, NakamaAuthFailure> {
        request
            .validate()
            .map_err(|_| NakamaAuthFailure::InvalidRequest)?;
        validate_nakama_device_id(&request.device_id)?;
        let mut request_builder = self
            .agent
            .post(self.endpoint("/v2/account/authenticate/device"))
            .query("create", request.create.to_string());
        if let Some(username) = &request.username {
            request_builder = request_builder.query("username", username);
        }
        let response = request_builder
            .header("Authorization", &self.authorization)
            .header("Content-Type", "application/json")
            .send(
                serde_json::to_vec(&json!({
                    "id": request.device_id,
                    "vars": { "locale": request.locale.as_str() },
                }))
                .map_err(|_| NakamaAuthFailure::Protocol)?,
            )
            .map_err(|_| NakamaAuthFailure::Transport)?;
        parse_session(response)
    }

    /// Authenticate an existing email account; account creation is never enabled.
    pub fn authenticate_email(
        &self,
        email: &str,
        password: &str,
    ) -> Result<NakamaSession, NakamaAuthFailure> {
        if email.is_empty()
            || email.len() > 320
            || email.chars().any(char::is_control)
            || password.is_empty()
            || password.len() > 4096
        {
            return Err(NakamaAuthFailure::InvalidRequest);
        }
        let response = self
            .agent
            .post(self.endpoint("/v2/account/authenticate/email"))
            .query("create", "false")
            .header("Authorization", &self.authorization)
            .header("Content-Type", "application/json")
            .send(
                serde_json::to_vec(&json!({ "email": email, "password": password }))
                    .map_err(|_| NakamaAuthFailure::Protocol)?,
            )
            .map_err(|_| NakamaAuthFailure::Transport)?;
        parse_session(response)
    }

    /// Exchange a prior refresh token for a new session pair.
    pub fn refresh(&self, refresh_token: &str) -> Result<NakamaSession, NakamaAuthFailure> {
        validate_outbound_token(refresh_token)?;
        let response = self
            .agent
            .post(self.endpoint("/v2/account/session/refresh"))
            .header("Authorization", &self.authorization)
            .header("Content-Type", "application/json")
            .send(
                serde_json::to_vec(&json!({ "token": refresh_token }))
                    .map_err(|_| NakamaAuthFailure::Protocol)?,
            )
            .map_err(|_| NakamaAuthFailure::Transport)?;
        parse_session(response)
    }

    /// Revoke this access/refresh-token pair without affecting other sessions.
    pub fn logout(&self, session: &NakamaSession) -> Result<(), NakamaAuthFailure> {
        validate_outbound_token(session.access_token())?;
        validate_outbound_token(session.refresh_token())?;
        let response = self
            .agent
            .post(self.endpoint("/v2/session/logout"))
            .header(
                "Authorization",
                format!("Bearer {}", session.access_token()),
            )
            .header("Content-Type", "application/json")
            .send(
                serde_json::to_vec(&json!({
                    "token": session.access_token(),
                    "refreshToken": session.refresh_token(),
                }))
                .map_err(|_| NakamaAuthFailure::Protocol)?,
            )
            .map_err(|_| NakamaAuthFailure::Transport)?;
        checked_body(response).map(|_| ())
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

fn validate_base_url(url: &str) -> Result<String, NakamaAuthFailure> {
    let base_url = url.trim_end_matches('/');
    let uri: ureq::http::Uri = base_url
        .parse()
        .map_err(|_| NakamaAuthFailure::InvalidConfiguration)?;
    let host = uri
        .host()
        .filter(|host| !host.is_empty())
        .ok_or(NakamaAuthFailure::InvalidConfiguration)?;
    let local_http = {
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    };
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri
            .authority()
            .is_none_or(|authority| authority.as_str().contains('@'))
        || uri.query().is_some()
        || uri.path() != "/"
        || (uri.scheme_str() == Some("http") && !local_http)
    {
        return Err(NakamaAuthFailure::InvalidConfiguration);
    }
    Ok(base_url.to_owned())
}

fn validate_nakama_device_id(value: &str) -> Result<(), NakamaAuthFailure> {
    if !(10..=128).contains(&value.len()) || value.chars().any(char::is_whitespace) {
        return Err(NakamaAuthFailure::InvalidRequest);
    }
    Ok(())
}

fn validate_outbound_token(value: &str) -> Result<(), NakamaAuthFailure> {
    if !is_valid_token(value) {
        return Err(NakamaAuthFailure::InvalidRequest);
    }
    Ok(())
}

fn is_valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

fn checked_body(mut response: Response<ureq::Body>) -> Result<String, NakamaAuthFailure> {
    let status = response.status();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|_| NakamaAuthFailure::Transport)?;
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 | 403 => NakamaAuthFailure::Authentication,
            429 => NakamaAuthFailure::RateLimited,
            502..=504 => NakamaAuthFailure::Unavailable,
            _ => NakamaAuthFailure::Protocol,
        });
    }
    Ok(body)
}

fn parse_session(response: Response<ureq::Body>) -> Result<NakamaSession, NakamaAuthFailure> {
    let response: SessionResponse =
        serde_json::from_str(&checked_body(response)?).map_err(|_| NakamaAuthFailure::Protocol)?;
    if !is_valid_token(&response.token) || !is_valid_token(&response.refresh_token) {
        return Err(NakamaAuthFailure::Protocol);
    }
    Ok(NakamaSession {
        access_token: response.token,
        refresh_token: response.refresh_token,
        created: response.created,
    })
}

#[derive(Deserialize)]
struct SessionResponse {
    token: String,
    refresh_token: String,
    #[serde(default)]
    created: bool,
}

#[cfg(test)]
mod tests {
    use engine_core::Locale;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    use super::*;

    fn read_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let length = stream.read(&mut buffer).unwrap();
            assert_ne!(length, 0, "client closed before completing request");
            bytes.extend_from_slice(&buffer[..length]);
            let Some(headers_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
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
                return String::from_utf8(bytes).unwrap();
            }
        }
    }

    fn write_json_response(stream: &mut TcpStream, body: &[u8]) {
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
        stream.write_all(body).unwrap();
    }

    fn authorization_header(request: &str) -> Option<&str> {
        request
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
            .and_then(|line| line.split_once(':').map(|(_, value)| value.trim()))
    }

    #[test]
    fn device_login_posts_safely_and_redacts_tokens() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(request.starts_with("POST /v2/account/authenticate/device?create=true"));
            assert_eq!(
                authorization_header(&request),
                Some("Basic cHVibGljLWtleTo=")
            );
            assert!(request.contains("\"id\":\"device-123\""));
            let body = b"{\"token\":\"access-secret\",\"refresh_token\":\"refresh-secret\",\"created\":true}";
            write_json_response(&mut stream, body);
        });
        let client = NakamaAuthClient::new(NakamaAuthConfig::new(
            format!("http://{address}"),
            "public-key",
        ))
        .unwrap();
        let session = client
            .authenticate_device(&DeviceLogin {
                device_id: "device-123".into(),
                username: None,
                create: true,
                locale: Locale::default(),
            })
            .unwrap();
        worker.join().unwrap();
        assert_eq!(session.access_token(), "access-secret");
        assert_eq!(session.refresh_token(), "refresh-secret");
        assert!(session.created);
        assert!(!format!("{client:?}").contains("public-key"));
        assert!(!format!("{session:?}").contains("access-secret"));
        assert!(!format!("{session:?}").contains("refresh-secret"));
    }

    #[test]
    fn refresh_posts_token_with_basic_authentication() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(request.starts_with("POST /v2/account/session/refresh HTTP/1.1"));
            assert_eq!(
                authorization_header(&request),
                Some("Basic cHVibGljLWtleTo=")
            );
            assert!(request.contains("\r\n\r\n{\"token\":\"refresh-secret\"}"));
            write_json_response(
                &mut stream,
                b"{\"token\":\"next-access\",\"refresh_token\":\"next-refresh\"}",
            );
        });
        let client = NakamaAuthClient::new(NakamaAuthConfig::new(
            format!("http://{address}"),
            "public-key",
        ))
        .unwrap();
        let session = client.refresh("refresh-secret").unwrap();
        worker.join().unwrap();
        assert_eq!(session.access_token(), "next-access");
        assert_eq!(session.refresh_token(), "next-refresh");
    }

    #[test]
    fn logout_revokes_the_access_and_refresh_pair() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(request.starts_with("POST /v2/session/logout HTTP/1.1"));
            assert_eq!(authorization_header(&request), Some("Bearer access-secret"));
            let body: serde_json::Value = serde_json::from_str(
                request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .expect("request body"),
            )
            .unwrap();
            assert_eq!(
                body,
                json!({ "token": "access-secret", "refreshToken": "refresh-secret" })
            );
            write_json_response(&mut stream, b"{}");
        });
        let client = NakamaAuthClient::new(NakamaAuthConfig::new(
            format!("http://{address}"),
            "public-key",
        ))
        .unwrap();
        let session = NakamaSession {
            access_token: "access-secret".into(),
            refresh_token: "refresh-secret".into(),
            created: false,
        };
        client.logout(&session).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn remote_plaintext_authentication_is_rejected() {
        assert!(matches!(
            NakamaAuthClient::new(NakamaAuthConfig::new("http://example.test", "key")),
            Err(NakamaAuthFailure::InvalidConfiguration)
        ));
        assert!(matches!(
            NakamaAuthClient::new(NakamaAuthConfig::new("https://example.test/path", "key")),
            Err(NakamaAuthFailure::InvalidConfiguration)
        ));
    }
}
