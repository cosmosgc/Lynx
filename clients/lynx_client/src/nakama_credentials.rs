//! Secure persistence for the Nakama device identity and rotating refresh token.
//!
//! No file-based fallback is provided. If the operating-system credential
//! store is unavailable, login fails instead of writing credentials in
//! plaintext.

use std::fmt;

use engine_core::Locale;
use game_api::DeviceLogin;
use lynx_client_net::{NakamaAuthClient, NakamaAuthFailure, NakamaSession};
use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "com.lynx.client.nakama";

/// Per-server credentials retained between client launches.
#[derive(PartialEq, Eq)]
pub struct NakamaCredential {
    device_id: String,
    refresh_token: String,
}

impl NakamaCredential {
    /// Store a device identifier and the current rotated refresh token.
    #[must_use]
    pub fn new(device_id: impl Into<String>, refresh_token: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            refresh_token: refresh_token.into(),
        }
    }

    /// Borrow the persistent Nakama device identifier.
    #[must_use]
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Borrow the refresh token for one refresh operation.
    #[must_use]
    pub fn refresh_token(&self) -> &str {
        &self.refresh_token
    }
}

impl fmt::Debug for NakamaCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NakamaCredential")
            .field("device_id", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .finish()
    }
}

/// Coarse credential-store failures, with no platform error text or secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialStoreError {
    Unavailable,
    InvalidRecord,
}

/// Login failures safe to display without exposing platform error details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NakamaLoginError {
    Authentication(NakamaAuthFailure),
    CredentialStore(CredentialStoreError),
    DeviceIdGeneration,
}

/// Outcome of clearing a saved Nakama device session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NakamaLogoutOutcome {
    NoSavedCredentials,
    Revoked,
    ExpiredCredentialsCleared,
}

/// Injectable storage boundary for OS credentials and deterministic tests.
pub trait NakamaCredentialStore: Send + Sync {
    /// Load credentials for a Nakama base URL; `None` means first login.
    fn load(&self, server_url: &str) -> Result<Option<NakamaCredential>, CredentialStoreError>;

    /// Store the current device ID and refresh token securely.
    fn save(
        &self,
        server_url: &str,
        credential: &NakamaCredential,
    ) -> Result<(), CredentialStoreError>;

    /// Delete stored credentials after a successful logout.
    fn delete(&self, server_url: &str) -> Result<(), CredentialStoreError>;
}

/// Credential store backed by Windows Credential Manager, macOS Keychain, or
/// the Linux Secret Service. Other platforms fail closed.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsNakamaCredentialStore;

impl NakamaCredentialStore for OsNakamaCredentialStore {
    fn load(&self, server_url: &str) -> Result<Option<NakamaCredential>, CredentialStoreError> {
        let entry = keyring_entry(server_url)?;
        let value = match entry.get_password() {
            Ok(value) => value,
            Err(keyring::Error::NoEntry) => return Ok(None),
            Err(_) => return Err(CredentialStoreError::Unavailable),
        };
        serde_json::from_str::<CredentialRecord>(&value)
            .map(|record| {
                Some(NakamaCredential::new(
                    record.device_id,
                    record.refresh_token,
                ))
            })
            .map_err(|_| CredentialStoreError::InvalidRecord)
    }

    fn save(
        &self,
        server_url: &str,
        credential: &NakamaCredential,
    ) -> Result<(), CredentialStoreError> {
        let value = serde_json::to_string(&CredentialRecord {
            device_id: credential.device_id.clone(),
            refresh_token: credential.refresh_token.clone(),
        })
        .map_err(|_| CredentialStoreError::InvalidRecord)?;
        keyring_entry(server_url)?
            .set_password(&value)
            .map_err(|_| CredentialStoreError::Unavailable)
    }

    fn delete(&self, server_url: &str) -> Result<(), CredentialStoreError> {
        let entry = keyring_entry(server_url)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(CredentialStoreError::Unavailable),
        }
    }
}

fn keyring_entry(server_url: &str) -> Result<keyring::Entry, CredentialStoreError> {
    keyring::Entry::new(KEYRING_SERVICE, server_url).map_err(|_| CredentialStoreError::Unavailable)
}

#[derive(Serialize, Deserialize)]
struct CredentialRecord {
    device_id: String,
    refresh_token: String,
}

