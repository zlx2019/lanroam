//! Joining a desk group with a PIN.
//!
//! A device joins through a *sponsor*: a member of the group, or an
//! ungrouped device, which then founds a group with it. The sponsor shows a
//! 6-digit PIN, the user types it on the joiner, and both sides run SPAKE2
//! with it on a [`Purpose::Join`](crate::protocol::Purpose) link:
//!
//! ```text
//! sponsor → JoinChallenge { pake, attempts_left }
//! joiner  → JoinAnswer { pake, confirm }
//! sponsor → JoinAccepted { confirm, doc }   the PIN matched
//!         | JoinChallenge { .. }            it did not; the next attempt
//!         | JoinDenied { reason_code }      it did not, and none are left
//! ```
//!
//! Why SPAKE2 rather than sending the PIN: discovery is unauthenticated, so
//! the device the user picked may be an impostor relaying to the real
//! sponsor. Each side feeds SPAKE2 the two certificate fingerprints *it
//! sees* in TLS, so a relay ends up in two exchanges with mismatched
//! identities. Without the PIN it gets one online guess per attempt, and
//! nothing it can brute-force offline. The `confirm` values prove both sides
//! derived the same key, and the joiner checks the sponsor's too: a fake
//! sponsor cannot hand over a group.

use std::time::{Duration, Instant};

use spake2::{Ed25519Group, Identity, Password, Spake2};
use thiserror::Error;

use super::GroupDoc;
use crate::protocol::{Control, join_denied};
use crate::transport::{Link, TransportError};

/// Digits in a PIN
pub const PIN_DIGITS: usize = 6;

/// Wrong answers one PIN survives before it is used up
pub const PIN_ATTEMPTS: u32 = 3;

/// How long a sponsor turns joins away after a PIN was used up
pub const COOLDOWN: Duration = Duration::from_secs(30);

/// How long a sponsor turns joins away after any other failed join, so
/// repeated requests cannot flood it with PINs
const SHORT_COOLDOWN: Duration = Duration::from_secs(3);

/// How long a sponsor waits for an answer: a person reads and types the PIN
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a joiner waits for the first challenge, and for the verdict on
/// an answer (the sponsor replies without human involvement)
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Key derivation context of the joiner's proof
const JOINER_PROOF: &str = "lanroam 2026-09-27 join proof: joiner";

/// Key derivation context of the sponsor's proof
const SPONSOR_PROOF: &str = "lanroam 2026-09-27 join proof: sponsor";

/// Join errors
#[derive(Debug, Error)]
pub enum JoinError {
    /// The link failed
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The sponsor turned the join down (a [`join_denied`] code)
    #[error("the join was turned down: {0}")]
    Denied(String),
    /// The joiner gave a wrong PIN for every attempt (sponsor side)
    #[error("the PIN was wrong {PIN_ATTEMPTS} times")]
    PinUsedUp,
    /// The other side took too long
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    /// The sponsor accepted without proving it knows the PIN: it is likely
    /// an impostor
    #[error("the sponsor could not prove it knows the PIN; it may be an impostor")]
    Unverified,
    /// A message out of place
    #[error("unexpected message: {0}")]
    Unexpected(&'static str),
    /// No randomness for a PIN
    #[error("cannot draw a random PIN: {0}")]
    Random(String),
}

/// Draw a PIN: [`PIN_DIGITS`] uniformly random digits
pub fn new_pin() -> Result<String, JoinError> {
    // The largest multiple of 10^6 that fits a u32; draws at or above it
    // would make low PINs slightly likelier
    const LIMIT: u32 = u32::MAX - u32::MAX % 1_000_000;
    loop {
        let n = getrandom::u32().map_err(|e| JoinError::Random(e.to_string()))?;
        if n < LIMIT {
            return Ok(format!("{:06}", n % 1_000_000));
        }
    }
}

/// A PIN as typed, without spaces or dashes; `None` unless it has exactly
/// [`PIN_DIGITS`] digits
pub fn normalize_pin(typed: &str) -> Option<String> {
    let pin: String = typed
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    (pin.len() == PIN_DIGITS && pin.bytes().all(|b| b.is_ascii_digit())).then_some(pin)
}

/// The sponsor's limits on joins: one at a time, and a pause after a
/// failure
#[derive(Debug, Default)]
pub struct JoinGate {
    /// A join is in progress
    busy: bool,
    /// Joins are turned away until then
    closed_until: Option<Instant>,
}

impl JoinGate {
    /// Let a join begin, or say why not (a [`join_denied`] code)
    pub fn begin(&mut self, now: Instant) -> Result<(), &'static str> {
        if self.busy {
            return Err(join_denied::BUSY);
        }
        if self.closed_until.is_some_and(|until| now < until) {
            return Err(join_denied::COOLDOWN);
        }
        self.busy = true;
        Ok(())
    }

