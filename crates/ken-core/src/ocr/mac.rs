//! macOS OCR backend: decode/rasterize with ImageIO + CoreGraphics, then
//! recognize text with Apple's Vision framework (`VNRecognizeTextRequest`,
//! accurate level). Everything here is a thin, synchronous shell over the
//! platform APIs — like `record::mac`, it can only be exercised on real macOS
//! hardware, so the pure parts (the y-flip) are tested in `super`.
//!
//! Every native call is wrapped so a `NULL`/failure becomes an `anyhow::Error`
//! rather than a panic across the Objective-C boundary. `performRequests` runs
//! Vision synchronously on the calling thread, so no completion blocks or
//! cross-thread plumbing are needed (unlike the recorder's async capture).

use std::ffi::c_void;
use std::ptr::NonNull;

use anyhow::{anyhow, Result};

use objc2::runtime::AnyObject;
use objc2::AnyThread;

use objc2_core_foundation::{CFData, CFRetained};
use objc2_core_graphics::{
    CGColorSpace, CGContext, CGDataProvider, CGImage, CGPDFBox, CGPDFDocument, CGPDFPage,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSArray, NSDictionary};
use objc2_image_io::CGImageSource;
use objc2_vision::{
    VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest,
    VNRequestTextRecognitionLevel,
};

use super::{to_top_left, OcrRegion};

/// PDF pages are rasterized at this resolution before OCR. 150 DPI is a good
/// accuracy/speed/memory tradeoff for document text.
const PDF_RENDER_DPI: f64 = 150.0;

/// Guardrail on a rasterized page's pixel dimensions so a hostile or absurd
/// MediaBox can't request a multi-gigabyte bitmap. 10k px on a side at 150 DPI
/// covers ~66 inches — far beyond any real page.
const MAX_PAGE_PIXELS: f64 = 10_000.0;

// `CGBitmapContextCreate` is not surfaced by objc2-core-graphics 0.3 (only the
// block-based "adaptive" variant is), so declare the classic C entry point
// ourselves — the same extern "C" CoreGraphics pattern `record::mac` uses for
// the screen-capture TCC calls. Returns a +1-owned CGContextRef (or NULL).
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: Option<&CGColorSpace>,
        bitmap_info: u32,
    ) -> *mut CGContext;
}

/// kCGImageAlphaNoneSkipLast — opaque RGBX, the simplest fast bitmap layout for
/// rendering an OCR raster (we paint an opaque white background first).
const ALPHA_NONE_SKIP_LAST: u32 = 5;

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Decode image bytes to a `CGImage` via ImageIO and OCR it. All regions are
/// tagged `page = 0`.
pub fn ocr_image_bytes(bytes: &[u8]) -> Result<Vec<OcrRegion>> {
    let image = cgimage_from_bytes(bytes)?;
    let lines = recognize(&image)?;
    Ok(lines
        .into_iter()
        .map(|(text, bbox)| OcrRegion { page: 0, text, bbox })
        .collect())
}

