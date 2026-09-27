//! Framing: length-prefixed JSON messages over any async byte stream.
//!
//! Frame format: a 4-byte big-endian length prefix + a JSON body. The
//! message type is the app's own; this module only cares that it is serde
//! (de)serializable. Each app fixes its size limit once:
//!
//! ```
//! use lan_kit::frame::Framing;
//! const FRAMING: Framing = Framing::new(64 * 1024);
//! ```

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Framing layer errors
#[derive(Debug, Error)]
pub enum FrameError {
    /// The stream ended cleanly on a frame boundary: the peer is done
    #[error("stream closed")]
    Closed,
    /// Underlying I/O failure, including a stream that ends mid-frame
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The frame exceeds the configured size limit
    #[error("frame of {len} bytes exceeds the {max}-byte limit")]
    TooLarge {
        /// Declared or encoded length
        len: u64,
        /// Configured limit
        max: u32,
    },
    /// Encoding or decoding the JSON body failed
    #[error("frame codec error: {0}")]
    Codec(#[from] serde_json::Error),
}

/// Frame codec with a fixed size limit; the limit guards against a peer
/// announcing a huge frame to exhaust memory
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Framing {
    /// Maximum body length in bytes
    max_len: u32,
}

impl Framing {
    /// A codec that accepts bodies of up to `max_len` bytes
    pub const fn new(max_len: u32) -> Self {
        Self { max_len }
    }

    /// Maximum body length in bytes
    pub const fn max_len(&self) -> u32 {
        self.max_len
    }

    /// Encode one frame (length prefix + body) into contiguous bytes
    ///
    /// Useful when the same frame goes to several peers: encode once, then
    /// send the bytes with [`write_raw`].
    pub fn encode<T: Serialize>(&self, msg: &T) -> Result<Vec<u8>, FrameError> {
        let body = serde_json::to_vec(msg)?;
        let len = u32::try_from(body.len())
            .ok()
            .filter(|len| *len <= self.max_len)
            .ok_or(FrameError::TooLarge {
                len: body.len() as u64,
                max: self.max_len,
            })?;
        let mut frame = Vec::with_capacity(4 + body.len());
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(&body);
        Ok(frame)
    }

    /// Encode and write one frame, then flush
    pub async fn write<W, T>(&self, w: &mut W, msg: &T) -> Result<(), FrameError>
    where
        W: AsyncWrite + Unpin,
        T: Serialize,
    {
        let frame = self.encode(msg)?;
        write_raw(w, &frame).await
    }

    /// Read and decode one frame
    ///
    /// Returns [`FrameError::Closed`] when the stream ends exactly on a frame
    /// boundary, so callers can tell a finished peer from a broken one. An
    /// oversized frame is refused before its body is read.
    pub async fn read<R, T>(&self, r: &mut R) -> Result<T, FrameError>
    where
        R: AsyncRead + Unpin,
        T: DeserializeOwned,
    {
        let mut len_buf = [0u8; 4];
        let mut filled = 0;
        while filled < len_buf.len() {
            let n = r.read(&mut len_buf[filled..]).await?;
            if n == 0 {
                return Err(if filled == 0 {
                    FrameError::Closed
                } else {
                    FrameError::Io(std::io::ErrorKind::UnexpectedEof.into())
                });
            }
            filled += n;
        }
        let len = u32::from_be_bytes(len_buf);
        if len > self.max_len {
            return Err(FrameError::TooLarge {
                len: u64::from(len),
                max: self.max_len,
            });
        }
        let mut body = vec![0u8; len as usize];
        r.read_exact(&mut body).await?;
        Ok(serde_json::from_slice(&body)?)
    }
}

/// Write a frame pre-encoded by [`Framing::encode`], then flush
pub async fn write_raw<W: AsyncWrite + Unpin>(w: &mut W, frame: &[u8]) -> Result<(), FrameError> {
    w.write_all(frame).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    /// Test message type
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum Msg {
        Hello { name: String },
        Data { text: String },
        Bye,
    }

    /// Codec with a small limit to exercise the size checks
    const FRAMING: Framing = Framing::new(1024);

    /// Every message comes back unchanged through a pipe, text byte for byte
    #[tokio::test]
    async fn roundtrip() {
        let samples = vec![
            Msg::Hello {
                name: "desk".into(),
            },
            Msg::Data {
                text: "  你好\n\t emoji🚀 \0 tail  ".into(),
            },
            Msg::Bye,
        ];
        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        for msg in &samples {
            FRAMING.write(&mut a, msg).await.unwrap();
            let got: Msg = FRAMING.read(&mut b).await.unwrap();
            assert_eq!(&got, msg);
        }
    }

    /// A pre-encoded frame is byte for byte what `write` produces
    #[tokio::test]
    async fn raw_frame_matches_write() {
        let msg = Msg::Data { text: "x".into() };
        let frame = FRAMING.encode(&msg).unwrap();
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_raw(&mut a, &frame).await.unwrap();
        let got: Msg = FRAMING.read(&mut b).await.unwrap();
        assert_eq!(got, msg);
    }

    /// Encoding refuses a message over the limit
    #[test]
    fn encode_rejects_oversized() {
        let msg = Msg::Data {
            text: "x".repeat(2048),
        };
        assert!(matches!(
            FRAMING.encode(&msg),
            Err(FrameError::TooLarge { .. })
        ));
    }

    /// Reading refuses an oversized frame before touching its body
    #[tokio::test]
    async fn read_rejects_oversized() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&(FRAMING.max_len() + 1).to_be_bytes())
            .await
            .unwrap();
        assert!(matches!(
            FRAMING.read::<_, Msg>(&mut b).await,
            Err(FrameError::TooLarge { .. })
        ));
    }

    /// A clean end of stream on a frame boundary is `Closed`; one inside the
    /// length prefix is an I/O error
    #[tokio::test]
    async fn eof_is_distinguished() {
        let (a, mut b) = tokio::io::duplex(1024);
        drop(a);
        assert!(matches!(
            FRAMING.read::<_, Msg>(&mut b).await,
            Err(FrameError::Closed)
        ));

        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&[0, 0]).await.unwrap();
        drop(a);
        assert!(matches!(
            FRAMING.read::<_, Msg>(&mut b).await,
            Err(FrameError::Io(_))
        ));
    }
}
