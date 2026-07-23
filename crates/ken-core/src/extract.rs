//! Format extractors: turn any file into indexable text. Extraction is
//! best-effort per file — a failure is data (status + reason), never a
//! pipeline stop. All extractors are pure Rust so ken-mcp shares them.

use std::fs;
use std::io::{BufReader, Read};
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;
use serde_json::Value;

use crate::{Error, Result};

/// Files larger than this get a metadata-only entry rather than content
/// extraction (protects the index and the UI from pathological inputs).
pub const MAX_EXTRACT_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Md,
    Txt,
    Code,
    Docx,
    Xlsx,
    Pptx,
    Pdf,
    Ipynb,
    Image,
    Video,
    Binary,
}

impl FileKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileKind::Md => "md",
            FileKind::Txt => "txt",
            FileKind::Code => "code",
            FileKind::Docx => "docx",
            FileKind::Xlsx => "xlsx",
            FileKind::Pptx => "pptx",
            FileKind::Pdf => "pdf",
            FileKind::Ipynb => "ipynb",
            FileKind::Image => "image",
            FileKind::Video => "video",
            FileKind::Binary => "binary",
        }
    }

    pub fn from_path(path: &Path) -> FileKind {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "md" | "markdown" => FileKind::Md,
            // `.vtt` (WebVTT) is plain text: classify it as Txt so a standalone
            // transcript is editable in the UI and indexes its own words. (An
            // adjacent-to-video `.vtt` is still pulled in as that video's
            // transcript by `crate::transcript`; see the note in `extract`.)
            "txt" | "text" | "log" | "vtt" => FileKind::Txt,
            "rs" | "ts" | "js" | "jsx" | "tsx" | "svelte" | "py" | "rb" | "go" | "java"
            | "c" | "cc" | "cpp" | "h" | "hpp" | "cs" | "swift" | "kt" | "sh" | "bash"
            | "zsh" | "sql" | "json" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "html"
            | "htm" | "css" | "scss" | "xml" | "csv" => FileKind::Code,
            "docx" => FileKind::Docx,
            "xlsx" | "xlsm" => FileKind::Xlsx,
            "pptx" => FileKind::Pptx,
            "pdf" => FileKind::Pdf,
            "ipynb" => FileKind::Ipynb,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "bmp" | "tiff" | "tif"
            | "svg" => FileKind::Image,
            "mp4" | "mov" | "m4v" | "webm" | "mkv" | "avi" => FileKind::Video,
            _ => FileKind::Binary,
        }
    }

    /// Does this kind carry extractable text content?
    pub fn has_content(&self) -> bool {
        !matches!(self, FileKind::Image | FileKind::Binary)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Extracted {
    pub text: String,
    /// A human title when the format offers one (e.g. first markdown H1).
    pub title: Option<String>,
}

/// Extract text from a file according to its detected kind.
pub fn extract(path: &Path) -> Result<Extracted> {
    let kind = FileKind::from_path(path);
    // A video's "content" is its transcript, resolved from adjacent/generated
    // files — never the container itself, so the size cap below (which would
    // reject most videos) must not gate it. Note: a `.vtt` sitting next to a
    // video is indexed both here (as the video's transcript) and, since `.vtt`
    // classifies as Txt, as its own file — two rows carrying the same words.
    // That's benign (both are legitimately searchable); de-duping would need
    // cross-file context the per-file extractor deliberately doesn't have.
    if kind == FileKind::Video {
        return Ok(Extracted {
            text: crate::transcript::indexable_text(path),
            title: None,
        });
    }
    let size = fs::metadata(path).map_err(|e| Error::io(path, e))?.len();
    if size > MAX_EXTRACT_BYTES {
        return Ok(Extracted::default());
    }
    match kind {
        FileKind::Md => extract_markdown(path),
        FileKind::Txt | FileKind::Code => extract_plain(path),
        FileKind::Docx => extract_docx(path),
        FileKind::Xlsx => extract_xlsx(path),
        FileKind::Pptx => extract_pptx(path),
        FileKind::Pdf => extract_pdf(path),
        FileKind::Ipynb => extract_ipynb(path),
        FileKind::Image => extract_image(path),
        // Handled above, before the size cap.
        FileKind::Video => Ok(Extracted::default()),
        FileKind::Binary => Ok(Extracted::default()),
    }
}

fn extract_plain(path: &Path) -> Result<Extracted> {
    let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
    Ok(Extracted {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        title: None,
    })
}

fn extract_markdown(path: &Path) -> Result<Extracted> {
    let mut out = extract_plain(path)?;
    out.title = out
        .text
        .lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|t| t.trim().to_string());
    Ok(out)
}

