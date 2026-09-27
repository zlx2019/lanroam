//! Link diagnostics: round-trip measurement over the reliable control stream
//! and over unreliable datagrams, plus the responder that answers both.
//!
//! Datagrams are the path pointer motion will take (M1), so their latency
//! and loss on real networks are what this module is mainly for.

use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::protocol::{Control, Datagram, FRAMING};
use crate::transport::{Link, TransportError};

/// How long to wait for one pong on the reliable path
const PONG_TIMEOUT: Duration = Duration::from_secs(5);

/// Microseconds on a process-local monotonic clock; only ever compared with
/// itself, so the epoch does not matter
fn now_us() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Round-trip time elapsed since `sent_us`
fn rtt_since(sent_us: u64) -> Duration {
    Duration::from_micros(now_us().saturating_sub(sent_us))
}

/// Round-trip samples of one measurement
#[derive(Debug, Clone, Default)]
pub struct RttStats {
    /// Probes sent
    pub sent: u32,
    /// Round trips of the answered probes, in arrival order
    pub samples: Vec<Duration>,
}

impl RttStats {
    /// Share of probes that went unanswered (0.0 ..= 1.0)
    pub fn loss(&self) -> f64 {
        if self.sent == 0 {
            return 0.0;
        }
        1.0 - self.samples.len() as f64 / f64::from(self.sent)
    }

    /// Fastest round trip
    pub fn min(&self) -> Option<Duration> {
        self.samples.iter().min().copied()
    }

    /// Slowest round trip
    pub fn max(&self) -> Option<Duration> {
        self.samples.iter().max().copied()
    }

    /// Mean round trip
    pub fn mean(&self) -> Option<Duration> {
        let total: Duration = self.samples.iter().sum();
        let count = u32::try_from(self.samples.len()).ok().filter(|n| *n > 0)?;
        Some(total / count)
    }

    /// Round trip at percentile `p` (0.0 ..= 100.0), nearest-rank method
    pub fn percentile(&self, p: f64) -> Option<Duration> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let rank = (p.clamp(0.0, 100.0) / 100.0 * sorted.len() as f64).ceil() as usize;
        sorted.get(rank.saturating_sub(1)).copied()
    }
}

/// Measure round trips over the control stream: one ping at a time, each
/// waiting for its pong
pub async fn ping_stream(link: &mut Link, count: u32) -> Result<RttStats, TransportError> {
    let mut stats = RttStats::default();
    for seq in 0..count {
        let sent_us = now_us();
        link.send(&Control::Ping { seq, sent_us }).await?;
        stats.sent += 1;
        loop {
            // A timeout ends the measurement, so the half-read frame it may
            // abandon is never read again
            let reply = tokio::time::timeout(PONG_TIMEOUT, link.recv())
                .await
                .map_err(|_| TransportError::Timeout("pong"))??;
            match reply {
                Control::Pong { seq: got, sent_us } if got == seq => {
                    stats.samples.push(rtt_since(sent_us));
                    break;
                }
                other => tracing::debug!(kind = other.kind(), "ignoring message while pinging"),
            }
        }
    }
    Ok(stats)
}

/// Measure round trips over datagrams: `count` pings spaced by `interval`,
/// then up to `grace` for the last answers
///
/// Lost datagrams are expected and show up as [`RttStats::loss`]. This must
/// be the only reader of the link's datagrams while it runs.
pub async fn ping_datagrams(
    link: &Link,
    count: u32,
    interval: Duration,
    grace: Duration,
) -> Result<RttStats, TransportError> {
    let conn = link.connection();
    let mut stats = RttStats::default();
    let mut answered = HashSet::new();
    let mut tick = tokio::time::interval(interval);
    let mut deadline: Option<tokio::time::Instant> = None;
    while answered.len() < count as usize {
        let grace_over = async {
            match deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = tick.tick(), if stats.sent < count => {
                let ping = Datagram::Ping { seq: stats.sent, sent_us: now_us() };
                conn.send_datagram(ping.encode())?;
                stats.sent += 1;
                if stats.sent == count {
                    deadline = Some(tokio::time::Instant::now() + grace);
                }
            }
            received = conn.read_datagram() => {
                if let Some(Datagram::Pong { seq, sent_us }) = Datagram::decode(&received?)
                    && seq < stats.sent
                    && answered.insert(seq)
                {
                    stats.samples.push(rtt_since(sent_us));
                }
            }
            () = grace_over => break,
        }
    }
    Ok(stats)
}

/// Answer pings on both paths until the link closes
///
/// Datagrams are echoed from their own task and the control stream is read
/// sequentially, so no read is ever raced and abandoned halfway.
pub async fn respond(link: Link) {
    let (remote, conn, mut tx, mut rx) = link.into_parts();
    let echo = conn.clone();
    let datagrams = tokio::spawn(async move {
        while let Ok(bytes) = echo.read_datagram().await {
            if let Some(Datagram::Ping { seq, sent_us }) = Datagram::decode(&bytes) {
                // Losing an echo is fine; the pinger counts it as loss
                let _ = echo.send_datagram(Datagram::Pong { seq, sent_us }.encode());
            }
        }
    });
    loop {
        match FRAMING.read::<_, Control>(&mut rx).await {
            Ok(Control::Ping { seq, sent_us }) => {
                if FRAMING
                    .write(&mut tx, &Control::Pong { seq, sent_us })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Ok(other) => {
                tracing::debug!(kind = other.kind(), from = %remote.name, "ignoring control message");
            }
            Err(_) => break,
        }
    }
    datagrams.abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Statistics over known samples
    #[test]
    fn stats_math() {
        let stats = RttStats {
            sent: 5,
            samples: [4, 1, 3, 2]
                .into_iter()
                .map(Duration::from_millis)
                .collect(),
        };
        assert_eq!(stats.min(), Some(Duration::from_millis(1)));
        assert_eq!(stats.max(), Some(Duration::from_millis(4)));
        assert_eq!(stats.mean(), Some(Duration::from_micros(2500)));
        assert_eq!(stats.percentile(50.0), Some(Duration::from_millis(2)));
        assert_eq!(stats.percentile(99.0), Some(Duration::from_millis(4)));
        assert_eq!(stats.percentile(0.0), Some(Duration::from_millis(1)));
        assert!((stats.loss() - 0.2).abs() < 1e-9);
    }

    /// Pings on both paths are answered by the responder over a real
    /// loopback link
    #[tokio::test]
    async fn ping_both_paths_against_responder() {
        let (_a, _b, mut client, server) = crate::test_util::TestNode::link_pair().await;
        let responder = tokio::spawn(respond(server));

        let stream = ping_stream(&mut client, 5).await.unwrap();
        assert_eq!(stream.sent, 5);
        assert_eq!(stream.samples.len(), 5);

        let datagrams = ping_datagrams(
            &client,
            20,
            Duration::from_millis(5),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(datagrams.sent, 20);
        // Loopback may still drop the odd datagram, but most must come back
        assert!(datagrams.samples.len() >= 15, "{datagrams:?}");

        // Closing the link ends the responder
        client.close();
        tokio::time::timeout(Duration::from_secs(5), responder)
            .await
            .unwrap()
            .unwrap();
    }

    /// Empty measurements have no statistics and no loss
    #[test]
    fn empty_stats() {
        let stats = RttStats::default();
        assert_eq!(stats.min(), None);
        assert_eq!(stats.mean(), None);
        assert_eq!(stats.percentile(50.0), None);
        assert_eq!(stats.loss(), 0.0);
    }
}
