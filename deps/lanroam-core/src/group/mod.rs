//! Desk group: the devices that trust each other, and the document they
//! share.
//!
//! The document holds one record per device, keyed by certificate
//! fingerprint. Members exchange whole documents and merge them; the merge
//! is commutative, associative and idempotent, so every member converges on
//! the same document whatever order the copies arrive in:
//!
//! - **Membership** is a register per device ordered by `(epoch, removed)`:
//!   the higher epoch wins, and at equal epochs the removal wins. Kicking
//!   (or leaving) bumps the epoch and sets `removed`, a tombstone an old
//!   copy cannot undo; joining again bumps the epoch once more
//! - **Profile** (name, platform) is written by the device itself only and
//!   ordered by its own revision counter
//!
//! Records are not signed: any member may add or remove devices anyway, so
//! a signature would not stop a hostile member. Trust comes from the
//! transport instead, where only TLS-authenticated current members are heard.

pub mod join;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use lan_kit::PeerInfo;
use serde::{Deserialize, Serialize};

/// File holding the group document in the data directory
const GROUP_FILE: &str = "group.json";

/// Where a device stands in a group
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// A current member
    Member,
    /// Kicked out or left; refused until it joins again
    Removed,
    /// Never part of this group
    Unknown,
}

/// What a device says about itself
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Revision, bumped by the device on every change
    pub rev: u64,
    /// Device ID (UUID v4)
    pub device_id: String,
    /// Display name
    pub name: String,
    /// Platform tag (macos / windows / linux)
    pub platform: String,
}

impl Profile {
    /// Whether this profile already says what `info` says
    fn describes(&self, info: &PeerInfo) -> bool {
        self.device_id == info.device_id && self.name == info.name && self.platform == info.platform
    }
}

/// One device in the group document
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecord {
    /// Membership generation: every admission and removal bumps it
    pub epoch: u64,
    /// Kicked out or left (a tombstone)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
    /// The device's own description
    pub profile: Profile,
}

impl DeviceRecord {
    /// Order of the membership register: the higher epoch wins, and a
    /// removal wins over an admission of the same epoch
    fn membership(&self) -> (u64, bool) {
        (self.epoch, self.removed)
    }

    /// Merge `other` into this record; true if anything changed
    fn merge(&mut self, other: &Self) -> bool {
        let mut changed = false;
        if other.membership() > self.membership() {
            self.epoch = other.epoch;
            self.removed = other.removed;
            changed = true;
        }
        if other.profile.rev > self.profile.rev {
            self.profile = other.profile.clone();
            changed = true;
        }
        changed
    }
}

/// The document a desk group shares
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupDoc {
    /// Group ID (UUID v4); documents of different groups never merge
    pub id: String,
    /// Records by certificate fingerprint, removed devices included
    pub devices: BTreeMap<String, DeviceRecord>,
}

impl GroupDoc {
    /// A new group whose only member is `founder`
    pub fn new(founder: &PeerInfo) -> Self {
        let mut doc = Self {
            id: uuid::Uuid::new_v4().to_string(),
            devices: BTreeMap::new(),
        };
        doc.admit(founder);
        doc
    }

    /// Where the device with `fingerprint` stands
    pub fn standing(&self, fingerprint: &str) -> Standing {
        match self.devices.get(fingerprint) {
            Some(record) if record.removed => Standing::Removed,
            Some(_) => Standing::Member,
            None => Standing::Unknown,
        }
    }

    /// Whether the device with `fingerprint` is a current member
    pub fn is_member(&self, fingerprint: &str) -> bool {
        self.standing(fingerprint) == Standing::Member
    }

    /// Current members: (fingerprint, record)
    pub fn members(&self) -> impl Iterator<Item = (&str, &DeviceRecord)> {
        self.devices
            .iter()
            .filter(|(_, record)| !record.removed)
            .map(|(fp, record)| (fp.as_str(), record))
    }