    /// A join is over; failures close the gate for a while, longest when
    /// the PIN was used up
    pub fn end(&mut self, now: Instant, result: &Result<(), JoinError>) {
        self.busy = false;
        let pause = match result {
            Ok(()) => return,
            Err(JoinError::PinUsedUp) => COOLDOWN,
            Err(_) => SHORT_COOLDOWN,
        };
        self.closed_until = Some(now + pause);
    }
}

/// Start one side of SPAKE2 for an attempt: the joiner is side A, the
/// sponsor side B, each naming the fingerprints it sees in TLS
fn start(
    pin: &str,
    joiner_fp: &str,
    sponsor_fp: &str,
    joiner: bool,
) -> (Spake2<Ed25519Group>, Vec<u8>) {
    let password = Password::new(pin.as_bytes());
    let id_a = Identity::new(joiner_fp.as_bytes());
    let id_b = Identity::new(sponsor_fp.as_bytes());
    if joiner {
        Spake2::<Ed25519Group>::start_a(&password, &id_a, &id_b)
    } else {
        Spake2::<Ed25519Group>::start_b(&password, &id_a, &id_b)
    }
}

/// Proof of holding `key`, one per role so a proof cannot be echoed back
fn proof(key: &[u8], context: &str) -> [u8; 32] {
    blake3::derive_key(context, key)
}

/// Whether `received` is the expected proof (constant time)
fn proof_matches(expected: [u8; 32], received: &[u8]) -> bool {
    blake3::Hash::from(expected) == *received
}

/// Receive the next control message within `timeout`
async fn recv_within(
    link: &mut Link,
    timeout: Duration,
    what: &'static str,
) -> Result<Control, JoinError> {
    tokio::time::timeout(timeout, link.recv())
        .await
        .map_err(|_| JoinError::Timeout(what))?
        .map_err(JoinError::from)
}

/// An attempt the sponsor opened (joiner side)
#[derive(Debug, Clone)]
pub struct Challenge {
    /// The sponsor's SPAKE2 message
    pake: Vec<u8>,
    /// Attempts left for this PIN, this one included
    pub attempts_left: u32,
}

/// The sponsor's verdict on an answer (joiner side)
#[derive(Debug)]
pub enum Verdict {
    /// In: the group document, this device included
    Accepted(GroupDoc),
    /// Wrong PIN; the next attempt
    Retry(Challenge),
}

/// Turn a sponsor message into a challenge, a denial or a violation
fn as_challenge(msg: Control) -> Result<Challenge, JoinError> {
    match msg {
        Control::JoinChallenge {
            pake,
            attempts_left,
        } => Ok(Challenge {
            pake,
            attempts_left,
        }),
        Control::JoinDenied { reason_code } => Err(JoinError::Denied(reason_code)),
        other => Err(JoinError::Unexpected(other.kind())),
    }
}

