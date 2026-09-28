//! Images on the macOS pasteboard, decoded by the system and converted to
//! sRGB.
//!
//! A screenshot on a Mac carries its display's color profile (Display P3
//! on most). Its pixels would look washed out where they are shown as sRGB,
//! which is how Windows and most of its apps show them; converting here,
//! the one place that still knows the profile, keeps the colors right. The
//! system's decoder is also far faster than decoding in Rust, above all in
//! debug builds.

use objc2_app_kit::{NSBitmapImageRep, NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeTIFF};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, kCGColorSpaceSRGB,
};
use objc2_foundation::{NSArray, NSData};

use crate::{Image, MAX_PIXEL_BYTES};

/// The pasteboard's image, if it holds one (PNG or TIFF, or anything the
/// system converts to them), in sRGB
#[allow(unsafe_code)] // extern statics of AppKit
pub(crate) fn read_image() -> Option<Image> {
    // A pool around the autoreleased objects: this runs on a worker
    // thread, which has none of its own
    objc2::rc::autoreleasepool(|_| {
        let pasteboard = NSPasteboard::generalPasteboard();
        // SAFETY: constant strings AppKit defines
        let types = unsafe { [NSPasteboardTypePNG, NSPasteboardTypeTIFF] };
        let kind = pasteboard.availableTypeFromArray(&NSArray::from_slice(&types))?;
        let data = pasteboard.dataForType(&kind)?;
        decode(&data)
    })
}

/// Encoded image `data` (PNG, TIFF, ...) as sRGB pixels
fn decode(data: &NSData) -> Option<Image> {
    let image = NSBitmapImageRep::imageRepWithData(data)?.CGImage()?;
    to_srgb(&image)
}

/// The pixels of `image` in sRGB, straight (not premultiplied) RGBA
#[allow(unsafe_code)] // a bitmap context drawing into our buffer
fn to_srgb(image: &CGImage) -> Option<Image> {
    let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let bytes = width.checked_mul(height)?.checked_mul(4)?;
    if bytes == 0 || bytes > MAX_PIXEL_BYTES {
        return None;
    }
    // SAFETY: a constant string CoreGraphics defines
    let srgb = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    let mut rgba = vec![0u8; bytes];
    // RGBA byte by byte; contexts draw premultiplied only
    let info = CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0;
    // SAFETY: `rgba` holds `height` rows of `width × 4` bytes, and outlives
    // the context, which is dropped below before `rgba` is touched again
    let context = unsafe {
        CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            width,
            height,
            8,
            width * 4,
            Some(&srgb),
            info,
        )
    }?;
    let rect = CGRect::new(CGPoint::ZERO, CGSize::new(width as f64, height as f64));
    CGContext::draw_image(Some(&context), rect, Some(image));
    drop(context);
    unpremultiply(&mut rgba);
    Some(Image {
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        rgba,
    })
}

/// Undo premultiplied alpha, pixel by pixel (opaque ones stay as they are)
fn unpremultiply(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            let straight = (u32::from(*channel) * 255 + alpha / 2) / alpha;
            *channel = straight.min(255) as u8;
        }
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use objc2_core_foundation::{CFRetained, CFString};
    use objc2_core_graphics::{CGBitmapContextCreateImage, kCGColorSpaceDisplayP3};

    use super::*;
    use crate::Content;

    #[test]
    fn our_png_decodes_as_it_was() {
        let original = Image {
            width: 3,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, //
                12, 34, 56, 255, 200, 100, 50, 255, 1, 2, 3, 255,
            ],
        };
        let png = Content::Image(original.clone()).encode().unwrap();
        let decoded = decode(&NSData::with_bytes(&png)).unwrap();
        assert_eq!(decoded, original);
    }

    /// A 2×1 image of the given pixels (RGBA, premultiplied) in `space`
    fn image(space: &CFString, pixels: [u8; 8]) -> CFRetained<CGImage> {
        let space = CGColorSpace::with_name(Some(space)).unwrap();
        let mut data = pixels;
        let info = CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0;
        let context = unsafe {
            CGBitmapContextCreate(data.as_mut_ptr().cast(), 2, 1, 8, 8, Some(&space), info)
        }
        .unwrap();
        CGBitmapContextCreateImage(Some(&context)).unwrap()
    }

    #[test]
    fn srgb_stays_as_it_is() {
        // Opaque, and half transparent (premultiplied: 100 of 200 is 50)
        let pixels = [10, 200, 30, 255, 50, 100, 0, 128];
        let converted = to_srgb(&image(unsafe { kCGColorSpaceSRGB }, pixels)).unwrap();
        assert_eq!((converted.width, converted.height), (2, 1));
        assert_eq!(converted.rgba[..4], [10, 200, 30, 255]);
        let half = &converted.rgba[4..];
        assert_eq!(half[3], 128);
        for (got, want) in half[..3].iter().zip([100, 199, 0]) {
            assert!(got.abs_diff(want) <= 2, "{half:?}");
        }
    }

    #[test]
    fn display_p3_becomes_srgb() {
        // A muted P3 red is a stronger red in sRGB numbers
        let pixels = [204, 51, 51, 255, 204, 51, 51, 255];
        let converted = to_srgb(&image(unsafe { kCGColorSpaceDisplayP3 }, pixels)).unwrap();
        let [r, g, b, a] = converted.rgba[..4] else {
            unreachable!()
        };
        assert!(r > 210 && g < 45 && b < 51 && a == 255, "{r} {g} {b} {a}");
    }
}