/// Resume a saved account or create a new device account, persisting each
/// rotated refresh token before returning the access session to the caller.
///
/// If a saved refresh token is rejected, the same saved device ID is
/// reauthenticated with account creation disabled. Any credential-store error
/// fails closed; there is no plaintext fallback.
pub fn authenticate_or_resume(
    client: &NakamaAuthClient,
    server_url: &str,
    store: &dyn NakamaCredentialStore,
    locale: Locale,
) -> Result<NakamaSession, NakamaLoginError> {
    let previous = store
        .load(server_url)
        .map_err(NakamaLoginError::CredentialStore)?;
    let (device_id, session) = if let Some(previous) = previous {
        let session = match client.refresh(previous.refresh_token()) {
            Ok(session) => session,
            Err(NakamaAuthFailure::Authentication) => client
                .authenticate_device(&DeviceLogin {
                    device_id: previous.device_id().to_owned(),
                    username: None,
                    create: false,
                    locale,
                })
                .map_err(NakamaLoginError::Authentication)?,
            Err(error) => return Err(NakamaLoginError::Authentication(error)),
        };
        (previous.device_id().to_owned(), session)
    } else {
        let device_id = new_device_id().ok_or(NakamaLoginError::DeviceIdGeneration)?;
        let session = client
            .authenticate_device(&DeviceLogin {
                device_id: device_id.clone(),
                username: None,
                create: true,
                locale,
            })
            .map_err(NakamaLoginError::Authentication)?;
        (device_id, session)
    };

    store
        .save(
            server_url,
            &NakamaCredential::new(device_id, session.refresh_token().to_owned()),
        )
        .map_err(NakamaLoginError::CredentialStore)?;
    Ok(session)
}

/// Revoke and remove a saved session. The rotated refresh token is saved before
/// logout so an interrupted request never leaves only an already-consumed token
/// in the credential store.
pub fn logout_saved_account(
    client: &NakamaAuthClient,
    server_url: &str,
    store: &dyn NakamaCredentialStore,
) -> Result<NakamaLogoutOutcome, NakamaLoginError> {
    let Some(credential) = store
        .load(server_url)
        .map_err(NakamaLoginError::CredentialStore)?
    else {
        return Ok(NakamaLogoutOutcome::NoSavedCredentials);
    };
    let session = match client.refresh(credential.refresh_token()) {
        Ok(session) => session,
        Err(NakamaAuthFailure::Authentication) => {
            store
                .delete(server_url)
                .map_err(NakamaLoginError::CredentialStore)?;
            return Ok(NakamaLogoutOutcome::ExpiredCredentialsCleared);
        }
        Err(error) => return Err(NakamaLoginError::Authentication(error)),
    };
    store
        .save(
            server_url,
            &NakamaCredential::new(
                credential.device_id().to_owned(),
                session.refresh_token().to_owned(),
            ),
        )
        .map_err(NakamaLoginError::CredentialStore)?;
    client
        .logout(&session)
        .map_err(NakamaLoginError::Authentication)?;
    store
        .delete(server_url)
        .map_err(NakamaLoginError::CredentialStore)?;
    Ok(NakamaLogoutOutcome::Revoked)
}

