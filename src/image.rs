//! JPEG resize and re-encode for the screenshot tool. Pure CPU work, no I/O.

use std::io::Cursor;

use image::{ImageFormat, ImageReader, imageops::FilterType};
use tracing::debug;

use crate::error::{Error, Result};

/// Decode a JPEG, optionally resize (Lanczos3) to fit `max_w`/`max_h` while
/// preserving aspect ratio, then re-encode at `quality` (1..100). `max_*` of 0
/// means no limit on that axis — useful when only the other axis is bounded.
pub fn process_image(jpeg: &[u8], max_w: u32, max_h: u32, quality: u8) -> Result<Vec<u8>> {
    let img = ImageReader::with_format(Cursor::new(jpeg), ImageFormat::Jpeg)
        .decode()
        .map_err(|e| Error::InvalidArgument(format!("jpeg decode failed: {e}")))?;

    let (w, h) = (img.width(), img.height());
    let (new_w, new_h) = fit_within(w, h, max_w, max_h);

    let resized = if (new_w, new_h) != (w, h) {
        debug!(from_w = w, from_h = h, new_w, new_h, "resizing screenshot");
        img.resize_exact(new_w, new_h, FilterType::Lanczos3)
    } else {
        img
    };

    let mut out = Vec::with_capacity(jpeg.len());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    resized
        .write_with_encoder(encoder)
        .map_err(|e| Error::InvalidArgument(format!("jpeg encode failed: {e}")))?;
    Ok(out)
}

/// Compute the largest `(w, h) ≤ (max_w, max_h)` preserving the aspect of
/// `w:h`. A `max_*` of 0 means "no limit on that axis".
fn fit_within(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let (mut nw, mut nh) = (w, h);
    if max_w != 0 && nw > max_w {
        let r = max_w as f64 / nw as f64;
        nw = max_w;
        nh = (nh as f64 * r).round() as u32;
    }
    if max_h != 0 && nh > max_h {
        let r = max_h as f64 / nh as f64;
        nh = max_h;
        nw = (nw as f64 * r).round() as u32;
    }
    (nw.max(1), nh.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_within_axes_respected() {
        // Both axes within limits — no resize.
        assert_eq!(fit_within(800, 600, 1920, 1080), (800, 600));
        // Width exceeds; height scales down proportionally.
        assert_eq!(fit_within(3840, 2160, 1920, 0), (1920, 1080));
        // Height exceeds; width scales down proportionally.
        assert_eq!(fit_within(3840, 2160, 0, 1080), (1920, 1080));
        // Both exceed; the tighter constraint wins.
        assert_eq!(fit_within(3840, 2160, 1920, 1080), (1920, 1080));
        // No limits — pass-through.
        assert_eq!(fit_within(3840, 2160, 0, 0), (3840, 2160));
    }
}
