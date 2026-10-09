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
    /// A Windows `.url` internet shortcut — an INI-ish stub whose only real
    /// content is the link it points at.
    Url,
    /// A diagrams.net drawing; its searchable content is its text labels.
    Drawio,
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
            FileKind::Url => "url",
            FileKind::Drawio => "drawio",
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
            "txt" | "text" | "log" | "vtt" | "srt" => FileKind::Txt,
            "rs" | "ts" | "js" | "jsx" | "tsx" | "svelte" | "py" | "rb" | "go" | "java"
            | "c" | "cc" | "cpp" | "h" | "hpp" | "cs" | "swift" | "kt" | "sh" | "bash"
            | "zsh" | "sql" | "json" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "html"
            | "htm" | "css" | "scss" | "xml" | "csv" => FileKind::Code,
            // Build scripts, module variants, Vue and game-UI markup were read
            // as binary, so their words were never searchable: on 2026-10-06
            // "how is the mod packaged" missed build.gradle.kts and "one party
            // member's row on the HUD" missed PartyMemberChip.ui (Shattered
            // Realms has 129 .ui and 43 .mjs files). Named one by one rather
            // than "any file without NUL bytes is text", which would also read
            // the 12,000 .blockyanim/.blockymodel/.particle* asset files that
            // are meant to stay name-only. `.env.example` is still a secret
            // name (kenignore), so it is not here.
            "kts" | "gradle" | "properties" | "mjs" | "cjs" | "mts" | "cts" | "vue" | "ui" | "lang" => FileKind::Code,
            "docx" => FileKind::Docx,
            "xlsx" | "xlsm" => FileKind::Xlsx,
            "pptx" => FileKind::Pptx,
            "pdf" => FileKind::Pdf,
            "ipynb" => FileKind::Ipynb,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "bmp" | "tiff" | "tif"
            | "svg" => FileKind::Image,
            // A recording, with or without pictures: its content is its
            // transcript (`crate::transcript`), so audio files are this kind
            // too.
            "mp4" | "mov" | "m4v" | "webm" | "mkv" | "avi" | "wav" | "mp3" | "m4a" | "aac" | "flac" | "ogg"
            | "oga" | "opus" | "wma" => FileKind::Video,
            "url" => FileKind::Url,
            "drawio" => FileKind::Drawio,
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
        FileKind::Url => extract_url(path),
        FileKind::Drawio => extract_drawio(path),
        // Handled above, before the size cap.
        FileKind::Video => Ok(Extracted::default()),
        FileKind::Binary => Ok(Extracted::default()),
    }
}

fn extract_plain(path: &Path) -> Result<Extracted> {
    let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let is_html = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"));
    if is_html {
        return Ok(html_text(&text));
    }
    Ok(Extracted { text, title: None })
}

/// The text a person reads on an HTML page, and its `<title>`. A page saved
/// from a browser carries its scripts, styles and images inline: one 1.3 MB
/// talk page held 5,000 characters of words. Searching and ingest want the
/// words.
pub fn html_text(html: &str) -> Extracted {
    use regex::Regex;
    use std::sync::OnceLock;
    static RES: OnceLock<(Vec<Regex>, Regex, Regex, Regex, Regex)> = OnceLock::new();
    let (drop, title_re, block, tag, blank) = RES.get_or_init(|| {
        let drop = ["script", "style", "svg", "noscript", "template", "head"]
            .iter()
            .map(|t| Regex::new(&format!(r"(?is)<{t}\b[^>]*>.*?</{t}\s*>")).unwrap())
            .chain(std::iter::once(Regex::new(r"(?s)<!--.*?-->").unwrap()))
            .collect();
        (
            drop,
            Regex::new(r"(?is)<title[^>]*>(.*?)</title\s*>").unwrap(),
            Regex::new(r"(?i)</?(p|div|br|li|h[1-6]|tr|td|th|section|article|header|footer|main|nav|aside|ul|ol|table|blockquote|pre|figure|figcaption|dt|dd)\b[^>]*>").unwrap(),
            Regex::new(r"(?s)<[^>]*>").unwrap(),
            Regex::new(r"\n{3,}").unwrap(),
        )
    });
    let title = title_re
        .captures(html)
        .map(|c| decode_entities(c[1].trim()))
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty());
    let mut s = html.to_string();
    for re in drop {
        s = re.replace_all(&s, " ").into_owned();
    }
    let s = block.replace_all(&s, "\n");
    // Inline tags go without a space: `<b>faster</b>,` reads "faster,".
    let s = tag.replace_all(&s, "");
    let s = decode_entities(&s);
    let lines: Vec<String> = s.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect();
    let text = blank.replace_all(lines.join("\n").trim(), "\n\n").into_owned();
    Extracted { text, title }
}

