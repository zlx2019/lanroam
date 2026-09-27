//! Device identity: who I am, and how I prove it.
//!
//! - On first launch, generates a persistent UUID plus a self-signed X.509
//!   certificate (rcgen), stored in the app's data directory
//! - The certificate's BLAKE3 fingerprint is the device's network identity.
//!   The MAC address is not used: modern systems randomize it, reading it
//!   needs extra permissions, and it is privacy-sensitive
//! - The display name defaults to the hostname. Addresses are for dialing
//!   only, so an IP change never affects identity
//! - Trust is layered on top: mutual TLS 1.3 with fingerprint pinning (see
//!   [`crate::tls`]) plus whatever pairing model the app builds above that

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::profile::AppProfile;

/// Metadata file name (UUID, display name); written last, so its presence
/// marks a complete identity
const META_FILE: &str = "identity.json";
/// File name of the DER-encoded self-signed certificate
const CERT_FILE: &str = "cert.der";
/// File name of the DER-encoded PKCS#8 private key
const KEY_FILE: &str = "key.der";

/// Identity layer errors
#[derive(Debug, Error)]
pub enum IdentityError {
    /// Reading or writing an identity file failed
    #[error("identity file I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// Certificate or key generation failed
    #[error("certificate generation failed: {0}")]
    CertGen(#[from] rcgen::Error),
    /// Encoding or decoding the identity metadata failed
    #[error("identity metadata is malformed: {0}")]
    Meta(#[from] serde_json::Error),
}

/// Device info exchanged during discovery and handshakes
///
/// The JSON field names match the sibling apps' existing wire format, so a
/// migrated app keeps interoperating with its older releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    /// Unique device ID (UUID v4)
    pub device_id: String,
    /// Display name
    pub name: String,
    /// BLAKE3 fingerprint of the certificate (64 lowercase hex digits)
    pub fingerprint: String,
    /// Platform tag (macos / windows / linux)
    pub platform: String,
    /// OS version description (e.g. "macOS 15.3.1")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    /// App-defined properties advertised with the identity (a protocol
    /// version, a group id, ...). Discovery puts them into mDNS TXT records
    /// and UDP announcements, so they must stay small; see
    /// [`crate::discovery::validate_props`] for the exact rules
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub props: BTreeMap<String, String>,
}

/// Metadata persisted in identity.json
#[derive(Debug, Serialize, Deserialize)]
struct IdentityMeta {
    /// Unique device ID (UUID v4)
    device_id: String,
    /// User-chosen display name; None follows the hostname
    display_name: Option<String>,
}

/// Device identity: unique ID plus TLS certificate material
#[derive(Debug)]
pub struct DeviceIdentity {
    /// Unique device ID (a UUID v4 generated on first launch)
    pub device_id: String,
    /// Name shown to others (user-editable, follows the hostname by default)
    pub display_name: String,
    /// BLAKE3 fingerprint of the certificate (lowercase hex): the device's
    /// identity on the network
    pub fingerprint: String,
    /// DER-encoded self-signed certificate, presented during handshakes
    pub cert_der: CertificateDer<'static>,
    /// DER-encoded PKCS#8 private key; hand out copies via [`Self::key_der`]
    key_der: PrivateKeyDer<'static>,
}

impl DeviceIdentity {
    /// Load the identity from `dir`, generating and persisting a new one when
    /// any of the identity files is missing
    pub fn load_or_create(dir: &Path, profile: &AppProfile) -> Result<Self, IdentityError> {
        let complete = [META_FILE, CERT_FILE, KEY_FILE]
            .iter()
            .all(|f| dir.join(f).exists());
        if complete {
            Self::load(dir, profile)
        } else {
            Self::create(dir, profile)
        }
    }

    /// Load an existing identity from the data directory
    fn load(dir: &Path, profile: &AppProfile) -> Result<Self, IdentityError> {
        let cert_der = CertificateDer::from(fs::read(dir.join(CERT_FILE))?);
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(fs::read(dir.join(KEY_FILE))?));
        // Corrupt metadata is rebuilt around the intact certificate: the real
        // identity is the fingerprint, and the metadata is regenerable (a
        // fresh device_id, the display name falls back to the hostname).
        // Better than refusing to start with no way out
        let meta = match serde_json::from_slice::<IdentityMeta>(&fs::read(dir.join(META_FILE))?) {
            Ok(meta) => meta,
            Err(e) => {
                tracing::warn!(
                    "identity.json is malformed, rebuilding it from the certificate: {e}"
                );
                let meta = IdentityMeta {
                    device_id: uuid::Uuid::new_v4().to_string(),
                    display_name: None,
                };
                write_meta(dir, &meta)?;
                meta
            }
        };
        Ok(Self::from_parts(
            meta.device_id,
            meta.display_name
                .unwrap_or_else(|| default_display_name(profile)),
            cert_der,
            key_der,
        ))
    }