    /// Admit a device (again); a current member stays as it is
    pub fn admit(&mut self, info: &PeerInfo) {
        match self.devices.get_mut(&info.fingerprint) {
            Some(record) if !record.removed => {}
            Some(record) => {
                record.epoch += 1;
                record.removed = false;
            }
            None => {
                let record = DeviceRecord {
                    epoch: 1,
                    removed: false,
                    profile: Profile {
                        rev: 1,
                        device_id: info.device_id.clone(),
                        name: info.name.clone(),
                        platform: info.platform.clone(),
                    },
                };
                self.devices.insert(info.fingerprint.clone(), record);
            }
        }
    }

    /// Remove a current member (kick, or leave for oneself); false if it is
    /// not one
    pub fn remove(&mut self, fingerprint: &str) -> bool {
        match self.devices.get_mut(fingerprint) {
            Some(record) if !record.removed => {
                record.epoch += 1;
                record.removed = true;
                true
            }
            _ => false,
        }
    }

    /// Bring the record of `info`'s device up to date with it; only the
    /// device itself calls this. False if nothing changed (or it has no
    /// record)
    pub fn update_profile(&mut self, info: &PeerInfo) -> bool {
        let Some(record) = self.devices.get_mut(&info.fingerprint) else {
            return false;
        };
        if record.profile.describes(info) {
            return false;
        }
        record.profile = Profile {
            rev: record.profile.rev + 1,
            device_id: info.device_id.clone(),
            name: info.name.clone(),
            platform: info.platform.clone(),
        };
        true
    }

    /// Merge another copy of this group's document; true if this copy
    /// changed. A document of another group is ignored
    pub fn merge(&mut self, other: &Self) -> bool {
        if other.id != self.id {
            return false;
        }
        let mut changed = false;
        for (fp, theirs) in &other.devices {
            match self.devices.get_mut(fp) {
                Some(ours) => changed |= ours.merge(theirs),
                None => {
                    self.devices.insert(fp.clone(), theirs.clone());
                    changed = true;
                }
            }
        }
        changed
    }
}

/// The group document on disk
#[derive(Debug, Clone)]
pub struct GroupStore {
    /// Path of the document file
    path: PathBuf,
}

