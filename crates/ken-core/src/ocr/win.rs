//! Windows OCR backend: decode with Windows.Graphics.Imaging, rasterize PDF
//! pages with Windows.Data.Pdf, recognize with Windows.Media.Ocr. All three
//! ship with Windows 10 and 11; recognition uses the OCR language of the
//! person's profile languages, so nothing is downloaded or bundled.
//!
//! WinRT wants an initialized apartment: every entry point joins the
//! multithreaded one ([`init_thread`]) and blocks on the async operations with
//! `.get()`. The OCR worker thread is the only caller in the app.

use std::cell::{Cell, RefCell};

use anyhow::{anyhow, Context, Result};
use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat, BitmapTransform,
    ColorManagementMode, ExifOrientationMode, SoftwareBitmap,
};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

use super::OcrRegion;

/// PDF pages are rasterized at this resolution before OCR, as on macOS.
const PDF_RENDER_DPI: f64 = 150.0;

/// A page's size is in device-independent pixels (1/96 inch).
const DIPS_PER_INCH: f64 = 96.0;

/// Guardrail on a rasterized page's pixel size, so an absurd page box cannot
/// ask for a multi-gigabyte bitmap (10k px is ~66 inches at 150 DPI).
const MAX_PAGE_PIXELS: f64 = 10_000.0;

/// Join the multithreaded apartment on this thread, once. A thread that is
/// already in a single-threaded apartment stays there (the call fails with
/// RPC_E_CHANGED_MODE, which is fine: WinRT works there too).
pub fn init_thread() {
    thread_local!(static JOINED: Cell<bool> = const { Cell::new(false) });
    JOINED.with(|joined| {
        if !joined.get() {
            // SAFETY: plain Win32 call on the current thread; the apartment
            // lives as long as the thread.
            let _ = unsafe {
                windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED)
            };
            joined.set(true);
        }
    });
}

/// Whether Windows can recognize text here: an OCR engine for one of the
/// person's profile languages, or failing that any installed OCR language.
/// Checked on a thread of its own, so the caller's apartment does not matter.
pub fn available() -> bool {
    std::thread::spawn(|| {
        init_thread();
        create_engine().is_ok()
    })
    .join()
    .unwrap_or(false)
}

/// OCR an image file's bytes (PNG, JPEG, TIFF, BMP, GIF, HEIC when its codec
/// is installed). All regions are on page 0.
pub fn ocr_image_bytes(bytes: &[u8]) -> Result<Vec<OcrRegion>> {
    init_thread();
    let stream = stream_from_bytes(bytes)?;
    let lines = recognize_stream(&stream)?;
    Ok(lines.into_iter().map(|(text, bbox)| OcrRegion { page: 0, text, bbox }).collect())
}

/// Render up to `max_pages` pages of a PDF at 150 DPI and OCR each. A page
/// that fails to render or read is skipped; a PDF that does not open is an
/// error.
pub fn ocr_pdf_bytes(bytes: &[u8], max_pages: usize) -> Result<Vec<OcrRegion>> {
    if max_pages == 0 || bytes.is_empty() {
        return Ok(Vec::new());
    }
    init_thread();
    let doc = open_pdf(bytes)?;
    let pages = (doc.PageCount()? as usize).min(max_pages);
    let mut out = Vec::new();
    for i in 0..pages {
        let Ok(png) = render_page(&doc, i as u32) else { continue };
        if let Ok(lines) = recognize_stream(&png) {
            out.extend(lines.into_iter().map(|(text, bbox)| OcrRegion { page: i as u32, text, bbox }));
        }
    }
    Ok(out)
}

/// An in-memory stream holding a copy of `bytes`, positioned at the start.
fn stream_from_bytes(bytes: &[u8]) -> Result<InMemoryRandomAccessStream> {
    if bytes.is_empty() {
        return Err(anyhow!("no image data"));
    }
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(bytes)?;
    writer.StoreAsync()?.get()?;
    writer.FlushAsync()?.get()?;
    // Keep the stream open when the writer goes.
    writer.DetachStream()?;
    stream.Seek(0)?;
    Ok(stream)
}

fn open_pdf(bytes: &[u8]) -> Result<PdfDocument> {
    let stream = stream_from_bytes(bytes)?;
    PdfDocument::LoadFromStreamAsync(&stream)
        .and_then(|op| op.get())
        .context("couldn't open PDF (corrupt, encrypted, or not a PDF)")
}