    /// Generate a new identity (UUID + self-signed certificate) and persist it
    fn create(dir: &Path, profile: &AppProfile) -> Result<Self, IdentityError> {
        let key_pair = rcgen::KeyPair::generate()?;
        let params = rcgen::CertificateParams::new(vec![profile.server_name.to_string()])?;
        let cert = params.self_signed(&key_pair)?;
        let cert_der = cert.der().clone();
        let key_bytes = key_pair.serialize_der();
        let device_id = uuid::Uuid::new_v4().to_string();

        fs::create_dir_all(dir)?;
        // Write order matters: the metadata lands last and marks the identity
        // complete, so an interrupted first launch regenerates everything
        // instead of keeping a truncated key that fails every later handshake
        write_atomic(&dir.join(KEY_FILE), &key_bytes, FileMode::Private)?;
        write_atomic(&dir.join(CERT_FILE), cert_der.as_ref(), FileMode::Public)?;
        write_meta(
            dir,
            &IdentityMeta {
                device_id: device_id.clone(),
                display_name: None,
            },
        )?;
        tracing::info!(%device_id, "generated a new device identity");

        Ok(Self::from_parts(
            device_id,
            default_display_name(profile),
            cert_der,
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_bytes)),
        ))
    }

    /// Assemble an identity from existing material; the fingerprint is
    /// computed here, in one place
    fn from_parts(
        device_id: String,
        display_name: String,
        cert_der: CertificateDer<'static>,
        key_der: PrivateKeyDer<'static>,
    ) -> Self {
        Self {
            fingerprint: fingerprint_of(&cert_der),
            device_id,
            display_name,
            cert_der,
            key_der,
        }
    }

    /// Copy of the private key (building a rustls config needs owned
    /// material)
    pub fn key_der(&self) -> PrivateKeyDer<'static> {
        self.key_der.clone_key()
    }

    /// Device info to advertise and to send in handshakes; props start empty
    /// and are filled in by the app
    pub fn peer_info(&self) -> PeerInfo {
        PeerInfo {
            device_id: self.device_id.clone(),
            name: self.display_name.clone(),
            fingerprint: self.fingerprint.clone(),
            platform: platform(),
            os_version: Some(os_version().to_string()),
            props: BTreeMap::new(),
        }
    }
}

/// Persist the display name to identity.json (None goes back to following
/// the hostname)
///
/// Only the metadata changes; certificate and key are untouched, so the
/// fingerprint stays the same. Call [`DeviceIdentity::load_or_create`] again
/// afterwards for a fresh snapshot.
pub fn persist_display_name(dir: &Path, name: Option<&str>) -> Result<(), IdentityError> {
    let mut meta: IdentityMeta = serde_json::from_slice(&fs::read(dir.join(META_FILE))?)?;
    meta.display_name = name.map(str::to_string);
    write_meta(dir, &meta)
}

/// Write identity.json atomically
fn write_meta(dir: &Path, meta: &IdentityMeta) -> Result<(), IdentityError> {
    write_atomic(
        &dir.join(META_FILE),
        &serde_json::to_vec_pretty(meta)?,
        FileMode::Public,
    )?;
    Ok(())
}

/// Who may read a written file
#[derive(Clone, Copy, PartialEq, Eq)]
enum FileMode {
    /// Default permissions
    Public,
    /// Owner only (0600 on unix); Windows relies on the per-user data
    /// directory's ACL
    Private,
}

/// Write a file atomically (temp file + rename): an interrupted in-place
/// overwrite would leave a torn file behind
fn write_atomic(path: &Path, bytes: &[u8], mode: FileMode) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    // A leftover temp file from a crash would keep its old permissions,
    // because the mode only applies when the file is created
    let _ = fs::remove_file(&tmp);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if mode == FileMode::Private {
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)
}

/// Platform tag of this machine (macos / windows / linux)
pub fn platform() -> String {
    std::env::consts::OS.to_string()
}

/// OS version description of this machine (e.g. "macOS 15.3.1")
///
/// Detection costs a syscall, so the result is cached for the process
/// lifetime (peer info is rebuilt on heartbeat and handshake paths).
pub fn os_version() -> &'static str {
    static OS_VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OS_VERSION.get_or_init(|| {
        let info = os_info::get();
        // os_info spells the macOS type "Mac OS"; use the official spelling
        let name = match info.os_type() {
            os_info::Type::Macos => "macOS".to_string(),
            t => t.to_string(),
        };
        format!("{} {}", name, info.version())
    })
}

/// Certificate fingerprint: lowercase hex of BLAKE3(cert_der)
///
/// The fingerprint only identifies devices inside these apps and never needs
/// to interoperate with other tools, so it uses BLAKE3, which is faster than
/// SHA-256 and already a dependency.
pub fn fingerprint_of(cert: &CertificateDer<'_>) -> String {
    blake3::hash(cert.as_ref()).to_hex().to_string()
}

