//! Shared PostgreSQL configuration and connection policy.

use std::fs;
use std::net::{IpAddr, ToSocketAddrs};
use std::str::FromStr;
use std::time::Duration;

use postgres::config::Host;
use postgres::{Client, Config};
use postgres_native_tls::MakeTlsConnector;
use rustls_pki_types::{CertificateDer, pem::PemObject};
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
        .tcp_user_timeout(Duration::from_secs(5))
        .keepalives(true)
        .keepalives_idle(Duration::from_secs(5))
        .keepalives_interval(Duration::from_secs(2))
        .keepalives_retries(3)
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

    connect_tls(config, std::env::var_os(CA_CERT))
}

fn connect_tls(
    config: &Config,
    ca_path: Option<std::ffi::OsString>,
) -> Result<Client, ConnectionError> {
    let mut tls_builder = native_tls::TlsConnector::builder();
    if let Some(path) = ca_path {
        let pem = fs::read(path).map_err(|_| ConnectionError)?;
        let certificates = CertificateDer::pem_slice_iter(&pem)
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

/// Accept an IP literal or hostname only when every address it resolves to is
/// loopback.
///
/// Checking the hostname text is not enough: `localhost` and any other name can
/// be redirected by hosts-file or DNS entries. Plaintext is a development-only
/// opt-in, so an unresolvable name is rejected instead of deferring the check
/// to the driver's own resolution.
fn is_loopback_host(host: &str) -> bool {
    if let Ok(address) = IpAddr::from_str(host) {
        return address.is_loopback();
    }
    let Ok(mut addresses) = (host, 0_u16).to_socket_addrs() else {
        return false;
    };
    let Some(first) = addresses.next() else {
        return false;
    };
    first.ip().is_loopback() && addresses.all(|address| address.ip().is_loopback())
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
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, TcpListener};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    // This self-signed localhost key pair is test-only and must never be used
    // outside the in-process TLS probe below.
    fn tls_probe_server() -> (Config, u16, mpsc::Receiver<bool>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (handshake_tx, handshake_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let Ok((mut socket, _)) = listener.accept() else {
                let _ = handshake_tx.send(false);
                return;
            };
            let _ = socket.set_read_timeout(Some(Duration::from_secs(3)));
            let mut request = [0_u8; 8];
            if socket.read_exact(&mut request).is_err()
                || u32::from_be_bytes(request[0..4].try_into().unwrap()) != 8
                || u32::from_be_bytes(request[4..8].try_into().unwrap()) != 80_877_103
                || socket.write_all(b"S").is_err()
            {
                let _ = handshake_tx.send(false);
                return;
            }
            let identity = native_tls::Identity::from_pkcs8(
                include_bytes!("../tests/fixtures/postgres-localhost-cert.pem"),
                include_bytes!("../tests/fixtures/postgres-localhost-key.pem"),
            )
            .unwrap();
            let acceptor = native_tls::TlsAcceptor::new(identity).unwrap();
            let Ok(mut tls) = acceptor.accept(socket) else {
                let _ = handshake_tx.send(false);
                return;
            };
            // Read the PostgreSQL startup packet, then close without an
            // authentication response. The TLS handshake itself is the subject
            // of this probe, not PostgreSQL authentication.
            let mut length = [0_u8; 4];
            let startup_received = tls.read_exact(&mut length).is_ok();
            let _ = handshake_tx.send(startup_received);
            if startup_received {
                let packet_length = u32::from_be_bytes(length) as usize;
                if (4..=16 * 1024).contains(&packet_length) {
                    let mut packet = vec![0_u8; packet_length - 4];
                    let _ = tls.read_exact(&mut packet);
                }
            }
        });
        let mut config = Config::new();
        config
            .host("localhost")
            // The probe listener is deliberately IPv4-only. Pin the socket
            // address while retaining `localhost` for certificate verification;
            // macOS may otherwise resolve localhost to ::1 first.
            .hostaddr(IpAddr::V4(Ipv4Addr::LOCALHOST))
            .port(port)
            .user("tls-test")
            .dbname("tls-test")
            .connect_timeout(Duration::from_secs(3));
        (config, port, handshake_rx, server)
    }

    #[test]
    fn recognizes_ip_and_name_loopback_hosts() {
        for host in ["localhost", "LOCALHOST", "127.0.0.1", "::1"] {
            assert!(is_loopback_host(host), "{host}");
        }
        for host in [
            "db.internal",
            "192.168.1.2",
            "0.0.0.0",
            "lynx-postgres.invalid",
        ] {
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

    #[test]
    fn verified_tls_handshake_accepts_configured_ca_and_matching_hostname() {
        let (config, _, handshake, server) = tls_probe_server();
        let ca = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/postgres-localhost-cert.pem")
            .into_os_string();
        assert!(connect_tls(&config, Some(ca)).is_err());
        assert!(handshake.recv_timeout(Duration::from_secs(4)).unwrap());
        server.join().unwrap();
    }

    #[test]
    fn verified_tls_handshake_rejects_hostname_mismatch() {
        let (_, port, handshake, server) = tls_probe_server();
        let mut config = Config::new();
        config
            .host("127.0.0.1")
            .port(port)
            .user("tls-test")
            .dbname("tls-test")
            .connect_timeout(Duration::from_secs(3));
        let ca = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/postgres-localhost-cert.pem")
            .into_os_string();
        assert!(connect_tls(&config, Some(ca)).is_err());
        assert!(!handshake.recv_timeout(Duration::from_secs(4)).unwrap());
        server.join().unwrap();
    }

    #[test]
    fn verified_tls_handshake_rejects_an_untrusted_certificate() {
        let (config, _, handshake, server) = tls_probe_server();
        assert!(connect_tls(&config, None).is_err());
        assert!(!handshake.recv_timeout(Duration::from_secs(4)).unwrap());
        server.join().unwrap();
    }

    #[test]
    fn initial_connection_retry_uses_a_bounded_budget() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut config = Config::new();
        config
            .host("127.0.0.1")
            .port(port)
            .user("retry-test")
            .dbname("retry-test")
            .connect_timeout(Duration::from_secs(1));
        let started = Instant::now();
        let error = connect_with_retry(&config).err().unwrap();
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(
            error.to_string(),
            "PostgreSQL connection failed or configuration is invalid"
        );
    }
}