/// Open a PDF from bytes, rasterize up to `max_pages` pages at ~150 DPI, and
/// OCR each. Regions carry their 0-based page index. Pages that fail to render
/// are skipped rather than aborting the whole document.
pub fn ocr_pdf_bytes(bytes: &[u8], max_pages: usize) -> Result<Vec<OcrRegion>> {
    if max_pages == 0 || bytes.is_empty() {
        return Ok(Vec::new());
    }
    let data = cfdata_from_bytes(bytes)?;
    let provider = CGDataProvider::with_cf_data(Some(&data))
        .ok_or_else(|| anyhow!("couldn't wrap PDF bytes in a data provider"))?;
    let doc = CGPDFDocument::with_provider(Some(&provider))
        .ok_or_else(|| anyhow!("couldn't open PDF (corrupt, encrypted, or not a PDF)"))?;

    let total = CGPDFDocument::number_of_pages(Some(&doc));
    let pages = total.min(max_pages);
    let mut out = Vec::new();
    for i in 0..pages {
        // CGPDFDocument pages are 1-based; our region index is 0-based.
        let page_number = i + 1;
        let Some(page) = CGPDFDocument::page(Some(&doc), page_number) else {
            continue;
        };
        let image = match render_pdf_page(&page) {
            Ok(img) => img,
            Err(_) => continue, // skip an unrenderable page, keep the rest
        };
        // A blank/failed recognition on one page shouldn't sink the document.
        if let Ok(lines) = recognize(&image) {
            for (text, bbox) in lines {
                out.push(OcrRegion { page: i as u32, text, bbox });
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// ImageIO: bytes -> CGImage
// ---------------------------------------------------------------------------

/// Wrap a byte slice in a (copying) `CFData`. ImageIO/CoreGraphics keep their
/// own reference to the data, so a copy is the safe, simple choice.
fn cfdata_from_bytes(bytes: &[u8]) -> Result<CFRetained<CFData>> {
    if bytes.is_empty() {
        return Err(anyhow!("no image data"));
    }
    // SAFETY: `bytes` is a valid slice of `bytes.len()` bytes; CFDataCreate
    // copies out of it and does not retain the pointer.
    let data = unsafe { CFData::new(None, bytes.as_ptr(), bytes.len() as isize) };
    data.ok_or_else(|| anyhow!("couldn't allocate CFData for image bytes"))
}

/// Decode the first image in the byte stream to a `CGImage` via ImageIO. Fails
/// cleanly (no panic) for undecodable or vector-only inputs such as SVG.
fn cgimage_from_bytes(bytes: &[u8]) -> Result<CFRetained<CGImage>> {
    let data = cfdata_from_bytes(bytes)?;
    // SAFETY: no options dictionary; `data` outlives the source creation.
    let source = unsafe { CGImageSource::with_data(&data, None) }
        .ok_or_else(|| anyhow!("unrecognized image data"))?;
    // SAFETY: valid source; index 0 is the primary image, no options.
    let image = unsafe { source.image_at_index(0, None) }
        .ok_or_else(|| anyhow!("couldn't decode image (unsupported or vector-only format)"))?;
    Ok(image)
}

// ---------------------------------------------------------------------------
// CoreGraphics: PDF page -> CGImage
// ---------------------------------------------------------------------------

/// Rasterize one PDF page into an RGB bitmap and return it as a `CGImage`.
/// Uses the crop box (falling back to the media box) and `CGPDFPageGetDrawing-
/// Transform` so page rotation and box offset are handled correctly.
fn render_pdf_page(page: &CGPDFPage) -> Result<CFRetained<CGImage>> {
    // Prefer the crop box; fall back to the media box if it's empty/degenerate.
    let crop = CGPDFPage::box_rect(Some(page), CGPDFBox::CropBox);
    let rect = if crop.size.width > 1.0 && crop.size.height > 1.0 {
        crop
    } else {
        CGPDFPage::box_rect(Some(page), CGPDFBox::MediaBox)
    };
    if rect.size.width <= 1.0 || rect.size.height <= 1.0 {
        return Err(anyhow!("PDF page has no usable page box"));
    }

    let scale = PDF_RENDER_DPI / 72.0; // PDF user space is 72 units / inch
    let pw = (rect.size.width * scale).ceil().clamp(1.0, MAX_PAGE_PIXELS) as usize;
    let ph = (rect.size.height * scale).ceil().clamp(1.0, MAX_PAGE_PIXELS) as usize;

    let colorspace = CGColorSpace::new_device_rgb()
        .ok_or_else(|| anyhow!("couldn't create RGB color space"))?;

    // SAFETY: passing NULL data lets CoreGraphics own the backing store; the
    // returned context is +1-owned and adopted into CFRetained below.
    let ctx_ptr = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            pw,
            ph,
            8,
            pw * 4,
            Some(&colorspace),
            ALPHA_NONE_SKIP_LAST,
        )
    };
    let ctx_ptr = NonNull::new(ctx_ptr)
        .ok_or_else(|| anyhow!("couldn't create bitmap context for PDF page"))?;
    // SAFETY: `CGBitmapContextCreate` returns a +1 reference; adopt it so it is
    // released exactly once when this CFRetained drops.
    let ctx: CFRetained<CGContext> = unsafe { CFRetained::from_raw(ctx_ptr) };

    // Opaque white background so anti-aliased dark text has good contrast.
    let full = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(pw as f64, ph as f64));
    CGContext::set_rgb_fill_color(Some(&ctx), 1.0, 1.0, 1.0, 1.0);
    CGContext::fill_rect(Some(&ctx), full);

    // Fit the chosen page box into the pixel rect (handles rotation + offset).
    let transform = CGPDFPage::drawing_transform(Some(page), CGPDFBox::CropBox, full, 0, true);
    CGContext::concat_ctm(Some(&ctx), transform);
    CGContext::draw_pdf_page(Some(&ctx), Some(page));

    objc2_core_graphics::CGBitmapContextCreateImage(Some(&ctx))
        .ok_or_else(|| anyhow!("couldn't snapshot rendered PDF page"))
}

// ---------------------------------------------------------------------------
// Vision: CGImage -> recognized lines
// ---------------------------------------------------------------------------

/// Run an accurate `VNRecognizeTextRequest` over `image` and return each line's
/// top candidate string plus its bounding box, already converted to top-left
/// normalized coordinates. An image with no text yields an empty vec.
fn recognize(image: &CGImage) -> Result<Vec<(String, [f32; 4])>> {
    // Empty options dictionary — we pass no camera intrinsics etc.
    let options = NSDictionary::<VNImageOption, AnyObject>::new();
    // SAFETY: `image` is a valid CGImage that outlives the handler use below;
    // `options` is an empty, correctly-typed dictionary.
    let handler = unsafe {
        VNImageRequestHandler::initWithCGImage_options(
            VNImageRequestHandler::alloc(),
            image,
            &options,
        )
    };

    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    request.setUsesLanguageCorrection(true);

    // Build the [VNRequest] array Vision wants. VNRecognizeTextRequest is a
    // subclass of VNRequest, so borrow it as its superclass.
    let req_ref: &VNRequest = request.as_ref();
    let requests = NSArray::from_slice(&[req_ref]);

    handler
        .performRequests_error(&requests)
        .map_err(|e| anyhow!("Vision text recognition failed: {e}"))?;

    let mut out = Vec::new();
    let Some(results) = request.results() else {
        return Ok(out);
    };
    for obs in results.to_vec() {
        // Top candidate only (maxCount = 1); skip observations with none.
        let candidates = obs.topCandidates(1);
        let Some(top) = candidates.to_vec().into_iter().next() else {
            continue;
        };
        let text = top.string().to_string();
        if text.is_empty() {
            continue;
        }
        // Vision boundingBox: normalized, bottom-left origin.
        let b = unsafe { obs.boundingBox() };
        let bbox = to_top_left(
            b.origin.x as f32,
            b.origin.y as f32,
            b.size.width as f32,
            b.size.height as f32,
        );
        out.push((text, bbox));
    }
    Ok(out)
}
