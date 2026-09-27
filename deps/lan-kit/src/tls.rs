//! TLS layer: mutual TLS 1.3 with self-signed certificates and fingerprint
//! verification.
//!
//! No CA chain is involved:
//! - The client verifies the server certificate by fingerprint pinning (the
//!   expected fingerprint comes from discovery or from the user)
//! - The server requires a client certificate but performs no CA validation;
//!   after the handshake the app compares the fingerprint against its own
//!   trust model (pairing, group membership, ...)
//! - Only TLS 1.3 is offered, which is also what QUIC requires, so the same
//!   configs serve tokio-rustls over TCP and QUIC alike
//!
//! The configs carry no ALPN; apps set `alpn_protocols` on the returned
//! config.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme,
};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use thiserror::Error;

use crate::identity::{DeviceIdentity, fingerprint_of};

/// TLS layer errors
#[derive(Debug, Error)]
pub enum TlsError {
    /// Building the rustls config failed (invalid certificate or key, ...)
    #[error("failed to build the TLS config: {0}")]
    Config(#[from] rustls::Error),
}

/// Protocol versions offered on both sides
static VERSIONS: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];

/// Server config: present this device's certificate and require one from the
/// client (no CA validation; the app checks the fingerprint afterwards)
pub fn server_config(identity: &DeviceIdentity) -> Result<ServerConfig, TlsError> {
    let config = ServerConfig::builder_with_provider(shared_provider())
        .with_protocol_versions(VERSIONS)?
        .with_client_cert_verifier(Arc::new(AcceptAnyClientCert::new()))
        .with_single_cert(vec![identity.cert_der.clone()], identity.key_der())?;
    Ok(config)
}

/// Client config, presenting this device's certificate for mutual
/// authentication
///
/// `Some(expected_fingerprint)` strictly pins the server certificate; `None`
/// accepts any certificate. Use `None` only where the fingerprint is compared
/// right after the handshake (identity probes) or shown to the user (direct
/// connections in debug tools).
pub fn client_config(
    identity: &DeviceIdentity,
    expected_fingerprint: Option<String>,
) -> Result<ClientConfig, TlsError> {
    let mut config = ClientConfig::builder_with_provider(shared_provider())
        .with_protocol_versions(VERSIONS)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServerCert::new(expected_fingerprint)))
        .with_client_auth_cert(vec![identity.cert_der.clone()], identity.key_der())?;
    // **Session resumption stays off, so the client certificate is sent on
    // every connection.** A resumed TLS 1.3 handshake carries no client
    // certificate. A server that reads the fingerprint from the chain after
    // the handshake never notices, but one that reads it from a verification
    // callback gets nothing, drops the connection, and the client only sees a
    // bare "early eof" (hit in practice by Lanecho's native macOS client,
    // whose BoringSSL server issues tickets by default). Connections here are
    // either short transactions or long-lived sessions, so resumption saves
    // close to nothing anyway
    config.resumption = rustls::client::Resumption::disabled();
    Ok(config)
}