fn new_device_id() -> Option<String> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).ok()?;
    let hex = b"0123456789abcdef";
    let mut device_id = String::with_capacity(random.len() * 2);
    for byte in random {
        device_id.push(char::from(hex[usize::from(byte >> 4)]));
        device_id.push(char::from(hex[usize::from(byte & 0x0f)]));
    }
    Some(device_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lynx_client_net::NakamaAuthConfig;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;

    #[test]
    fn credential_debug_output_redacts_device_and_refresh_secrets() {
        let credential = NakamaCredential::new("device-secret", "refresh-secret");
        let debug = format!("{credential:?}");
        assert!(!debug.contains("device-secret"));
        assert!(!debug.contains("refresh-secret"));
    }

    #[test]
    fn credential_json_round_trip_is_internal_to_the_secure_store() {
        let credential = NakamaCredential::new("device-id", "refresh-token");
        let encoded = serde_json::to_string(&CredentialRecord {
            device_id: credential.device_id.clone(),
            refresh_token: credential.refresh_token.clone(),
        })
        .unwrap();
        let decoded: CredentialRecord = serde_json::from_str(&encoded).unwrap();
        let decoded = NakamaCredential::new(decoded.device_id, decoded.refresh_token);
        assert_eq!(decoded, credential);
    }

    #[derive(Default)]
    struct MemoryStore(Mutex<Option<(String, String)>>);

    impl NakamaCredentialStore for MemoryStore {
        fn load(
            &self,
            _server_url: &str,
        ) -> Result<Option<NakamaCredential>, CredentialStoreError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .as_ref()
                .map(|(device, token)| NakamaCredential::new(device.clone(), token.clone())))
        }

        fn save(
            &self,
            _server_url: &str,
            credential: &NakamaCredential,
        ) -> Result<(), CredentialStoreError> {
            *self.0.lock().unwrap() = Some((
                credential.device_id().to_owned(),
                credential.refresh_token().to_owned(),
            ));
            Ok(())
        }

        fn delete(&self, _server_url: &str) -> Result<(), CredentialStoreError> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let length = stream.read(&mut buffer).unwrap();
            assert_ne!(length, 0, "client closed before completing HTTP request");
            request.extend_from_slice(&buffer[..length]);
            let Some(headers_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..headers_end]);
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
            if request.len() >= headers_end + 4 + content_length {
                return String::from_utf8(request).unwrap();
            }
        }
    }

    fn respond_json(stream: &mut TcpStream, body: &[u8]) {
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

    #[test]
    fn fresh_device_login_is_saved_then_reused_with_rotating_refresh_token() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_url = format!("http://{address}");
        let worker = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let request = read_http_request(&mut first);
            assert!(request.starts_with("POST /v2/account/authenticate/device?create=true"));
            let body: serde_json::Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            let device_id = body["id"].as_str().unwrap().to_owned();
            assert_eq!(device_id.len(), 32);
            respond_json(
                &mut first,
                b"{\"token\":\"access-one\",\"refresh_token\":\"refresh-one\",\"created\":true}",
            );

            let (mut second, _) = listener.accept().unwrap();
            let request = read_http_request(&mut second);
            assert!(request.starts_with("POST /v2/account/session/refresh HTTP/1.1"));
            let body: serde_json::Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["token"], "refresh-one");
            respond_json(
                &mut second,
                b"{\"token\":\"access-two\",\"refresh_token\":\"refresh-two\"}",
            );
            device_id
        });

        let store = MemoryStore::default();
        let client =
            NakamaAuthClient::new(NakamaAuthConfig::new(&server_url, "public-key")).unwrap();
        let first =
            authenticate_or_resume(&client, &server_url, &store, Locale::default()).unwrap();
        assert_eq!(first.access_token(), "access-one");
        assert_eq!(first.refresh_token(), "refresh-one");
        let second =
            authenticate_or_resume(&client, &server_url, &store, Locale::default()).unwrap();
        assert_eq!(second.access_token(), "access-two");
        assert_eq!(second.refresh_token(), "refresh-two");
        let device_id = worker.join().unwrap();
        let saved = store.load(&server_url).unwrap().unwrap();
        assert_eq!(saved.device_id(), device_id);
        assert_eq!(saved.refresh_token(), "refresh-two");
    }

    #[test]
    fn logout_revokes_rotated_credentials_and_removes_secure_entry() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_url = format!("http://{address}");
        let worker = std::thread::spawn(move || {
            let (mut refresh, _) = listener.accept().unwrap();
            let request = read_http_request(&mut refresh);
            assert!(request.starts_with("POST /v2/account/session/refresh HTTP/1.1"));
            respond_json(
                &mut refresh,
                b"{\"token\":\"logout-access\",\"refresh_token\":\"logout-refresh\"}",
            );

            let (mut logout, _) = listener.accept().unwrap();
            let request = read_http_request(&mut logout);
            assert!(request.starts_with("POST /v2/session/logout HTTP/1.1"));
            assert!(
                request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("authorization: Bearer logout-access"))
            );
            let body: serde_json::Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(
                body,
                serde_json::json!({
                    "token": "logout-access",
                    "refreshToken": "logout-refresh"
                })
            );
            respond_json(&mut logout, b"{}");
        });

        let store = MemoryStore::default();
        store
            .save(
                &server_url,
                &NakamaCredential::new("known-device-id", "old-refresh"),
            )
            .unwrap();
        let client =
            NakamaAuthClient::new(NakamaAuthConfig::new(&server_url, "public-key")).unwrap();
        assert_eq!(
            logout_saved_account(&client, &server_url, &store).unwrap(),
            NakamaLogoutOutcome::Revoked
        );
        worker.join().unwrap();
        assert!(store.load(&server_url).unwrap().is_none());
    }
}