/// Page `index` rendered to a PNG stream at [`PDF_RENDER_DPI`].
fn render_page(doc: &PdfDocument, index: u32) -> Result<InMemoryRandomAccessStream> {
    let page = doc.GetPage(index)?;
    let size = page.Size()?;
    let scale = PDF_RENDER_DPI / DIPS_PER_INCH;
    let width = (f64::from(size.Width) * scale).round().clamp(1.0, MAX_PAGE_PIXELS);
    let height = (f64::from(size.Height) * scale).round().clamp(1.0, MAX_PAGE_PIXELS);
    let options = PdfPageRenderOptions::new()?;
    options.SetDestinationWidth(width as u32)?;
    options.SetDestinationHeight(height as u32)?;
    let out = InMemoryRandomAccessStream::new()?;
    page.RenderWithOptionsToStreamAsync(&out, &options)?.get()?;
    out.Seek(0)?;
    Ok(out)
}

/// Decode the image in `stream` and OCR it: one `(text, bbox)` per line, the
/// box normalized to the decoded image, top-left origin (WinRT's own).
fn recognize_stream(stream: &InMemoryRandomAccessStream) -> Result<Vec<(String, [f32; 4])>> {
    let decoder = BitmapDecoder::CreateAsync(stream)
        .and_then(|op| op.get())
        .context("couldn't decode the image")?;
    let bitmap = decoded_bitmap(&decoder)?;
    let (w, h) = (bitmap.PixelWidth()? as f32, bitmap.PixelHeight()? as f32);
    if w < 1.0 || h < 1.0 {
        return Ok(Vec::new());
    }
    let result = with_engine(|engine| Ok(engine.RecognizeAsync(&bitmap)?.get()?))?;
    let lines = result.Lines()?;
    let mut out = Vec::new();
    for i in 0..lines.Size()? {
        let line = lines.GetAt(i)?;
        let text = line.Text()?.to_string();
        if text.trim().is_empty() {
            continue;
        }
        let words = line.Words()?;
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for j in 0..words.Size()? {
            let r = words.GetAt(j)?.BoundingRect()?;
            x0 = x0.min(r.X);
            y0 = y0.min(r.Y);
            x1 = x1.max(r.X + r.Width);
            y1 = y1.max(r.Y + r.Height);
        }
        if x0 > x1 || y0 > y1 {
            continue;
        }
        out.push((text, normalized_box(x0, y0, x1, y1, w, h)));
    }
    Ok(out)
}

/// The decoded frame as BGRA8, upright (EXIF orientation applied) and scaled
/// down to the engine's largest supported side when it is bigger.
fn decoded_bitmap(decoder: &BitmapDecoder) -> Result<SoftwareBitmap> {
    let (w, h) = (decoder.PixelWidth()?, decoder.PixelHeight()?);
    let max = OcrEngine::MaxImageDimension().unwrap_or(10_000).max(1);
    let transform = BitmapTransform::new()?;
    let longest = w.max(h).max(1);
    if longest > max {
        // Scaling happens before the EXIF rotation, so it is on the stored
        // width and height.
        let scale = f64::from(max) / f64::from(longest);
        transform.SetScaledWidth(((f64::from(w) * scale) as u32).max(1))?;
        transform.SetScaledHeight(((f64::from(h) * scale) as u32).max(1))?;
        transform.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
    }
    Ok(decoder
        .GetSoftwareBitmapTransformedAsync(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Premultiplied,
            &transform,
            ExifOrientationMode::RespectExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )?
        .get()?)
}

/// A pixel rectangle as `[x, y, w, h]` fractions of a `w`×`h` image, clamped
/// to the unit square.
fn normalized_box(x0: f32, y0: f32, x1: f32, y1: f32, w: f32, h: f32) -> [f32; 4] {
    let x = (x0 / w).clamp(0.0, 1.0);
    let y = (y0 / h).clamp(0.0, 1.0);
    let right = (x1 / w).clamp(0.0, 1.0);
    let bottom = (y1 / h).clamp(0.0, 1.0);
    [x, y, right - x, bottom - y]
}