/// Fingerprint of the end-entity certificate in a peer's chain
pub fn peer_fingerprint(certs: Option<&[CertificateDer<'_>]>) -> Option<String> {
    certs.and_then(|c| c.first()).map(fingerprint_of)
}

/// Verify a TLS 1.2 handshake signature (required by the verifier traits;
/// never reached since only TLS 1.3 is offered)
fn verify_sig_tls12(
    provider: &CryptoProvider,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, rustls::Error> {
    rustls::crypto::verify_tls12_signature(
        message,
        cert,
        dss,
        &provider.signature_verification_algorithms,
    )
}

/// Verify a TLS 1.3 handshake signature (shared by both verifiers)
fn verify_sig_tls13(
    provider: &CryptoProvider,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, rustls::Error> {
    rustls::crypto::verify_tls13_signature(
        message,
        cert,
        dss,
        &provider.signature_verification_algorithms,
    )
}

/// Signature schemes the provider supports (shared by both verifiers)
fn supported_schemes(provider: &CryptoProvider) -> Vec<SignatureScheme> {
    provider
        .signature_verification_algorithms
        .supported_schemes()
}

/// Process-wide crypto provider
///
/// Building the algorithm tables has a fixed cost and configs are assembled
/// per connection in some apps, so one shared instance is reused.
fn shared_provider() -> Arc<CryptoProvider> {
    static PROVIDER: std::sync::OnceLock<Arc<CryptoProvider>> = std::sync::OnceLock::new();
    Arc::clone(PROVIDER.get_or_init(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider())))
}

/// Client-side verifier: pins the server certificate by fingerprint
#[derive(Debug)]
struct PinnedServerCert {
    /// Expected server certificate fingerprint; None accepts any certificate
    expected: Option<String>,
    /// Crypto provider for handshake signature verification
    provider: Arc<CryptoProvider>,
}

impl PinnedServerCert {
    /// Create the verifier on the shared provider
    fn new(expected: Option<String>) -> Self {
        Self {
            expected,
            provider: shared_provider(),
        }
    }
}

impl ServerCertVerifier for PinnedServerCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match &self.expected {
            Some(expected) => {
                let actual = fingerprint_of(end_entity);
                if &actual == expected {
                    Ok(ServerCertVerified::assertion())
                } else {
                    Err(rustls::Error::General(format!(
                        "peer certificate fingerprint mismatch: {actual}"
                    )))
                }
            }
            // Nothing to pin; the caller compares or shows the fingerprint
            None => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_sig_tls12(&self.provider, message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_sig_tls13(&self.provider, message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        supported_schemes(&self.provider)
    }
}

/// Server-side verifier: accepts any client certificate and leaves the trust
/// decision on the fingerprint to the app
#[derive(Debug)]
pub(crate) struct AcceptAnyClientCert {
    /// Crypto provider for handshake signature verification
    provider: Arc<CryptoProvider>,
}

impl AcceptAnyClientCert {
    /// Create the verifier on the shared provider
    pub(crate) fn new() -> Self {
        Self {
            provider: shared_provider(),
        }
    }
}

impl ClientCertVerifier for AcceptAnyClientCert {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        // Validity is not judged here; the app decides trust from the
        // fingerprint
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_sig_tls12(&self.provider, message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_sig_tls13(&self.provider, message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        supported_schemes(&self.provider)
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_rustls::{TlsAcceptor, TlsConnector};

    use super::*;
    use crate::identity::tests::test_identity;

    /// Server name used by the tests (only needed to satisfy the API)
    fn server_name() -> ServerName<'static> {
        ServerName::try_from("lan-kit-test").unwrap()
    }

    /// Loopback: pinning the right fingerprint completes the handshake, and
    /// each side can read the other's certificate fingerprint
    #[tokio::test]
    async fn handshake_with_pinned_fingerprint() {
        let ((_d1, server_id), (_d2, client_id)) = (test_identity(), test_identity());
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server_config(&server_id).unwrap()));

        let client_fp = client_id.fingerprint.clone();
        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut tls = acceptor.accept(tcp).await.unwrap();
            let fp = peer_fingerprint(tls.get_ref().1.peer_certificates()).unwrap();
            assert_eq!(fp, client_fp);
            assert_eq!(
                tls.get_ref().1.protocol_version(),
                Some(rustls::ProtocolVersion::TLSv1_3)
            );
            let mut buf = [0u8; 4];
            tls.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"ping");
        });

        let connector = TlsConnector::from(Arc::new(
            client_config(&client_id, Some(server_id.fingerprint.clone())).unwrap(),
        ));
        let tcp = TcpStream::connect(addr).await.unwrap();
        let mut tls = connector.connect(server_name(), tcp).await.unwrap();
        tls.write_all(b"ping").await.unwrap();
        tls.flush().await.unwrap();
        server_task.await.unwrap();
    }

    /// Pinning a wrong fingerprint fails the client handshake
    #[tokio::test]
    async fn handshake_rejects_wrong_fingerprint() {
        let ((_d1, server_id), (_d2, client_id)) = (test_identity(), test_identity());
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server_config(&server_id).unwrap()));
        tokio::spawn(async move {
            if let Ok((tcp, _)) = listener.accept().await {
                // Expected to fail; the result does not matter
                let _ = acceptor.accept(tcp).await;
            }
        });

        let connector = TlsConnector::from(Arc::new(
            client_config(&client_id, Some("0".repeat(64))).unwrap(),
        ));
        let tcp = TcpStream::connect(addr).await.unwrap();
        assert!(connector.connect(server_name(), tcp).await.is_err());
    }

    /// Verifier that counts client certificate verifications and delegates
    /// the real work
    #[derive(Debug)]
    struct CountingClientCert {
        inner: AcceptAnyClientCert,
        calls: Arc<AtomicUsize>,
    }

    impl ClientCertVerifier for CountingClientCert {
        fn root_hint_subjects(&self) -> &[DistinguishedName] {
            self.inner.root_hint_subjects()
        }

        fn verify_client_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            intermediates: &[CertificateDer<'_>],
            now: UnixTime,
        ) -> Result<ClientCertVerified, rustls::Error> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.inner
                .verify_client_cert(end_entity, intermediates, now)
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            self.inner.verify_tls12_signature(message, cert, dss)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            self.inner.verify_tls13_signature(message, cert, dss)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.inner.supported_verify_schemes()
        }
    }

    /// **Regression guard**: a reused ClientConfig still sends the client
    /// certificate on every connection.
    ///
    /// Remove the `resumption` line from `client_config` and this fails with
    /// 1 verification instead of 2: the second connection resumes and
    /// carries no client certificate.
    #[tokio::test]
    async fn reused_client_config_still_sends_the_certificate() {
        let ((_d1, server_id), (_d2, client_id)) = (test_identity(), test_identity());
        let calls = Arc::new(AtomicUsize::new(0));
        let verifier = CountingClientCert {
            inner: AcceptAnyClientCert::new(),
            calls: Arc::clone(&calls),
        };
        let mut server = ServerConfig::builder_with_provider(shared_provider())
            .with_protocol_versions(VERSIONS)
            .unwrap()
            .with_client_cert_verifier(Arc::new(verifier))
            .with_single_cert(vec![server_id.cert_der.clone()], server_id.key_der())
            .unwrap();
        // The ticketer is what makes this test meaningful: rustls servers
        // issue no tickets by default, so without it resumption could never
        // happen and the bug would stay invisible
        server.ticketer = rustls::crypto::aws_lc_rs::Ticketer::new().unwrap();

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server));
        let server_task = tokio::spawn(async move {
            for _ in 0..2 {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut tls = acceptor.accept(tcp).await.unwrap();
                let mut buf = [0u8; 4];
                tls.read_exact(&mut buf).await.unwrap();
                tls.write_all(b"pong").await.unwrap();
                tls.flush().await.unwrap();
            }
        });

        // One config for both connections, the way apps cache it
        let connector = TlsConnector::from(Arc::new(
            client_config(&client_id, Some(server_id.fingerprint.clone())).unwrap(),
        ));
        for _ in 0..2 {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let mut tls = connector.connect(server_name(), tcp).await.unwrap();
            tls.write_all(b"ping").await.unwrap();
            tls.flush().await.unwrap();
            // Reading the reply matters: TLS 1.3 tickets arrive after the
            // handshake, so a client that hangs up early never stores one
            let mut back = [0u8; 4];
            tls.read_exact(&mut back).await.unwrap();
        }
        server_task.await.unwrap();

        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "the second connection resumed and sent no client certificate"
        );
    }
}