/// The HTML entities that show up in prose, and numeric ones.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let end = after.find(';').filter(|e| *e <= 10);
        let decoded = end.and_then(|e| {
            let name = &after[..e];
            let ch = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                "ndash" => Some('–'),
                "mdash" => Some('—'),
                "lsquo" => Some('‘'),
                "rsquo" => Some('’'),
                "ldquo" => Some('“'),
                "rdquo" => Some('”'),
                "hellip" => Some('…'),
                "middot" => Some('·'),
                "bull" => Some('•'),
                "copy" => Some('©'),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            };
            ch.map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &after[e + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A `.url` shortcut's searchable content is the URL it points at — the rest of
/// the file (`[InternetShortcut]`, icon indexes) is noise no one searches for.
fn extract_url(path: &Path) -> Result<Extracted> {
    let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
    Ok(Extracted {
        text: parse_internet_shortcut(&String::from_utf8_lossy(&bytes)).unwrap_or_default(),
        title: None,
    })
}

/// A drawio file's searchable content is its diagram labels. Undecodable
/// content yields empty text (→ metadata_only in the scanner), never an error.
fn extract_drawio(path: &Path) -> Result<Extracted> {
    let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
    Ok(Extracted {
        text: crate::drawio::extract_labels(&String::from_utf8_lossy(&bytes)),
        title: None,
    })
}

/// The value of the first `URL=` key in an `.url` file. Case-insensitive on the
/// key (writers vary) and tolerant of surrounding whitespace; returns None when
/// the file carries no link at all.
pub fn parse_internet_shortcut(raw: &str) -> Option<String> {
    raw.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        key.trim().eq_ignore_ascii_case("url").then(|| value.trim().to_string())
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

    #[test]
    fn html_keeps_the_words_and_drops_scripts_styles_and_inline_images() {
        let page = format!(
            "<!doctype html><html><head><title>AI First Development &ndash; Optimize Yourself</title>\
             <style>body {{ color: red }}</style><script>var big = \"{}\";</script></head>\
             <body><!-- nav --><h1>Where to Start</h1><p>The goal is not only to develop <b>faster</b>, \
             but&nbsp;also to raise the quality.</p><img src=\"data:image/png;base64,{}\">\
             <svg><path d=\"M0 0\"/></svg><ul><li>Skills</li><li>Tools &amp; more</li></ul></body></html>",
            "x".repeat(200_000),
            "A".repeat(200_000),
        );
        let got = html_text(&page);
        assert_eq!(got.title.as_deref(), Some("AI First Development – Optimize Yourself"));
        assert!(got.text.contains("Where to Start"));
        assert!(got.text.contains("The goal is not only to develop faster, but also to raise the quality."));
        assert!(got.text.contains("Skills\n") && got.text.contains("Tools & more"));
        assert!(!got.text.contains("xxxx") && !got.text.contains("AAAA") && !got.text.contains("color"));
        assert!(got.text.len() < 300, "{} chars", got.text.len());
    }

    #[test]
    fn html_files_extract_as_text() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("talk.html");
        fs::write(&p, "<html><head><title>T</title></head><body><p>Hello <i>there</i></p></body></html>").unwrap();
        let got = extract(&p).unwrap();
        assert_eq!(got.text, "Hello there");
        assert_eq!(got.title.as_deref(), Some("T"));
    }

    #[test]
    fn entities_decode_and_a_bare_ampersand_stays() {
        assert_eq!(decode_entities("a &amp; b &#8212; c &#x2019; d & e &unknown; f"), "a & b — c ’ d & e &unknown; f");
    }

    /// The real page that hit the prompt limit: `KEN_HTML_SAMPLE=<path>
    /// cargo test -- --ignored html_sample`.
    #[test]
    #[ignore]
    fn html_sample() {
        let Ok(p) = std::env::var("KEN_HTML_SAMPLE") else { return };
        let got = extract(Path::new(&p)).unwrap();
        println!("title: {:?}\nchars: {}\n{}", got.title, got.text.len(), &got.text[..got.text.len().min(600)]);
        assert!(got.text.len() < 50_000);
    }
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
        assert_eq!(FileKind::from_path(Path::new("call.srt")), FileKind::Txt);
    }

    #[test]
    fn build_scripts_and_ui_markup_are_text() {
        for name in [
            "build.gradle.kts",
            "settings.gradle",
            "gradle.properties",
            "scripts/gen.mjs",
            "lib/x.cjs",
            "src/a.mts",
            "src/a.cts",
            "App.vue",
            "Common/UI/Custom/Party/PartyMemberChip.ui",
            "Server/Languages/en-US/server.lang",
            "Cargo.toml",
            "setup.cfg",
            "php.ini",
        ] {
            assert_eq!(FileKind::from_path(Path::new(name)), FileKind::Code, "{name}");
        }
        // Engine asset formats stay name-only.
        for name in ["Sword.blockymodel", "Swing.blockyanim", "Spark.particlespawner"] {
            assert_eq!(FileKind::from_path(Path::new(name)), FileKind::Binary, "{name}");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("PartyMemberChip.ui");
        std::fs::write(&path, "Group #NameCell { Anchor: (Width: 168); }\n").unwrap();
        assert!(extract(&path).unwrap().text.contains("NameCell"));
    }

    #[test]
    fn video_kind_mapping() {
        for ext in ["mp4", "mov", "m4v", "webm", "mkv", "avi", "MP4", "MoV", "wav", "mp3", "m4a", "flac", "ogg"] {
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
    fn url_shortcut_indexes_its_link() {
        assert_eq!(FileKind::from_path(Path::new("l/site.url")), FileKind::Url);
        assert_eq!(FileKind::from_path(Path::new("l/site.URL")), FileKind::Url);
        assert_eq!(FileKind::Url.as_str(), "url");
        assert!(FileKind::Url.has_content());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vendor.url");
        std::fs::write(
            &path,
            "[InternetShortcut]\r\nURL=https://langdonsoft.example/quotes\r\nIconIndex=0\r\n",
        )
        .unwrap();
        let out = extract(&path).unwrap();
        // Only the link — the INI scaffolding isn't worth indexing.
        assert_eq!(out.text, "https://langdonsoft.example/quotes");
    }

    #[test]
    fn drawio_classifies_and_extracts_labels() {
        assert_eq!(FileKind::from_path(Path::new("d/arch.drawio")), FileKind::Drawio);
        assert_eq!(FileKind::Drawio.as_str(), "drawio");
        assert!(FileKind::Drawio.has_content());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("arch.drawio");
        std::fs::write(
            &path,
            r#"<mxfile><diagram name="P1"><mxGraphModel><root>
            <mxCell id="2" value="Data Lake" vertex="1"/>
        </root></mxGraphModel></diagram></mxfile>"#,
        )
        .unwrap();
        let out = extract(&path).unwrap();
        assert!(out.text.contains("Data Lake"));
    }

    #[test]
    fn url_parsing_is_tolerant_and_optional() {
        assert_eq!(
            parse_internet_shortcut("[InternetShortcut]\nurl = https://x.example/a \n").as_deref(),
            Some("https://x.example/a")
        );
        // First URL key wins; a file with none extracts to nothing rather than
        // failing (a malformed shortcut is still a valid, listable file).
        assert_eq!(
            parse_internet_shortcut("URL=https://a.example\nURL=https://b.example").as_deref(),
            Some("https://a.example")
        );
        assert_eq!(parse_internet_shortcut("[InternetShortcut]\nIconIndex=0\n"), None);
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