impl GroupStore {
    /// The store in the data directory `dir`
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join(GROUP_FILE),
        }
    }

    /// Load the document; `None` when this device is in no group
    ///
    /// A malformed file is moved aside and reads as no group: better than
    /// refusing to start with no way out, and the other members still hold
    /// the document.
    pub fn load(&self) -> std::io::Result<Option<GroupDoc>> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        match serde_json::from_slice(&bytes) {
            Ok(doc) => Ok(Some(doc)),
            Err(e) => {
                let aside = self.path.with_extension("json.bad");
                tracing::warn!(
                    "{} is malformed ({e}), moved to {}; this device is in no group now",
                    self.path.display(),
                    aside.display()
                );
                fs::rename(&self.path, aside)?;
                Ok(None)
            }
        }
    }

    /// Save the document, or delete the file for `None` (no group)
    pub fn save(&self, doc: Option<&GroupDoc>) -> std::io::Result<()> {
        let Some(doc) = doc else {
            return match fs::remove_file(&self.path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        };
        let bytes = serde_json::to_vec_pretty(doc).map_err(std::io::Error::other)?;
        // Temp file + rename: an interrupted overwrite must not tear the file
        let tmp = self.path.with_extension("json.tmp");
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    /// Device info for the tests
    fn info(name: &str) -> PeerInfo {
        PeerInfo {
            device_id: format!("id-{name}"),
            name: name.to_string(),
            fingerprint: format!("fp-{name}"),
            platform: "macos".to_string(),
            os_version: None,
            props: BTreeMap::new(),
        }
    }

    /// Merge `b` into a copy of `a`
    fn merged(a: &GroupDoc, b: &GroupDoc) -> GroupDoc {
        let mut out = a.clone();
        out.merge(b);
        out
    }

    /// A founder is the only member; admitted devices join it
    #[test]
    fn founder_and_admission() {
        let mut doc = GroupDoc::new(&info("a"));
        assert!(doc.is_member("fp-a"));
        assert_eq!(doc.standing("fp-b"), Standing::Unknown);
        doc.admit(&info("b"));
        assert_eq!(doc.members().count(), 2);
        // Admitting a current member changes nothing
        let before = doc.clone();
        doc.admit(&info("b"));
        assert_eq!(doc, before);
    }

    /// A removal beats an old copy that still lists the device, and a
    /// later admission beats the removal
    #[test]
    fn tombstones_win_until_rejoined() {
        let mut a = GroupDoc::new(&info("a"));
        a.admit(&info("b"));
        let stale = a.clone();

        assert!(a.remove("fp-b"));
        assert!(!a.remove("fp-b"), "only current members can be removed");
        assert!(!a.merge(&stale));
        assert_eq!(a.standing("fp-b"), Standing::Removed);
        assert_eq!(merged(&stale, &a).standing("fp-b"), Standing::Removed);

        let removed = a.clone();
        a.admit(&info("b"));
        assert!(a.is_member("fp-b"));
        assert!(!a.merge(&removed));
        assert!(a.is_member("fp-b"));
    }

    /// Concurrent removal and admission at the same epoch: the removal wins
    /// on both sides
    #[test]
    fn removal_wins_ties() {
        let mut base = GroupDoc::new(&info("a"));
        base.admit(&info("b"));
        base.remove("fp-b");
        let (mut x, mut y) = (base.clone(), base.clone());
        x.admit(&info("b")); // epoch 3, member
        y.devices.get_mut("fp-b").unwrap().epoch = 3; // epoch 3, removed
        assert_eq!(merged(&x, &y).standing("fp-b"), Standing::Removed);
        assert_eq!(merged(&y, &x).standing("fp-b"), Standing::Removed);
    }

    /// Merging converges whatever the order, and repeating it changes
    /// nothing
    #[test]
    fn merge_converges() {
        let mut a = GroupDoc::new(&info("a"));
        a.admit(&info("b"));
        let mut b = a.clone();
        let mut c = a.clone();
        a.admit(&info("c"));
        b.remove("fp-a");
        let mut renamed = info("b");
        renamed.name = "Desk".into();
        c.update_profile(&renamed);

        let abc = merged(&merged(&a, &b), &c);
        let cba = merged(&merged(&c, &b), &a);
        assert_eq!(abc, cba);
        assert_eq!(merged(&abc, &abc), abc);
        assert_eq!(abc.standing("fp-a"), Standing::Removed);
        assert!(abc.is_member("fp-c"));
        assert_eq!(abc.devices["fp-b"].profile.name, "Desk");
    }

    /// Profiles only change when the device says something new
    #[test]
    fn profile_updates() {
        let mut doc = GroupDoc::new(&info("a"));
        assert!(!doc.update_profile(&info("a")));
        assert!(!doc.update_profile(&info("stranger")));
        let mut renamed = info("a");
        renamed.name = "Studio".into();
        assert!(doc.update_profile(&renamed));
        assert_eq!(doc.devices["fp-a"].profile.rev, 2);
    }

    /// Another group's document never mixes in
    #[test]
    fn other_groups_are_ignored() {
        let mut a = GroupDoc::new(&info("a"));
        let b = GroupDoc::new(&info("b"));
        assert!(!a.merge(&b));
        assert_eq!(a.standing("fp-b"), Standing::Unknown);
    }

    /// The store round-trips, deletes on `None`, and sets a malformed file
    /// aside
    #[test]
    fn store_roundtrip() {
        let dir = TempDir::new();
        let store = GroupStore::new(&dir.0);
        assert_eq!(store.load().unwrap(), None);

        let doc = GroupDoc::new(&info("a"));
        store.save(Some(&doc)).unwrap();
        assert_eq!(store.load().unwrap(), Some(doc));
        store.save(None).unwrap();
        assert_eq!(store.load().unwrap(), None);
        store.save(None).unwrap();

        fs::write(dir.0.join(GROUP_FILE), b"{not json").unwrap();
        assert_eq!(store.load().unwrap(), None);
        assert!(dir.0.join("group.json.bad").exists());
    }
}