/// Wait for the sponsor's first challenge (joiner side); it arrives once
/// the sponsor shows its PIN, or a denial if it cannot take the join
pub async fn recv_challenge(link: &mut Link) -> Result<Challenge, JoinError> {
    as_challenge(recv_within(link, REPLY_TIMEOUT, "join_challenge").await?)
}

/// Answer `challenge` with `pin` and return the sponsor's verdict (joiner
/// side)
pub async fn answer(
    link: &mut Link,
    own_fp: &str,
    challenge: &Challenge,
    pin: &str,
) -> Result<Verdict, JoinError> {
    let sponsor_fp = link.remote().fingerprint.clone();
    let (state, pake) = start(pin, own_fp, &sponsor_fp, true);
    // A malformed sponsor message fails like a wrong PIN: this side cannot
    // tell the two apart, and the sponsor's verdict decides anyway
    let key = state.finish(&challenge.pake).ok();
    let confirm = key
        .as_deref()
        .map_or_else(Vec::new, |key| proof(key, JOINER_PROOF).to_vec());
    link.send(&Control::JoinAnswer { pake, confirm }).await?;

    match recv_within(link, REPLY_TIMEOUT, "join verdict").await? {
        Control::JoinAccepted { confirm, doc } => {
            let genuine =
                key.is_some_and(|key| proof_matches(proof(&key, SPONSOR_PROOF), &confirm));
            if genuine {
                Ok(Verdict::Accepted(doc))
            } else {
                Err(JoinError::Unverified)
            }
        }
        other => as_challenge(other).map(Verdict::Retry),
    }
}

/// Proof that the joiner knew the PIN, to send along with the document
/// (sponsor side)
#[derive(Debug)]
pub struct Verified {
    /// The sponsor's proof for [`Control::JoinAccepted`]
    proof: [u8; 32],
}

