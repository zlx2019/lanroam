//! Test helpers shared across modules: throwaway identities and loopback
//! nodes.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lan_kit::{DeviceIdentity, Peer, PeerInfo};

use crate::PROFILE;
use crate::transport::{Link, Transport, TransportError};

/// Disambiguates temp directories within one process
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A dedicated temp directory, removed on drop
pub(crate) struct TempDir(pub(crate) std::path::PathBuf);

impl TempDir {
    pub(crate) fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "lanroam-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A node for the tests: identity + transport on a free port
pub(crate) struct TestNode {
    _dir: TempDir,
    pub(crate) transport: Arc<Transport>,
    pub(crate) info: PeerInfo,
}

impl TestNode {
    pub(crate) fn new() -> Self {
        let dir = TempDir::new();
        let identity = Arc::new(DeviceIdentity::load_or_create(&dir.0, &PROFILE).unwrap());
        let info = identity.peer_info();
        let transport = Arc::new(Transport::bind(identity, 0).unwrap());
        Self {
            _dir: dir,
            transport,
            info,
        }
    }

    /// This node as a discovered peer on loopback
    pub(crate) fn as_peer(&self) -> Peer {
        Peer {
            info: self.info.clone(),
            addrs: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            port: self.transport.local_port(),
        }
    }

    /// Accept one connection and run the handshake
    pub(crate) fn accept_one(&self) -> tokio::task::JoinHandle<Result<Link, TransportError>> {
        let transport = Arc::clone(&self.transport);
        let info = self.info.clone();
        tokio::spawn(async move {
            let incoming = transport.accept().await.unwrap();
            incoming.handshake(&info).await
        })
    }

    /// A connected pair of links: (dialer's, acceptor's)
    pub(crate) async fn link_pair() -> (TestNode, TestNode, Link, Link) {
        let (a, b) = (TestNode::new(), TestNode::new());
        let server = b.accept_one();
        let client = a.transport.connect(&b.as_peer(), &a.info).await.unwrap();
        let server = server.await.unwrap().unwrap();
        (a, b, client, server)
    }
}
