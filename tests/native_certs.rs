#![cfg(feature = "native-certs")]

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose, SanType};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use ureq::tls::{RootCerts, TlsConfig};

struct EnvGuard {
    key: &'static str,
    orig: Option<std::ffi::OsString>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.orig {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

impl EnvGuard {
    fn new(key: &'static str) -> Self {
        Self {
            key,
            orig: std::env::var_os(key),
        }
    }
}

struct CaAndServer {
    ca_pem: String,
    server_cert_der: CertificateDer<'static>,
    server_key_der: PrivateKeyDer<'static>,
}

fn generate_certs() -> CaAndServer {
    let mut ca_params = CertificateParams::new(Vec::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages.push(KeyUsagePurpose::KeyCertSign);
    ca_params.key_usages.push(KeyUsagePurpose::CrlSign);

    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    let mut server_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    server_params
        .subject_alt_names
        .push(SanType::IpAddress(Ipv4Addr::LOCALHOST.into()));

    let server_key = KeyPair::generate().unwrap();
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .unwrap();

    CaAndServer {
        ca_pem: ca_cert.pem(),
        server_cert_der: CertificateDer::from(server_cert.der().to_vec()),
        server_key_der: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der())),
    }
}

// Spawns a thread that accepts one TLS connection, reads an HTTP request, and
// responds with 200. If the client never connects (e.g. test panics before the
// TLS handshake), the thread blocks on accept() — this is fine because the test
// process exits on panic and the orphaned thread is killed with it.
fn spawn_tls_server(
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
) -> (SocketAddr, thread::JoinHandle<()>) {
    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("bad server config");

    let acceptor = Arc::new(server_config);

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let conn = rustls::ServerConnection::new(acceptor).unwrap();
        let mut tls = rustls::StreamOwned::new(conn, stream);

        let mut reader = BufReader::new(&mut tls);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                break;
            }
        }

        let body = "ok";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = reader.get_mut().write_all(response.as_bytes());
        let tls = reader.get_mut();
        tls.conn.send_close_notify();
        let _ = tls.conn.complete_io(&mut tls.sock);
    });

    (addr, handle)
}

#[test]
fn ureq_platform_verifier_trusts_custom_ca_via_ssl_cert_file() {
    let _guard_file = EnvGuard::new("SSL_CERT_FILE");
    let _guard_dir = EnvGuard::new("SSL_CERT_DIR");

    let certs = generate_certs();
    let ca_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(ca_file.path(), &certs.ca_pem).unwrap();

    unsafe {
        std::env::set_var("SSL_CERT_FILE", ca_file.path());
        std::env::set_var("SSL_CERT_DIR", "/nonexistent");
    }

    let (addr, server_handle) = spawn_tls_server(
        certs.server_cert_der.clone(),
        certs.server_key_der.clone_key(),
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .new_agent();

    let resp = agent
        .get(&format!("https://127.0.0.1:{}/", addr.port()))
        .call()
        .expect("platform verifier should trust custom CA via SSL_CERT_FILE");

    assert_eq!(resp.status(), 200);
    server_handle.join().expect("server thread panicked");

    // --- Control: without valid CA certs, TLS should fail ---
    let (addr2, server_handle2) = spawn_tls_server(certs.server_cert_der, certs.server_key_der);

    unsafe {
        std::env::set_var("SSL_CERT_FILE", "/dev/null");
    }

    let agent2: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .new_agent();

    let result = agent2
        .get(&format!("https://127.0.0.1:{}/", addr2.port()))
        .call();

    assert!(
        result.is_err(),
        "TLS should fail when SSL_CERT_FILE contains no valid CA certificates"
    );
    // Server thread may error on broken TLS handshake — that's expected.
    let _ = server_handle2.join();
}