/// Pull the character data of specific XML elements out of a zip entry.
fn zip_xml_text(
    archive: &mut zip::ZipArchive<fs::File>,
    entry: &str,
    text_tag: &[u8],
    para_tag: &[u8],
) -> Result<String> {
    let mut file = archive
        .by_name(entry)
        .map_err(|e| Error::Extraction(format!("{entry}: {e}")))?;
    let mut xml = String::new();
    file.read_to_string(&mut xml)
        .map_err(|e| Error::Extraction(format!("{entry}: {e}")))?;

    let mut reader = XmlReader::from_str(&xml);
    let mut out = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == text_tag => in_text = true,
            Ok(Event::End(e)) if e.local_name().as_ref() == text_tag => in_text = false,
            Ok(Event::End(e)) if e.local_name().as_ref() == para_tag => out.push('\n'),
            Ok(Event::Text(t)) if in_text => {
                out.push_str(&t.decode().map_err(|e| Error::Extraction(e.to_string()))?);
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(Error::Extraction(format!("{entry}: {e}"))),
            _ => {}
        }
    }
    Ok(out)
}

fn open_zip(path: &Path) -> Result<zip::ZipArchive<fs::File>> {
    let file = fs::File::open(path).map_err(|e| Error::io(path, e))?;
    zip::ZipArchive::new(file).map_err(|e| Error::Extraction(e.to_string()))
}

fn extract_docx(path: &Path) -> Result<Extracted> {
    let mut archive = open_zip(path)?;
    let text = zip_xml_text(&mut archive, "word/document.xml", b"t", b"p")?;
    Ok(Extracted { text, title: None })
}

fn extract_pptx(path: &Path) -> Result<Extracted> {
    let mut archive = open_zip(path)?;
    let mut slides: Vec<String> = archive
        .file_names()
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .map(String::from)
        .collect();
    slides.sort();
    if slides.is_empty() {
        return Err(Error::Extraction("no slides found".into()));
    }
    let mut text = String::new();
    for slide in slides {
        text.push_str(&zip_xml_text(&mut archive, &slide, b"t", b"p")?);
        text.push('\n');
    }
    Ok(Extracted { text, title: None })
}

fn extract_xlsx(path: &Path) -> Result<Extracted> {
    use calamine::{Reader, Xlsx};
    let mut workbook = calamine::open_workbook::<Xlsx<BufReader<fs::File>>, _>(path)
        .map_err(|e| Error::Extraction(e.to_string()))?;
    let mut text = String::new();
    let sheet_names: Vec<String> = workbook.sheet_names().to_vec();
    for name in sheet_names {
        let range = workbook
            .worksheet_range(&name)
            .map_err(|e| Error::Extraction(e.to_string()))?;
        text.push_str(&name);
        text.push('\n');
        for row in range.rows() {
            let cells: Vec<String> = row
                .iter()
                .map(|c| c.to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !cells.is_empty() {
                text.push_str(&cells.join(" | "));
                text.push('\n');
            }
        }
    }
    Ok(Extracted { text, title: None })
}

fn extract_pdf(path: &Path) -> Result<Extracted> {
    // pdf-extract can panic on malformed files; contain it.
    let path_buf = path.to_path_buf();
    let text = std::panic::catch_unwind(move || pdf_extract::extract_text(&path_buf))
        .map_err(|_| Error::Extraction("pdf parser crashed on this file".into()))?
        .map_err(|e| Error::Extraction(e.to_string()))?;
    Ok(Extracted { text, title: None })
}

/// Jupyter notebook: concatenate the `source` of markdown and code cells,
/// skipping cell outputs (execution results, images, stderr). `source` is
/// either a single string or an array of line strings per nbformat.
fn extract_ipynb(path: &Path) -> Result<Extracted> {
    let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
    let nb: Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Extraction(format!("notebook JSON: {e}")))?;
    let mut text = String::new();
    for cell in nb.get("cells").and_then(Value::as_array).into_iter().flatten() {
        match cell.get("cell_type").and_then(Value::as_str) {
            Some("markdown") | Some("code") => {}
            _ => continue, // skip raw/unknown cells; never read outputs
        }
        match cell.get("source") {
            Some(Value::String(s)) => text.push_str(s),
            Some(Value::Array(lines)) => {
                for line in lines.iter().filter_map(Value::as_str) {
                    text.push_str(line);
                }
            }
            _ => {}
        }
        text.push('\n');
    }
    Ok(Extracted { text, title: None })
}

