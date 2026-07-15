//! Native OCR: extract text and bounding boxes from images and from the pages
//! of PDFs. The heavy lifting is Apple's Vision framework, which lives entirely
//! in the macOS backend (`mac.rs`) behind the small, platform-agnostic surface
//! declared here. Non-macOS targets get a stub so the crate still builds
//! everywhere (and with `--no-default-features`), mirroring how `record` gates
//! its native capture behind `#[cfg(target_os = "macos")] pub mod mac;`.
//!
//! Coordinates: every [`OcrRegion::bbox`] is normalized to the unit square and
//! uses a **top-left** origin (x grows right, y grows down) — already flipped
//! from Vision's bottom-left convention — so downstream code (highlighting,
//! cropping) can treat it like a web/image rectangle without further math.

use anyhow::Result;

#[cfg(target_os = "macos")]
pub mod mac;

/// One line of recognized text with its location on the page.
///
/// `bbox` is `[x, y, w, h]`, each in `0.0..=1.0`, in **top-left origin**
/// normalized coordinates (see the module docs). `page` is the 0-based page
/// index a region came from; single images always report page `0`.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrRegion {
    pub page: u32,
    pub text: String,
    pub bbox: [f32; 4],
}

/// OCR a single decoded image supplied as its raw file bytes (PNG, JPEG, HEIC,
/// TIFF, … — anything ImageIO can decode). Returns one [`OcrRegion`] per
/// recognized text line, all tagged `page = 0`.
///
/// Undecodable or vector-only inputs (e.g. SVG, which ImageIO won't rasterize)
/// yield a clean `Err`; an image ImageIO decodes but that contains no text
/// yields `Ok(vec![])`. Never panics across the Objective-C boundary.
pub fn ocr_image_bytes(bytes: &[u8]) -> Result<Vec<OcrRegion>> {
    #[cfg(target_os = "macos")]
    {
        mac::ocr_image_bytes(bytes)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bytes;
        Ok(Vec::new())
    }
}

/// Rasterize each page of a PDF (up to `max_pages`) at ~150 DPI and OCR it,
/// returning regions across all processed pages with `region.page` set to the
/// 0-based page index. `max_pages == 0` processes nothing.
///
/// A PDF that fails to open yields an `Err`; pages that render blank simply
/// contribute no regions. Never panics across the Objective-C boundary.
pub fn ocr_pdf_bytes(bytes: &[u8], max_pages: usize) -> Result<Vec<OcrRegion>> {
    #[cfg(target_os = "macos")]
    {
        mac::ocr_pdf_bytes(bytes, max_pages)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (bytes, max_pages);
        Ok(Vec::new())
    }
}

/// Convert a Vision bounding box (normalized, **bottom-left** origin) to this
/// module's normalized **top-left** origin form. Pure and platform-free so the
/// y-flip is unit-testable without any Vision call: `y_topleft = 1 - y - h`.
pub(crate) fn to_top_left(x: f32, y: f32, w: f32, h: f32) -> [f32; 4] {
    [x, 1.0 - y - h, w, h]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn y_axis_is_flipped_to_top_left_origin() {
        // Compare component-wise with a tolerance: the flip is `1 - y - h`, which
        // in f32 does not land on exact zeros (e.g. 1.0 - 0.8 - 0.2 ≈ -7e-9), so
        // exact equality would spuriously fail.
        fn approx(a: [f32; 4], b: [f32; 4]) {
            for (x, y) in a.iter().zip(b.iter()) {
                assert!((x - y).abs() < 1e-6, "got {a:?}, expected {b:?}");
            }
        }
        // A box sitting on the bottom edge in Vision's frame (y = 0) must land
        // at the bottom in top-left coords too: its top edge is 1 - h.
        approx(to_top_left(0.0, 0.0, 1.0, 0.25), [0.0, 0.75, 1.0, 0.25]);
        // A box on the top edge in Vision's frame (y + h = 1) maps to y = 0.
        approx(to_top_left(0.1, 0.8, 0.3, 0.2), [0.1, 0.0, 0.3, 0.2]);
        // x and w are unchanged by the flip; only y moves.
        let [x, _, w, h] = to_top_left(0.42, 0.30, 0.15, 0.10);
        assert!((x - 0.42).abs() < 1e-6 && (w - 0.15).abs() < 1e-6 && (h - 0.10).abs() < 1e-6);
    }

    /// Compile-time smoke test: the public API and `OcrRegion` are usable, and
    /// the stub/back-end returns `Ok` for empty input without panicking. On
    /// macOS empty bytes decode to nothing (an `Err`) or yield no regions; on
    /// other targets the stub returns an empty vec. Either way, no panic.
    #[test]
    fn public_api_is_callable() {
        let _region = OcrRegion { page: 0, text: "hi".into(), bbox: [0.0, 0.0, 1.0, 1.0] };
        // Empty input must not panic. Result may be Ok or Err depending on
        // platform; we only assert it doesn't unwind.
        let _ = ocr_image_bytes(&[]);
        let _ = ocr_pdf_bytes(&[], 1);
    }

    /// Real OCR of an embedded PNG. Ignored on non-macOS (no Vision). Even on
    /// macOS this is marked `#[ignore]` because it needs a text-bearing PNG
    /// fixture and hits the live Vision engine; run it by hand with a fixture:
    ///
    /// ```ignore
    /// let png = std::fs::read("some-image-with-text.png").unwrap();
    /// let regions = ken_core::ocr::ocr_image_bytes(&png).unwrap();
    /// assert!(regions.iter().any(|r| r.text.contains("Hello")));
    /// ```
    ///
    /// `cargo test -p ken-core --ignored ocr_smoke` (with the fixture wired in).
    #[test]
    #[ignore = "needs a text-bearing image fixture; see doc comment to run manually"]
    #[cfg_attr(not(target_os = "macos"), ignore = "Vision is macOS-only")]
    fn ocr_smoke() {
        // Intentionally empty: enable and point at a fixture to validate a real
        // end-to-end Vision recognition run.
    }
}
