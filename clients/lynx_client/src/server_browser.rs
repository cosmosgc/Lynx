//! Validated, secret-free server profiles and bounded connection history.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, net::IpAddr};

pub const MAX_PROFILES: usize = 64;
pub const MAX_RECENT: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerProfile {
    pub name: String,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub pinned: bool,
}

impl ServerProfile {
    pub fn new(name: impl Into<String>, host: impl Into<String>, port: u16) -> Result<Self, Error> {
        let profile = Self {
            name: name.into(),
            host: host.into(),
            port,
            pinned: false,
        };
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.name.trim().is_empty()
            || self.name.len() > 80
            || self.name.chars().any(char::is_control)
        {
            return Err(Error::InvalidName);
        }
        if self.port == 0 || !valid_host(&self.host) {
            return Err(Error::InvalidAddress);
        }
        Ok(())
    }

    /// Stable key used for history and per-server credential-store isolation.
    pub fn key(&self) -> String {
        format!("{}:{}", self.host.to_ascii_lowercase(), self.port)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerBrowser {
    profiles: Vec<ServerProfile>,
    recent: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidName,
    InvalidAddress,
    DuplicateAddress,
    ProfileLimit,
    UnknownProfile,
    InvalidDocument,
}

impl ServerBrowser {
    pub fn profiles(&self) -> &[ServerProfile] {
        &self.profiles
    }
    pub fn recent(&self) -> &[String] {
        &self.recent
    }

    pub fn add(&mut self, profile: ServerProfile) -> Result<(), Error> {
        profile.validate()?;
        if self.profiles.len() >= MAX_PROFILES {
            return Err(Error::ProfileLimit);
        }
        if self.profiles.iter().any(|p| p.key() == profile.key()) {
            return Err(Error::DuplicateAddress);
        }
        self.profiles.push(profile);
        self.profiles
            .sort_by_key(|p| (!p.pinned, p.name.to_lowercase(), p.key()));
        Ok(())
    }

    pub fn set_pinned(&mut self, key: &str, pinned: bool) -> Result<(), Error> {
        let profile = self
            .profiles
            .iter_mut()
            .find(|p| p.key() == key)
            .ok_or(Error::UnknownProfile)?;
        profile.pinned = pinned;
        self.profiles
            .sort_by_key(|p| (!p.pinned, p.name.to_lowercase(), p.key()));
        Ok(())
    }

    /// Record only an explicit successful connection; unknown profiles cannot
    /// be smuggled into history and recent keys stay unique and bounded.
    pub fn record_connected(&mut self, key: &str) -> Result<(), Error> {
        if !self.profiles.iter().any(|p| p.key() == key) {
            return Err(Error::UnknownProfile);
        }
        self.recent.retain(|entry| entry != key);
        self.recent.insert(0, key.to_owned());
        self.recent.truncate(MAX_RECENT);
        Ok(())
    }

    pub fn to_json(&self) -> Result<String, Error> {
        serde_json::to_string_pretty(self).map_err(|_| Error::InvalidDocument)
    }

    pub fn from_json(json: &str) -> Result<Self, Error> {
        let browser: Self = serde_json::from_str(json).map_err(|_| Error::InvalidDocument)?;
        if browser.profiles.len() > MAX_PROFILES || browser.recent.len() > MAX_RECENT {
            return Err(Error::InvalidDocument);
        }
        let mut keys = BTreeSet::new();
        for profile in &browser.profiles {
            profile.validate().map_err(|_| Error::InvalidDocument)?;
            if !keys.insert(profile.key()) {
                return Err(Error::InvalidDocument);
            }
        }
        if browser.recent.iter().any(|key| !keys.contains(key))
            || browser.recent.iter().collect::<BTreeSet<_>>().len() != browser.recent.len()
        {
            return Err(Error::InvalidDocument);
        }
        Ok(browser)
    }
}

fn valid_host(host: &str) -> bool {
    if host.is_empty()
        || host.len() > 253
        || host.trim() != host
        || host.chars().any(char::is_control)
    {
        return false;
    }
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    if !host.is_ascii() || host.contains(['/', '@', ':', '?', '#', '%', ' ']) {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.as_bytes()[0].is_ascii_alphanumeric()
            && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_validate_addresses_and_reject_url_credentials_or_paths() {
        for host in ["example.org", "127.0.0.1", "2001:db8::1"] {
            assert!(ServerProfile::new("Home", host, 8090).is_ok());
        }
        for host in [
            "",
            "https://example.org",
            "user@example.org",
            "example.org/path",
            "bad host",
        ] {
            assert_eq!(
                ServerProfile::new("Home", host, 8090),
                Err(Error::InvalidAddress)
            );
        }
        assert_eq!(
            ServerProfile::new("", "localhost", 8090),
            Err(Error::InvalidName)
        );
        assert_eq!(
            ServerProfile::new("Home", "localhost", 0),
            Err(Error::InvalidAddress)
        );
    }

    #[test]
    fn pins_sort_first_and_recent_success_is_unique_and_bounded() {
        let mut browser = ServerBrowser::default();
        for n in 0..24 {
            let profile =
                ServerProfile::new(format!("Server {n}"), format!("s{n}.example.org"), 8000 + n)
                    .unwrap();
            browser.add(profile).unwrap();
        }
        let keys: Vec<_> = browser.profiles().iter().map(ServerProfile::key).collect();
        let recent_key = keys[0].clone();
        for key in &keys {
            browser.record_connected(key).unwrap();
        }
        browser.record_connected(&recent_key).unwrap();
        assert_eq!(browser.recent().len(), MAX_RECENT);
        assert_eq!(browser.recent()[0], recent_key);
        browser.set_pinned(&recent_key, true).unwrap();
        assert_eq!(browser.profiles()[0].key(), recent_key);
        assert!(browser.record_connected("unknown:1").is_err());
        assert!(ServerBrowser::from_json(&browser.to_json().unwrap()).is_ok());
    }

    #[test]
    fn persistence_schema_rejects_secrets_unknown_fields_and_invalid_history() {
        let mut browser = ServerBrowser::default();
        browser
            .add(ServerProfile::new("Local", "localhost", 8090).unwrap())
            .unwrap();
        let json = browser.to_json().unwrap();
        assert!(!json.contains("token"));
        assert!(
            ServerBrowser::from_json(
                &json.replace("\"recent\": []", "\"recent\": [\"missing:1\"]")
            )
            .is_err()
        );
        assert!(
            ServerBrowser::from_json(
                &json.replace("\"recent\": []", "\"recent\": [], \"password\": \"secret\"")
            )
            .is_err()
        );
    }
}