fn extract_image(path: &Path) -> Result<Extracted> {
    // Filename is indexed separately; here we add EXIF text if present.
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(Extracted::default()),
    };
    let mut reader = BufReader::new(file);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut reader) else {
        return Ok(Extracted::default());
    };
    let mut parts = Vec::new();
    for tag in [
        exif::Tag::ImageDescription,
        exif::Tag::Make,
        exif::Tag::Model,
        exif::Tag::DateTimeOriginal,
    ] {
        if let Some(field) = exif.get_field(tag, exif::In::PRIMARY) {
            parts.push(field.display_value().to_string());
        }
    }
    Ok(Extracted {
        text: parts.join(" "),
        title: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(rel: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/project")
            .join(rel)
    }

    #[test]
    fn kind_detection() {
        assert_eq!(FileKind::from_path(Path::new("a/b.md")), FileKind::Md);
        assert_eq!(FileKind::from_path(Path::new("b.RS")), FileKind::Code);
        assert_eq!(FileKind::from_path(Path::new("x.docx")), FileKind::Docx);
        assert_eq!(FileKind::from_path(Path::new("nb.ipynb")), FileKind::Ipynb);
        assert!(FileKind::Ipynb.has_content());
        assert_eq!(FileKind::from_path(Path::new("x.unknown")), FileKind::Binary);
        assert_eq!(FileKind::from_path(Path::new("noext")), FileKind::Binary);
    }

    #[test]
    fn video_kind_mapping() {
        for ext in ["mp4", "mov", "m4v", "webm", "mkv", "avi", "MP4", "MoV"] {
            let name = format!("clips/demo.{ext}");
            assert_eq!(
                FileKind::from_path(Path::new(&name)),
                FileKind::Video,
                "{ext} should be Video"
            );
        }
        // A video's content is its transcript, so the kind is content-bearing:
        // a transcript makes it searchable, its absence leaves it metadata-only.
        assert!(FileKind::Video.has_content());
        assert_eq!(FileKind::Video.as_str(), "video");
    }

    #[test]
    fn markdown_text_and_title() {
        let out = extract(&fixture("notes/meeting.md")).unwrap();
        assert!(out.text.contains("billing cutover"));
        assert_eq!(out.title.as_deref(), Some("Migration sync"));
    }

    #[test]
    fn plain_and_code() {
        assert!(extract(&fixture("notes/plain.txt"))
            .unwrap()
            .text
            .contains("Rollback rehearsal"));
        assert!(extract(&fixture("src/example.rs"))
            .unwrap()
            .text
            .contains("vendor pricing"));
    }

    #[test]
    fn vtt_is_editable_text_not_binary() {
        // A `.vtt` transcript is plain text: it classifies as Txt (kind "txt",
        // which the UI treats as editable) and its raw text is extracted,
        // rather than falling through to Binary and being metadata-only.
        assert_eq!(FileKind::from_path(Path::new("m/talk.vtt")), FileKind::Txt);
        assert_eq!(FileKind::from_path(Path::new("m/talk.VTT")), FileKind::Txt);
        assert_eq!(FileKind::Txt.as_str(), "txt");
        assert!(FileKind::Txt.has_content());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meeting.vtt");
        std::fs::write(
            &path,
            "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\nBudget approved by Priya\n",
        )
        .unwrap();
        let out = extract(&path).unwrap();
        // extract_plain returns the file verbatim (no VTT stripping here).
        assert!(out.text.contains("Budget approved by Priya"), "got: {}", out.text);
        assert!(out.text.contains("WEBVTT"));
    }

    #[test]
    fn docx_paragraphs() {
        let out = extract(&fixture("docs/sample.docx")).unwrap();
        assert!(out.text.contains("Quarterly budget approved by Priya."));
        assert!(out.text.contains("LangdonSoft contract renewal pending review."));
    }

    #[test]
    fn pptx_slides_in_order() {
        let out = extract(&fixture("docs/deck.pptx")).unwrap();
        let kick = out.text.find("Migration kickoff deck").unwrap();
        let timeline = out.text.find("Timeline and owners").unwrap();
        assert!(kick < timeline);
    }

    #[test]
    fn xlsx_rows_and_sheets() {
        let out = extract(&fixture("vendor/quotes.xlsx")).unwrap();
        assert!(out.text.contains("Quotes"), "sheet name: {}", out.text);
        assert!(out.text.contains("LangdonSoft"));
        assert!(out.text.contains("12500"));
    }

    #[test]
    fn pdf_text() {
        let out = extract(&fixture("vendor/contract.pdf")).unwrap();
        assert!(
            out.text.contains("Contract renewal terms"),
            "got: {}",
            out.text
        );
    }

    #[test]
    fn corrupt_pdf_is_error_not_panic() {
        let err = extract(&fixture("vendor/corrupt.pdf")).unwrap_err();
        assert!(matches!(err, Error::Extraction(_)));
    }

    #[test]
    fn image_without_exif_is_empty_not_error() {
        let out = extract(&fixture("images/team-photo.png")).unwrap();
        assert_eq!(out.text, "");
    }

    #[test]
    fn ipynb_concatenates_markdown_and_code_skipping_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("analysis.ipynb");
        // `source` as both an array of lines and a plain string; an output
        // block and a raw cell that must not leak into the index.
        let nb = r##"{
          "cells": [
            {"cell_type": "markdown",
             "source": ["# Revenue analysis\n", "Quarterly ", "numbers."]},
            {"cell_type": "code",
             "source": "import pandas as pd\nrevenue = 12500\n",
             "outputs": [{"output_type": "stream", "text": "SECRET_OUTPUT_LEAK"}]},
            {"cell_type": "raw", "source": ["ignored raw cell"]}
          ],
          "metadata": {},
          "nbformat": 4,
          "nbformat_minor": 5
        }"##;
        std::fs::write(&path, nb).unwrap();
        let out = extract(&path).unwrap();
        assert!(out.text.contains("Revenue analysis"), "got: {}", out.text);
        assert!(out.text.contains("Quarterly numbers."), "got: {}", out.text);
        assert!(out.text.contains("import pandas as pd"));
        assert!(out.text.contains("12500"));
        assert!(!out.text.contains("SECRET_OUTPUT_LEAK"), "outputs must be skipped");
        assert!(!out.text.contains("ignored raw cell"), "raw cells must be skipped");
    }

    #[test]
    fn ipynb_malformed_is_error_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.ipynb");
        std::fs::write(&path, "{ this is not valid notebook json ").unwrap();
        let err = extract(&path).unwrap_err();
        assert!(matches!(err, Error::Extraction(_)));
    }

    #[test]
    fn binary_is_metadata_only() {
        let out = extract(&fixture("data/blob.bin")).unwrap();
        assert_eq!(out.text, "");
        assert!(!FileKind::from_path(&fixture("data/blob.bin")).has_content());
    }
}