/// The engine for the person's profile languages, else for the first OCR
/// language installed.
fn create_engine() -> Result<OcrEngine> {
    if let Ok(engine) = OcrEngine::TryCreateFromUserProfileLanguages() {
        return Ok(engine);
    }
    let languages = OcrEngine::AvailableRecognizerLanguages()?;
    for i in 0..languages.Size()? {
        if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&languages.GetAt(i)?) {
            return Ok(engine);
        }
    }
    Err(anyhow!("no OCR language is installed in Windows"))
}

/// Run `f` with this thread's engine, made once per thread.
fn with_engine<T>(f: impl FnOnce(&OcrEngine) -> Result<T>) -> Result<T> {
    thread_local!(static ENGINE: RefCell<Option<OcrEngine>> = const { RefCell::new(None) });
    let engine = ENGINE.with(|cell| -> Result<OcrEngine> {
        if let Some(e) = cell.borrow().as_ref() {
            return Ok(e.clone());
        }
        let e = create_engine()?;
        *cell.borrow_mut() = Some(e.clone());
        Ok(e)
    })?;
    f(&engine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Storage::Streams::DataReader;

    /// A one-page PDF with `text` set in 48 pt Helvetica (a standard font, so
    /// nothing is embedded), with a correct cross-reference table.
    fn text_pdf(text: &str) -> Vec<u8> {
        let content = format!("BT /F1 48 Tf 72 600 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for off in offsets {
            out.extend(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend(
            format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes(),
        );
        out
    }

    fn stream_bytes(stream: &InMemoryRandomAccessStream) -> Vec<u8> {
        let size = stream.Size().unwrap() as u32;
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).unwrap()).unwrap();
        reader.LoadAsync(size).unwrap().get().unwrap();
        let mut buf = vec![0u8; size as usize];
        reader.ReadBytes(&mut buf).unwrap();
        buf
    }

    fn joined(regions: &[OcrRegion]) -> String {
        regions.iter().map(|r| r.text.to_uppercase()).collect::<Vec<_>>().join(" ")
    }

    /// A real run through Windows.Data.Pdf and Windows.Media.Ocr: the PDF
    /// path renders the page and reads its words, and the same page as a PNG
    /// reads the same through the image path. Skipped (passes) on a Windows
    /// with no OCR language installed.
    #[test]
    fn reads_the_words_of_a_pdf_page_and_of_an_image() {
        if !available() {
            eprintln!("no OCR language installed; skipping");
            return;
        }
        let pdf = text_pdf("HELLO KEN 42");
        let regions = ocr_pdf_bytes(&pdf, 5).unwrap();
        let text = joined(&regions);
        assert!(text.contains("HELLO") && text.contains("KEN"), "read: {text:?}");
        assert!(regions.iter().all(|r| r.page == 0));
        for r in &regions {
            let [x, y, w, h] = r.bbox;
            assert!(w > 0.0 && h > 0.0 && x + w <= 1.0 + 1e-4 && y + h <= 1.0 + 1e-4, "{r:?}");
            // The line sits in the top quarter of the page, as drawn.
            assert!(y < 0.4, "box not top-left based: {r:?}");
        }

        init_thread();
        let png = stream_bytes(&render_page(&open_pdf(&pdf).unwrap(), 0).unwrap());
        let image = ocr_image_bytes(&png).unwrap();
        assert!(joined(&image).contains("HELLO"), "read: {:?}", joined(&image));
        assert!(image.iter().all(|r| r.page == 0));
    }

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        assert!(ocr_image_bytes(&[]).is_err());
        assert!(ocr_image_bytes(b"not an image").is_err());
        assert!(ocr_pdf_bytes(b"not a pdf", 3).is_err());
        assert!(ocr_pdf_bytes(&[], 3).unwrap().is_empty());
        assert!(ocr_pdf_bytes(&text_pdf("x"), 0).unwrap().is_empty());
    }

    #[test]
    fn boxes_are_fractions_of_the_image() {
        fn approx(a: [f32; 4], b: [f32; 4]) {
            assert!(a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() < 1e-6), "got {a:?}, expected {b:?}");
        }
        approx(normalized_box(10.0, 20.0, 60.0, 40.0, 100.0, 200.0), [0.1, 0.1, 0.5, 0.1]);
        // Clamped to the image.
        approx(normalized_box(-5.0, 0.0, 150.0, 10.0, 100.0, 100.0), [0.0, 0.0, 1.0, 0.1]);
    }
}