/// Default display name: the hostname, falling back to the product name
fn default_display_name(profile: &AppProfile) -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| profile.product.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    /// Profile used across the crate's tests
    pub(crate) const TEST_PROFILE: AppProfile = AppProfile {
        product: "LanKitTest",
        server_name: "lan-kit-test",
        mdns_service_type: "_lan-kit-test._udp.local.",
        multicast_group: Ipv4Addr::new(224, 0, 0, 250),
    };

    /// A dedicated temp directory, removed on drop
    pub(crate) struct TempDir(pub(crate) std::path::PathBuf);

    impl TempDir {
        pub(crate) fn new() -> Self {
            let p = std::env::temp_dir().join(format!("lan-kit-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A fresh identity in its own temp directory
    pub(crate) fn test_identity() -> (TempDir, DeviceIdentity) {
        let dir = TempDir::new();
        let id = DeviceIdentity::load_or_create(&dir.0, &TEST_PROFILE).unwrap();
        (dir, id)
    }

    /// Loading again after the first generation yields the same identity
    #[test]
    fn create_then_load_is_stable() {
        let dir = TempDir::new();
        let a = DeviceIdentity::load_or_create(&dir.0, &TEST_PROFILE).unwrap();
        let b = DeviceIdentity::load_or_create(&dir.0, &TEST_PROFILE).unwrap();
        assert_eq!(a.device_id, b.device_id);
        assert_eq!(a.fingerprint, b.fingerprint);
    }

    /// The fingerprint is BLAKE3 as 64 lowercase hex digits
    #[test]
    fn fingerprint_is_hex64() {
        let (_dir, id) = test_identity();
        assert_eq!(id.fingerprint.len(), 64);
        assert!(
            id.fingerprint
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    /// Identities generated in different directories never collide
    #[test]
    fn identities_are_unique() {
        let ((_d1, a), (_d2, b)) = (test_identity(), test_identity());
        assert_ne!(a.fingerprint, b.fingerprint);
        assert_ne!(a.device_id, b.device_id);
    }

    /// Corrupt metadata is rebuilt; the fingerprint (the real identity)
    /// survives
    #[test]
    fn corrupt_meta_keeps_fingerprint() {
        let dir = TempDir::new();
        let a = DeviceIdentity::load_or_create(&dir.0, &TEST_PROFILE).unwrap();
        std::fs::write(dir.0.join(META_FILE), b"{ torn").unwrap();
        let b = DeviceIdentity::load_or_create(&dir.0, &TEST_PROFILE).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        // and the rebuilt metadata is valid again
        let c = DeviceIdentity::load_or_create(&dir.0, &TEST_PROFILE).unwrap();
        assert_eq!(b.device_id, c.device_id);
    }

    /// Without identity.json the identity counts as incomplete and is
    /// regenerated as a whole (an interrupted first launch)
    #[test]
    fn missing_meta_regenerates() {
        let dir = TempDir::new();
        let a = DeviceIdentity::load_or_create(&dir.0, &TEST_PROFILE).unwrap();
        std::fs::remove_file(dir.0.join(META_FILE)).unwrap();
        let b = DeviceIdentity::load_or_create(&dir.0, &TEST_PROFILE).unwrap();
        assert_ne!(a.fingerprint, b.fingerprint);
    }

    /// A persisted display name wins over the hostname, and None goes back
    /// to following the hostname
    #[test]
    fn display_name_roundtrip() {
        let dir = TempDir::new();
        let a = DeviceIdentity::load_or_create(&dir.0, &TEST_PROFILE).unwrap();
        persist_display_name(&dir.0, Some("Desk Mac")).unwrap();
        let b = DeviceIdentity::load_or_create(&dir.0, &TEST_PROFILE).unwrap();
        assert_eq!(b.display_name, "Desk Mac");
        assert_eq!(a.fingerprint, b.fingerprint);
        persist_display_name(&dir.0, None).unwrap();
        let c = DeviceIdentity::load_or_create(&dir.0, &TEST_PROFILE).unwrap();
        assert_eq!(c.display_name, a.display_name);
    }

    /// The private key is readable by its owner only
    #[cfg(unix)]
    #[test]
    fn private_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _id) = test_identity();
        let mode = std::fs::metadata(dir.0.join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// Peer info without the optional fields (an older sender) still parses,
    /// and unset optional fields are not serialized
    #[test]
    fn peer_info_optional_fields_are_backward_compatible() {
        let legacy = r#"{"device_id":"d","name":"n","fingerprint":"f","platform":"macos"}"#;
        let info: PeerInfo = serde_json::from_str(legacy).unwrap();
        assert_eq!(info.os_version, None);
        assert!(info.props.is_empty());
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("os_version"));
        assert!(!json.contains("props"));
    }

    /// OS version detection produces a non-empty description
    #[test]
    fn os_version_is_detected() {
        assert!(!os_version().trim().is_empty());
    }
}
