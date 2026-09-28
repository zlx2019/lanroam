//! Clipboard content as it travels: the kinds, the hash devices compare,
//! and the bytes on the wire.

use std::io::Cursor;

use serde::{Deserialize, Serialize};

use crate::ClipboardError;

/// Most bytes one hand-over carries on the wire (text as UTF-8, an image as
/// PNG)
pub const MAX_BYTES: usize = 64 * 1024 * 1024;

/// Largest image, in bytes of RGBA pixels (8192 × 8192)
pub const MAX_PIXEL_BYTES: usize = 256 * 1024 * 1024;

/// What kind of content
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Plain text
    Text,
    /// An image
    Image,
}

/// An image as pixels
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// RGBA, 8 bits per channel, row by row (`width × height × 4` bytes)
    pub rgba: Vec<u8>,
}

/// What a clipboard holds
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// Plain text, byte for byte
    Text(String),
    /// An image
    Image(Image),
}

impl Content {
    /// What kind it is
    pub fn kind(&self) -> Kind {
        match self {
            Self::Text(_) => Kind::Text,
            Self::Image(_) => Kind::Image,
        }
    }

    /// Its size before encoding: bytes of text, or of RGBA pixels
    pub fn size(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Image(image) => image.rgba.len(),
        }
    }

    /// BLAKE3 of the content, in hex: equal hashes, same content. Text is
    /// hashed as its bytes, an image as its size and pixels (never as an
    /// encoding, which differs from machine to machine); a kind prefix
    /// keeps the two apart
    pub fn hash(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        match self {
            Self::Text(text) => {
                hasher.update(b"t:");
                hasher.update(text.as_bytes());
            }
            Self::Image(image) => {
                hasher.update(b"i:");
                hasher.update(&image.width.to_le_bytes());
                hasher.update(&image.height.to_le_bytes());
                hasher.update(&image.rgba);
            }
        }
        hasher.finalize().to_hex().to_string()
    }

    /// The bytes that travel: text as UTF-8, an image as PNG
    pub fn encode(&self) -> Result<Vec<u8>, ClipboardError> {
        let bytes = match self {
            Self::Text(text) => text.as_bytes().to_vec(),
            Self::Image(image) => encode_png(image)?,
        };
        if bytes.len() > MAX_BYTES {
            return Err(ClipboardError::TooLarge(bytes.len()));
        }
        Ok(bytes)
    }

    /// Content of `kind` from the bytes that travelled
    pub fn decode(kind: Kind, bytes: Vec<u8>) -> Result<Self, ClipboardError> {
        if bytes.len() > MAX_BYTES {
            return Err(ClipboardError::TooLarge(bytes.len()));
        }
        match kind {
            Kind::Text => String::from_utf8(bytes)
                .map(Self::Text)
                .map_err(|_| ClipboardError::Text),
            Kind::Image => decode_png(&bytes).map(Self::Image),
        }
    }
}

/// An image as PNG, compressed fast: a screenshot has to be on its way
/// while the pointer crosses. Marked sRGB, which its pixels are (macOS
/// converts them, see `pasteboard`; Windows has them so)
fn encode_png(image: &Image) -> Result<Vec<u8>, ClipboardError> {
    let error = |e: png::EncodingError| ClipboardError::Image(e.to_string());
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(error)?;
    writer.write_image_data(&image.rgba).map_err(error)?;
    writer.finish().map_err(error)?;
    Ok(out)
}

/// An image from PNG, as [`encode_png`] writes it (RGBA, 8 bits), no larger
/// than [`MAX_PIXEL_BYTES`]
fn decode_png(bytes: &[u8]) -> Result<Image, ClipboardError> {
    let error = |e: png::DecodingError| ClipboardError::Image(e.to_string());
    let limits = png::Limits {
        bytes: MAX_PIXEL_BYTES,
    };
    let mut reader = png::Decoder::new_with_limits(Cursor::new(bytes), limits)
        .read_info()
        .map_err(error)?;
    let size = reader
        .output_buffer_size()
        .ok_or(ClipboardError::TooLarge(usize::MAX))?;
    if size > MAX_PIXEL_BYTES {
        return Err(ClipboardError::TooLarge(size));
    }
    let mut rgba = vec![0; size];
    let info = reader.next_frame(&mut rgba).map_err(error)?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(ClipboardError::Image(format!(
            "unexpected pixel format {:?} {:?}",
            info.color_type, info.bit_depth
        )));
    }
    rgba.truncate(info.buffer_size());
    Ok(Image {
        width: info.width,
        height: info.height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3×2 image with some transparency
    fn image() -> Image {
        let rgba = (0..3 * 2 * 4).map(|i| (i * 11) as u8).collect();
        Image {
            width: 3,
            height: 2,
            rgba,
        }
    }

    #[test]
    fn hash_tells_contents_apart() {
        let a = Content::Text("hello".into());
        assert_eq!(a.hash(), Content::Text("hello".into()).hash());
        assert_ne!(a.hash(), Content::Text("hello ".into()).hash());

        // Same pixels, other shape: not the same image
        let wide = Content::Image(image());
        let tall = Content::Image(Image {
            width: 2,
            height: 3,
            ..image()
        });
        assert_ne!(wide.hash(), tall.hash());
    }

    #[test]
    fn text_travels_byte_for_byte() {
        let text = Content::Text("  tabs\tand\r\nlines, 中文 ✓ ".into());
        let bytes = text.encode().unwrap();
        assert_eq!(Content::decode(Kind::Text, bytes).unwrap(), text);
    }

    #[test]
    fn image_travels_as_png() {
        let original = Content::Image(image());
        let bytes = original.encode().unwrap();
        assert!(bytes.starts_with(b"\x89PNG"));
        let decoded = Content::decode(Kind::Image, bytes).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(decoded.hash(), original.hash());
    }

    #[test]
    fn broken_bytes_are_refused() {
        assert!(Content::decode(Kind::Image, b"not a png".to_vec()).is_err());
        assert!(Content::decode(Kind::Text, vec![0xff, 0xfe]).is_err());
    }
}
