//! Validated, secret-free server profiles and bounded connection history.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const MAX_PROFILES: usize = 64;
pub const MAX_RECENT: usize = 20;
/// Environment override for the secret-free server profile document.
pub const CONFIG_PATH_ENV: &str = "LYNX_SERVER_BROWSER_PATH";
const MAX_PROFILE_FILE_BYTES: usize = 64 * 1024;

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

    /// Stable key used for profile identity, history, and future per-server
    /// credential isolation.
    pub fn key(&self) -> String {
        format!("{}:{}", self.host.to_ascii_lowercase(), self.port)
    }

    /// Whether this address is a loopback or private-network host that may use
    /// the plaintext development transports.
    #[must_use]
    pub fn is_local_network(&self) -> bool {
        if self.host.eq_ignore_ascii_case("localhost") {
            return true;
        }
        match self.host.parse::<IpAddr>() {
            Ok(IpAddr::V4(address)) => {
                address.is_loopback() || address.is_private() || address.is_link_local()
            }
            Ok(IpAddr::V6(address)) => {
                address.is_loopback()
                    || address.is_unique_local()
                    || address.is_unicast_link_local()
            }
            Err(_) => false,
        }
    }

    /// Browser-safe `host:port` authority, including IPv6 brackets.
    #[must_use]
    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// World WebSocket endpoint. Remote servers must use TLS; plaintext is
    /// allowed only for loopback and private-network development.
    #[must_use]
    pub fn world_url(&self) -> String {
        let scheme = if self.is_local_network() { "ws" } else { "wss" };
        format!("{scheme}://{}/world", self.authority())
    }

    /// World HTTP API endpoint, following the same local/remote transport rule.
    #[must_use]
    pub fn api_url(&self) -> String {
        let scheme = if self.is_local_network() {
            "http"
        } else {
            "https"
        };
        format!("{scheme}://{}", self.authority())
    }
}

/// Default secret-free profile document location.
///
/// `LYNX_SERVER_BROWSER_PATH` overrides the OS-specific configuration path so
/// developers and tests can use an isolated file.
#[must_use]
pub fn default_config_path() -> Option<PathBuf> {
    config_path(env::var_os(CONFIG_PATH_ENV), platform_config_dir())
}

fn config_path(
    override_path: Option<std::ffi::OsString>,
    platform: Option<PathBuf>,
) -> Option<PathBuf> {
    override_path
        .map(PathBuf::from)
        .or_else(|| platform.map(|directory| directory.join("lynx").join("servers.json")))
}

#[cfg(windows)]
fn platform_config_dir() -> Option<PathBuf> {
    env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn platform_config_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_config_dir() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
}

#[cfg(not(any(windows, unix)))]
fn platform_config_dir() -> Option<PathBuf> {
    None
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
    /// Load the bounded, secret-free profile document. A missing file starts
    /// with an empty browser; malformed or oversized files fail closed.
    pub fn load(path: &Path) -> io::Result<Self> {
        let file = match OpenOptions::new().read(true).open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::with_capacity(MAX_PROFILE_FILE_BYTES.min(4096));
        file.take((MAX_PROFILE_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_PROFILE_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "server profile document exceeds the size limit",
            ));
        }
        let json = std::str::from_utf8(&bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid profile encoding"))?;
        Self::from_json(json).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid server profile document",
            )
        })
    }

    /// Persist validated profiles through a sibling temporary file so an
    /// interrupted write does not leave a partial JSON document behind.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let json = self.to_json().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid server profile state")
        })?;
        if json.len() > MAX_PROFILE_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "server profile document exceeds the size limit",
            ));
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(format!(".{}.{}.tmp", std::process::id(), stamp));
        let temporary = std::path::PathBuf::from(temporary);
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temporary, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

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

#[cfg_attr(coverage_nightly, coverage(off))]
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

    #[test]
    fn transport_policy_uses_tls_only_for_remote_servers() {
        for host in ["localhost", "127.0.0.1", "::1", "192.168.1.10", "10.0.0.2"] {
            let profile = ServerProfile::new("Local", host, 8090).unwrap();
            assert!(profile.is_local_network(), "{host} is local");
            assert!(profile.world_url().starts_with("ws://"), "{host}");
            assert!(profile.api_url().starts_with("http://"), "{host}");
        }
        let remote = ServerProfile::new("Remote", "play.example.org", 443).unwrap();
        assert!(!remote.is_local_network());
        assert_eq!(remote.world_url(), "wss://play.example.org:443/world");
        assert_eq!(remote.api_url(), "https://play.example.org:443");

        let ipv6 = ServerProfile::new("V6", "2001:db8::1", 8090).unwrap();
        assert_eq!(ipv6.authority(), "[2001:db8::1]:8090");
        assert!(
            ipv6.world_url()
                .starts_with("wss://[2001:db8::1]:8090/world")
        );
    }

    #[test]
    fn profile_document_round_trips_through_a_new_parent_directory() {
        let directory = temporary_path("round-trip");
        let path = directory.join("nested").join("servers.json");
        let mut browser = ServerBrowser::default();
        browser
            .add(ServerProfile::new("Home", "localhost", 8090).unwrap())
            .unwrap();
        browser
            .add(ServerProfile::new("Friends", "192.168.1.20", 8091).unwrap())
            .unwrap();
        browser.set_pinned("192.168.1.20:8091", true).unwrap();
        browser.record_connected("localhost:8090").unwrap();
        browser.save(&path).unwrap();

        let loaded = ServerBrowser::load(&path).unwrap();
        assert_eq!(loaded, browser);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("password"));
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporary profile file remained");
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn missing_file_starts_empty_and_corrupt_or_oversized_files_fail_closed() {
        let directory = temporary_path("invalid");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("servers.json");
        assert_eq!(
            ServerBrowser::load(&path).unwrap(),
            ServerBrowser::default()
        );

        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(
            ServerBrowser::load(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        std::fs::write(&path, vec![b' '; MAX_PROFILE_FILE_BYTES + 1]).unwrap();
        assert_eq!(
            ServerBrowser::load(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn default_config_path_prefers_the_override_then_the_platform_directory() {
        let platform = temporary_path("config");
        assert_eq!(
            config_path(
                Some(std::ffi::OsString::from("custom.json")),
                Some(platform.clone())
            ),
            Some(std::path::PathBuf::from("custom.json"))
        );
        assert_eq!(
            config_path(None, Some(platform.clone())),
            Some(platform.join("lynx").join("servers.json"))
        );
        assert_eq!(config_path(None, None), None);
    }

    fn temporary_path(label: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "lynx-server-browser-{label}-{}-{stamp}",
            std::process::id()
        ))
    }
}
