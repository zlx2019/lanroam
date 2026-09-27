//! Node: one running Lanroam instance. Loads the identity, binds the QUIC
//! endpoint and starts discovery with the endpoint as its identity probe.

use std::path::PathBuf;
use std::sync::Arc;

use lan_kit::discovery::{DiscoveryError, DiscoveryOptions, IdentityProbe};
use lan_kit::identity::IdentityError;
use lan_kit::{DeviceIdentity, DiscoveryService, Peer, PeerEvent, PeerInfo};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::PROFILE;
use crate::protocol::{PROP_PROTOCOL, PROTOCOL_VERSION, Purpose};
use crate::transport::{Link, Transport, TransportError};

/// Node startup errors
#[derive(Debug, Error)]
pub enum NodeError {
    /// Loading or creating the device identity failed
    #[error(transparent)]
    Identity(#[from] IdentityError),
    /// Binding the QUIC endpoint failed
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// Discovery could not start
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
}

/// How to start a node
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Identity data directory
    pub data_dir: PathBuf,
    /// QUIC port (0 picks a free one, for several instances on one machine)
    pub port: u16,
    /// UDP multicast discovery port
    pub discovery_port: u16,
    /// Display name for this run only; the persisted name stays untouched
    pub display_name: Option<String>,
    /// Listen without advertising (short-lived tools that only dial)
    pub passive: bool,
}

impl NodeConfig {
    /// Defaults: standard ports, advertised, persisted display name
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            port: crate::DEFAULT_PORT,
            discovery_port: crate::DEFAULT_DISCOVERY_PORT,
            display_name: None,
            passive: false,
        }
    }
}

/// A running node
pub struct Node {
    /// Device identity
    identity: Arc<DeviceIdentity>,
    /// Info this node advertises and sends in handshakes
    info: PeerInfo,
    /// QUIC endpoint
    transport: Arc<Transport>,
    /// Discovery service
    discovery: DiscoveryService,
}

impl Node {
    /// Start a node and return it with its peer event stream
    pub async fn start(config: NodeConfig) -> Result<(Self, mpsc::Receiver<PeerEvent>), NodeError> {
        let identity = Arc::new(DeviceIdentity::load_or_create(&config.data_dir, &PROFILE)?);
        let transport = Arc::new(Transport::bind(Arc::clone(&identity), config.port)?);

        let mut info = identity.peer_info();
        if let Some(name) = config.display_name {
            info.name = name;
        }
        info.props
            .insert(PROP_PROTOCOL.to_string(), PROTOCOL_VERSION.to_string());

        let mut options = DiscoveryOptions::new(transport.local_port(), config.discovery_port);
        options.passive = config.passive;
        options.prober = Some(Arc::clone(&transport) as Arc<dyn IdentityProbe>);
        let (discovery, events) = DiscoveryService::start(&PROFILE, info.clone(), options).await?;
        Ok((
            Self {
                identity,
                info,
                transport,
                discovery,
            },
            events,
        ))
    }

    /// Device identity
    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    /// Info this node advertises and sends in handshakes
    pub fn info(&self) -> &PeerInfo {
        &self.info
    }

    /// QUIC endpoint
    pub fn transport(&self) -> &Arc<Transport> {
        &self.transport
    }

    /// Discovery service
    pub fn discovery(&self) -> &DiscoveryService {
        &self.discovery
    }

    /// Dial a discovered peer for `purpose`
    pub async fn connect(&self, peer: &Peer, purpose: Purpose) -> Result<Link, TransportError> {
        self.transport.connect(peer, &self.info, purpose).await
    }

    /// Say goodbye on the LAN, close every connection and wait until peers
    /// were told
    pub async fn shutdown(&self) {
        self.discovery.shutdown().await;
        self.transport.close();
        // Bounded: an unresponsive peer must not hold up the exit
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.transport.wait_idle(),
        )
        .await;
    }
}
