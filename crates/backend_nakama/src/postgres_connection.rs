//! Shared PostgreSQL configuration and connection policy.

use std::fs;
use std::io::Cursor;
use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;

use postgres::config::Host;
use postgres::{Client, Config};
use postgres_native_tls::MakeTlsConnector;
use thiserror::Error;

const LOOPBACK_OPT_IN: &str = "LYNX_PG_ALLOW_INSECURE_LOOPBACK";
const CA_CERT: &str = "LYNX_PG_TLS_CA_CERT";

/// Connection failures deliberately omit driver details, which may include a DSN.
#[derive(Debug, Error)]
#[error("PostgreSQL connection failed or configuration is invalid")]
pub struct ConnectionError;

/// Build the shared PostgreSQL configuration from `LYNX_PG_*` variables.
pub fn config_from_env(application_name: &str) -> Result<Config, &'static str> {
    let host = environment_or("LYNX_PG_HOST", "127.0.0.1");
    let port = environment_or("LYNX_PG_PORT", "5432")
        .parse::<u16>()
        .map_err(|_| "LYNX_PG_PORT is not a valid port")?;
    let database = required_environment("LYNX_PG_DATABASE")?;
    let user = required_environment("LYNX_PG_USER")?;
    let password = required_environment("LYNX_PG_PASSWORD")?;

    let mut config = Config::new();
    config
        .host(&host)
        .port(port)
        .dbname(&database)
        .user(&user)
        .password(&password)
        .application_name(application_name)
        .connect_timeout(Duration::from_secs(5))
        .options("-c statement_timeout=5000 -c lock_timeout=2000 -c idle_in_transaction_session_timeout=5000");
    Ok(config)
}

/// Connect according to host classification and the explicit local plaintext opt-in.
pub fn connect(config: &Config) -> Result<Client, ConnectionError> {
    if plaintext_is_allowed(
        config.get_hosts(),
        config.get_hostaddrs(),
        environment_is_true(LOOPBACK_OPT_IN),
    ) {
        return config.connect(postgres::NoTls).map_err(|_| ConnectionError);
    }

    let mut tls_builder = native_tls::TlsConnector::builder();
    if let Some(path) = std::env::var_os(CA_CERT) {
        let pem = fs::read(path).map_err(|_| ConnectionError)?;
        let certificates = rustls_pemfile::certs(&mut Cursor::new(pem))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ConnectionError)?;
        if certificates.is_empty() {
            return Err(ConnectionError);
        }
        for der in certificates {
            let certificate =
                native_tls::Certificate::from_der(der.as_ref()).map_err(|_| ConnectionError)?;
            tls_builder.add_root_certificate(certificate);
        }
    }
    let tls = MakeTlsConnector::new(tls_builder.build().map_err(|_| ConnectionError)?);
    let mut verified = config.clone();
    // `native-tls` validates the certificate chain and server name by default;
    // `Require` prevents PostgreSQL's `prefer` negotiation from falling back.
    verified.ssl_mode(postgres::config::SslMode::Require);
    verified.connect(tls).map_err(|_| ConnectionError)
}

/// Retry only initial connection establishment, within a fixed small budget.
pub fn connect_with_retry(config: &Config) -> Result<Client, ConnectionError> {
    const ATTEMPTS: usize = 3;
    let mut delay = Duration::from_millis(100);
    for attempt in 0..ATTEMPTS {
        if let Ok(client) = connect(config) {
            return Ok(client);
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(delay);
            delay *= 2;
        }
    }
    Err(ConnectionError)
}

fn host_is_loopback(host: &Host) -> bool {
    match host {
        Host::Tcp(host) => is_loopback_host(host),
        #[cfg(unix)]
        Host::Unix(_) => true,
    }
}

fn plaintext_is_allowed(hosts: &[Host], addresses: &[IpAddr], opt_in: bool) -> bool {
    opt_in
        && !hosts.is_empty()
        && hosts.iter().all(host_is_loopback)
        && addresses.iter().all(IpAddr::is_loopback)
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || IpAddr::from_str(host).is_ok_and(|address| address.is_loopback())
}

fn environment_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn required_environment(name: &str) -> Result<String, &'static str> {
    std::env::var(name).map_err(|_| "a required PostgreSQL environment variable is missing")
}

fn environment_is_true(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| is_true_flag(&value))
}

fn is_true_flag(value: &str) -> bool {
    value.eq_ignore_ascii_case("true")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_ip_and_name_loopback_hosts() {
        for host in ["localhost", "LOCALHOST", "127.0.0.1", "::1"] {
            assert!(is_loopback_host(host), "{host}");
        }
        for host in ["db.internal", "192.168.1.2", "0.0.0.0"] {
            assert!(!is_loopback_host(host), "{host}");
        }
    }

    #[test]
    fn plaintext_requires_explicit_opt_in_and_every_host_to_be_loopback() {
        let loopback = vec![Host::Tcp("localhost".to_owned())];
        let remote = vec![Host::Tcp("db.internal".to_owned())];
        assert!(!plaintext_is_allowed(&loopback, &[], false));
        assert!(plaintext_is_allowed(&loopback, &[], true));
        assert!(!plaintext_is_allowed(&remote, &[], true));
        assert!(!plaintext_is_allowed(
            &[loopback[0].clone(), remote[0].clone()],
            &[],
            true
        ));
        assert!(!plaintext_is_allowed(
            &loopback,
            &["192.168.1.5".parse().unwrap()],
            true
        ));
        assert!(!plaintext_is_allowed(&[], &[], true));
    }

    #[test]
    fn loopback_opt_in_is_exact_and_explicit() {
        assert!(!is_true_flag("true "));
        assert!(is_true_flag("TRUE"));
        assert!(!is_true_flag("1"));
        assert_eq!(LOOPBACK_OPT_IN, "LYNX_PG_ALLOW_INSECURE_LOOPBACK");
    }

    #[test]
    fn connection_errors_do_not_include_connection_details() {
        let message = ConnectionError.to_string();
        assert!(!message.contains("password"));
        assert!(!message.contains("postgres://"));
    }
}