/// Run the PIN attempts until the joiner proves it knows `pin` (sponsor
/// side)
///
/// Denies the join itself when the PIN is used up or the joiner falls
/// silent; the caller then only closes the link.
pub async fn verify(link: &mut Link, own_fp: &str, pin: &str) -> Result<Verified, JoinError> {
    let joiner_fp = link.remote().fingerprint.clone();
    for attempts_left in (1..=PIN_ATTEMPTS).rev() {
        let (state, pake) = start(pin, &joiner_fp, own_fp, false);
        link.send(&Control::JoinChallenge {
            pake,
            attempts_left,
        })
        .await?;
        let (pake, confirm) = match recv_within(link, ANSWER_TIMEOUT, "join_answer").await {
            Ok(Control::JoinAnswer { pake, confirm }) => (pake, confirm),
            Ok(other) => return Err(JoinError::Unexpected(other.kind())),
            Err(e @ JoinError::Timeout(_)) => {
                deny(link, join_denied::TIMEOUT).await?;
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        if let Ok(key) = state.finish(&pake)
            && proof_matches(proof(&key, JOINER_PROOF), &confirm)
        {
            return Ok(Verified {
                proof: proof(&key, SPONSOR_PROOF),
            });
        }
    }
    deny(link, join_denied::WRONG_PIN).await?;
    Err(JoinError::PinUsedUp)
}

/// Let the verified joiner in with the group document (sponsor side)
pub async fn accept(link: &mut Link, verified: Verified, doc: &GroupDoc) -> Result<(), JoinError> {
    link.send(&Control::JoinAccepted {
        confirm: verified.proof.to_vec(),
        doc: doc.clone(),
    })
    .await?;
    Ok(())
}

/// Turn the join down with a [`join_denied`] code (sponsor side)
pub async fn deny(link: &mut Link, reason_code: &str) -> Result<(), JoinError> {
    link.send(&Control::JoinDenied {
        reason_code: reason_code.to_string(),
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use lan_kit::PeerInfo;

    use super::*;
    use crate::protocol::Purpose;
    use crate::test_util::TestNode;

    /// The sponsor side of a whole join: verify, then accept with a fresh
    /// group of the two
    async fn sponsor(mut link: Link, own: PeerInfo, pin: &str) -> Result<(), JoinError> {
        let result = match verify(&mut link, &own.fingerprint, pin).await {
            Ok(verified) => {
                let mut doc = GroupDoc::new(&own);
                doc.admit(link.remote());
                accept(&mut link, verified, &doc).await
            }
            Err(e) => Err(e),
        };
        // Whatever the outcome, the last message must reach the joiner
        link.close_after_flush().await;
        result
    }

    /// The joiner side: answer each challenge with the next PIN of `pins`,
    /// recording the attempts announced
    async fn joiner(
        link: &mut Link,
        own_fp: &str,
        pins: &[&str],
    ) -> (Result<GroupDoc, JoinError>, Vec<u32>) {
        let mut announced = Vec::new();
        let mut challenge = match recv_challenge(link).await {
            Ok(challenge) => challenge,
            Err(e) => return (Err(e), announced),
        };
        for pin in pins {
            announced.push(challenge.attempts_left);
            match answer(link, own_fp, &challenge, pin).await {
                Ok(Verdict::Accepted(doc)) => return (Ok(doc), announced),
                Ok(Verdict::Retry(next)) => challenge = next,
                Err(e) => return (Err(e), announced),
            }
        }
        (Err(JoinError::Timeout("more pins")), announced)
    }

    /// The right PIN gets the joiner in, with the sponsor's group document
    #[tokio::test]
    async fn right_pin_joins() {
        let (a, b, mut joiner_link, sponsor_link) = TestNode::link_pair().await;
        let task = tokio::spawn(sponsor(sponsor_link, b.info.clone(), "123456"));
        let (doc, announced) = joiner(&mut joiner_link, &a.info.fingerprint, &["123456"]).await;
        let doc = doc.unwrap();
        assert!(doc.is_member(&a.info.fingerprint) && doc.is_member(&b.info.fingerprint));
        assert_eq!(announced, [3]);
        task.await.unwrap().unwrap();
    }

    /// Wrong PINs cost attempts; the right one still works while any are
    /// left
    #[tokio::test]
    async fn retries_count_down() {
        let (a, b, mut joiner_link, sponsor_link) = TestNode::link_pair().await;
        let task = tokio::spawn(sponsor(sponsor_link, b.info.clone(), "000042"));
        let pins = ["111111", "222222", "000042"];
        let (doc, announced) = joiner(&mut joiner_link, &a.info.fingerprint, &pins).await;
        assert!(doc.is_ok());
        assert_eq!(announced, [3, 2, 1]);
        task.await.unwrap().unwrap();
    }

    /// Three wrong PINs use the PIN up on both sides
    #[tokio::test]
    async fn wrong_pins_use_it_up() {
        let (a, b, mut joiner_link, sponsor_link) = TestNode::link_pair().await;
        let task = tokio::spawn(sponsor(sponsor_link, b.info.clone(), "654321"));
        let pins = ["111111", "222222", "333333"];
        let (doc, _) = joiner(&mut joiner_link, &a.info.fingerprint, &pins).await;
        assert!(
            matches!(&doc, Err(JoinError::Denied(code)) if code == join_denied::WRONG_PIN),
            "{doc:?}"
        );
        assert!(matches!(task.await.unwrap(), Err(JoinError::PinUsedUp)));
    }

    /// A relay between the joiner and the real sponsor gets nowhere, even
    /// though the user typed the right PIN: the two exchanges name different
    /// fingerprints, so the sponsor sees a wrong answer
    #[tokio::test]
    async fn relay_cannot_join() {
        // joiner → relay (believed to be the sponsor) → sponsor
        let (joiner_node, relay, mut joiner_link, mut relay_in) = TestNode::link_pair().await;
        let sponsor_node = TestNode::new();
        let accepting = sponsor_node.accept_one();
        let mut relay_out = relay
            .transport
            .connect(&sponsor_node.as_peer(), &relay.info, Purpose::Join)
            .await
            .unwrap();
        let sponsor_link = accepting.await.unwrap().unwrap();
        let task = tokio::spawn(sponsor(sponsor_link, sponsor_node.info.clone(), "271828"));

        // The relay forwards every message untouched, both ways
        let relaying = tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = relay_out.recv() => match msg {
                        Ok(msg) => { let _ = relay_in.send(&msg).await; }
                        Err(_) => break,
                    },
                    msg = relay_in.recv() => match msg {
                        Ok(msg) => { let _ = relay_out.send(&msg).await; }
                        Err(_) => break,
                    },
                }
            }
        });

        let challenge = recv_challenge(&mut joiner_link).await.unwrap();
        let verdict = answer(
            &mut joiner_link,
            &joiner_node.info.fingerprint,
            &challenge,
            "271828",
        )
        .await
        .unwrap();
        assert!(
            matches!(&verdict, Verdict::Retry(next) if next.attempts_left == 2),
            "the sponsor must reject the relayed answer: {verdict:?}"
        );
        relaying.abort();
        drop(task);
    }

    /// A sponsor that does not know the PIN cannot hand over a group
    #[tokio::test]
    async fn fake_sponsor_is_caught() {
        let (a, b, mut joiner_link, mut fake) = TestNode::link_pair().await;
        let (joiner_fp, fake_info) = (a.info.fingerprint.clone(), b.info.clone());
        let task = tokio::spawn(async move {
            // A valid-looking challenge for a PIN the fake does not share
            let (_, pake) = start("999999", &joiner_fp, &fake_info.fingerprint, false);
            fake.send(&Control::JoinChallenge {
                pake,
                attempts_left: 3,
            })
            .await
            .unwrap();
            let _ = fake.recv().await.unwrap();
            fake.send(&Control::JoinAccepted {
                confirm: vec![0; 32],
                doc: GroupDoc::new(&fake_info),
            })
            .await
            .unwrap();
            fake
        });
        let challenge = recv_challenge(&mut joiner_link).await.unwrap();
        let verdict = answer(&mut joiner_link, &a.info.fingerprint, &challenge, "123123").await;
        assert!(matches!(verdict, Err(JoinError::Unverified)), "{verdict:?}");
        drop(task.await.unwrap());
    }

    /// PINs are six uniformly drawn digits, and typing them is forgiving
    #[test]
    fn pins() {
        for _ in 0..100 {
            let pin = new_pin().unwrap();
            assert_eq!(normalize_pin(&pin), Some(pin));
        }
        assert_eq!(normalize_pin(" 123 456 "), Some("123456".into()));
        assert_eq!(normalize_pin("123-456"), Some("123456".into()));
        assert_eq!(normalize_pin("12345"), None);
        assert_eq!(normalize_pin("12345a"), None);
        assert_eq!(normalize_pin("١٢٣٤٥٦"), None);
    }

    /// One join at a time; failures pause the gate, a used-up PIN longest
    #[test]
    fn gate() {
        let t0 = Instant::now();
        let mut gate = JoinGate::default();
        gate.begin(t0).unwrap();
        assert_eq!(gate.begin(t0), Err(join_denied::BUSY));
        gate.end(t0, &Ok(()));
        gate.begin(t0).unwrap();
        gate.end(t0, &Err(JoinError::Timeout("x")));
        assert_eq!(gate.begin(t0), Err(join_denied::COOLDOWN));
        gate.begin(t0 + SHORT_COOLDOWN).unwrap();
        gate.end(t0, &Err(JoinError::PinUsedUp));
        assert_eq!(gate.begin(t0 + SHORT_COOLDOWN), Err(join_denied::COOLDOWN));
        gate.begin(t0 + COOLDOWN).unwrap();
    }
}
