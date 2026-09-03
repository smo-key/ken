//! Pure text chunking for the semantic index. No I/O, no database access —
//! this module only turns already-read file text into a list of [`Chunk`]s
//! according to an [`IndexProfile`]. Callers (the ingest pipeline, `db.rs`)
//! are responsible for reading files and persisting the result.
//!
//! Two chunking strategies:
//!
//! * **Prose** — heading/paragraph-aware. Markdown ATX headings (`# ...`)
//!   always start a new chunk; otherwise paragraphs (blank-line separated)
//!   are packed up to `target_tokens`, with `overlap_pct` of the previous
//!   chunk's tail carried into the next chunk so a search hit near a chunk
//!   boundary still has surrounding context.
//! * **Code** — line-based blocks of ~`target_tokens`, no overlap. Splitting
//!   mid-statement is acceptable; the goal is bounded, roughly-uniform chunks
//!   for embedding, not syntactic correctness.
//!
//! Token counts are estimated as `chars / 4` — cheap and stable. Exactness
//! doesn't matter here, only consistency between index time and any future
//! re-chunking. `content_hash` is an xxHash64 of the chunk text, used by
//! `db::upsert_chunks` to diff against the previous chunk set so an unchanged
//! file costs zero re-embeddings.

use serde::{Deserialize, Serialize};

/// Chunking strategy for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkMode {
    /// Heading/paragraph-aware splitting with overlap. Markdown, plain text,
    /// extracted PDF text.
    Prose,
    /// Line-based fixed-size blocks, no overlap. Source code and other
    /// structured/dense text.
    Code,
    /// Never chunked or embedded. The file still keyword-searches (that's
    /// driven by kenignore's tier, not this), but produces zero semantic
    /// chunks — used by project-profiler pattern entries (e.g. `*.log`) to
    /// opt noisy/generated text out of embedding without excluding it
    /// entirely.
    Skip,
}

/// How a file should be split into chunks. Per-path selection lives in
/// [`IndexProfile::default_for`]; callers may also construct one directly to
/// override the default (e.g. a future user setting).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexProfile {
    pub mode: ChunkMode,
    /// Target chunk size, in estimated tokens (`chars / 4`).
    pub target_tokens: usize,
    /// Fraction (0.0-1.0) of `target_tokens` worth of trailing text from a
    /// chunk that is repeated at the start of the next chunk. Only applies
    /// to prose mode; code mode uses 0.0.
    pub overlap_pct: f32,
}

impl Default for IndexProfile {
    fn default() -> Self {
        IndexProfile {
            mode: ChunkMode::Prose,
            target_tokens: 350,
            overlap_pct: 0.15,
        }
    }
}

/// Prose-ish extensions: markdown, plain text, extracted PDF text. Module
/// level (not just local to `default_for`) so other modules can classify by
/// the same taxonomy instead of duplicating it — project-profiler's doc-ratio
/// scan and default chunking table reuse this exact list.
pub const PROSE_EXTS: &[&str] = &["md", "mdx", "txt", "pdf"];
/// Source-code and other structured/dense-text extensions. See [`PROSE_EXTS`].
pub const CODE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "c", "h", "cc", "cpp",
    "hpp", "cs", "rb", "php", "swift", "kt", "kts", "sql", "sh", "bash", "ps1", "toml", "yaml",
    "yml", "json", "css", "scss", "html", "svelte", "vue",
];

impl IndexProfile {
    /// v1 default profile selection by file extension: prose-ish formats
    /// (markdown, plain text, extracted PDF text) get the prose profile;
    /// source-code extensions get the code profile; anything else falls back
    /// to prose defaults.
    pub fn default_for(rel_path: &str) -> IndexProfile {
        let ext = rel_path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if PROSE_EXTS.contains(&ext.as_str()) {
            IndexProfile {
                mode: ChunkMode::Prose,
                target_tokens: 350,
                overlap_pct: 0.15,
            }
        } else if CODE_EXTS.contains(&ext.as_str()) {
            IndexProfile {
                mode: ChunkMode::Code,
                target_tokens: 500,
                overlap_pct: 0.0,
            }
        } else {
            IndexProfile::default()
        }
    }
}

/// One chunk of a file's text, ready to be persisted (`db::upsert_chunks`
/// assigns `id`/`path`/`tier`) and embedded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// 0-based position of this chunk within the file.
    pub seq: usize,
    pub text: String,
    /// Estimated token count (`text.len() / 4`, minimum 1).
    pub token_est: usize,
    /// xxHash64 of `text`, hex-encoded. Used for incremental diffing.
    pub content_hash: String,
}

/// Hard cap on chunks per file. Guards against pathological inputs
/// (generated/minified files, huge data dumps) blowing up chunk count and,
/// downstream, embedding cost.
const CHUNK_CAP: usize = 200;

/// Split `text` (the contents of `rel_path`) into chunks per `profile`.
/// Pure and infallible: empty/whitespace-only text yields no chunks.
pub fn chunk_file(rel_path: &str, text: &str, profile: &IndexProfile) -> Vec<Chunk> {
    // Reserved for future per-path overrides (e.g. path-pattern profiles);
    // profile selection itself happens in the caller via `default_for`.
    let _ = rel_path;

    if text.trim().is_empty() {
        return Vec::new();
    }

    let pieces = match profile.mode {
        ChunkMode::Prose => chunk_prose(text, profile),
        ChunkMode::Code => chunk_code(text, profile),
        ChunkMode::Skip => Vec::new(),
    };

    pieces
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .take(CHUNK_CAP)
        .enumerate()
        .map(|(seq, text)| {
            let token_est = (text.len() / 4).max(1);
            let content_hash = hash_text(&text);
            Chunk {
                seq,
                text,
                token_est,
                content_hash,
            }
        })
        .collect()
}

fn hash_text(text: &str) -> String {
    let h = twox_hash::XxHash64::oneshot(0, text.as_bytes());
    format!("{h:016x}")
}

/// Split text into paragraph/heading blocks: blank lines separate
/// paragraphs; any line starting with `#` (markdown ATX heading) is always
/// its own block, regardless of surrounding blank lines.
fn split_prose_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !current.trim().is_empty() {
                blocks.push(current.trim().to_string());
            }
            current.clear();
            continue;
        }
        if trimmed.starts_with('#') {
            if !current.trim().is_empty() {
                blocks.push(current.trim().to_string());
            }
            current.clear();
            blocks.push(trimmed.to_string());
            continue;
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.trim().is_empty() {
        blocks.push(current.trim().to_string());
    }
    blocks
}

/// Take the last `n` bytes of `s`, adjusted forward to the nearest char
/// boundary so the result is always valid UTF-8.
fn tail(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut start = s.len() - n;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

fn chunk_prose(text: &str, profile: &IndexProfile) -> Vec<String> {
    let target_chars = (profile.target_tokens * 4).max(1);
    let overlap_chars = ((target_chars as f32) * profile.overlap_pct).round() as usize;

    let blocks = split_prose_blocks(text);
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    for block in blocks {
        let is_heading = block.starts_with('#');
        let would_exceed = !current.is_empty() && current.len() + block.len() + 2 > target_chars;
        // Headings always start a fresh chunk (no merging a new section into
        // the tail of the previous one), but never against an empty buffer.
        let force_boundary = is_heading && !current.is_empty();

        if would_exceed || force_boundary {
            chunks.push(current.clone());
            current = if overlap_chars > 0 && !force_boundary {
                tail(&current, overlap_chars)
            } else {
                String::new()
            };
        }

        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(&block);
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

fn chunk_code(text: &str, profile: &IndexProfile) -> Vec<String> {
    let target_chars = (profile.target_tokens * 4).max(1);
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in text.lines() {
        if !current.is_empty() && current.len() + line.len() + 1 > target_chars {
            chunks.push(current.clone());
            current.clear();
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_for_picks_prose_for_markdown() {
        let p = IndexProfile::default_for("docs/readme.md");
        assert_eq!(p.mode, ChunkMode::Prose);
        assert_eq!(p.target_tokens, 350);
        assert!((p.overlap_pct - 0.15).abs() < f32::EPSILON);
    }

    #[test]
    fn default_for_picks_code_for_rust() {
        let p = IndexProfile::default_for("crates/ken-core/src/lib.rs");
        assert_eq!(p.mode, ChunkMode::Code);
        assert_eq!(p.target_tokens, 500);
        assert_eq!(p.overlap_pct, 0.0);
    }

    #[test]
    fn skip_mode_produces_no_chunks() {
        let profile = IndexProfile { mode: ChunkMode::Skip, target_tokens: 350, overlap_pct: 0.0 };
        assert!(chunk_file("noisy.log", "line one\nline two\nline three\n", &profile).is_empty());
    }

    #[test]
    fn default_for_falls_back_to_prose_for_unknown_extension() {
        let p = IndexProfile::default_for("weird_file.xyz123");
        assert_eq!(p, IndexProfile::default());
    }

    #[test]
    fn empty_text_yields_no_chunks() {
        let profile = IndexProfile::default();
        assert!(chunk_file("empty.md", "   \n\n  ", &profile).is_empty());
    }

    #[test]
    fn chunking_is_deterministic() {
        let text = "# Heading\n\nSome paragraph text that repeats a fair bit to build up length. "
            .repeat(20);
        let profile = IndexProfile::default_for("notes.md");
        let a = chunk_file("notes.md", &text, &profile);
        let b = chunk_file("notes.md", &text, &profile);
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn prose_mode_respects_heading_boundaries() {
        let text = "Intro paragraph one.\n\nIntro paragraph two continues the thought.\n\n\
                     # Section Two\n\nBody of section two goes here.";
        let profile = IndexProfile {
            mode: ChunkMode::Prose,
            target_tokens: 350,
            overlap_pct: 0.15,
        };
        let chunks = chunk_file("doc.md", text, &profile);
        // The heading starts its own chunk rather than being folded into the
        // middle of the previous section's chunk.
        assert!(chunks
            .iter()
            .any(|c| c.text.trim_start().starts_with("# Section Two")));
        // And it is not glued onto the tail of the intro paragraphs either.
        assert!(!chunks[0].text.contains("# Section Two"));
    }

    #[test]
    fn prose_mode_produces_overlap_between_adjacent_chunks() {
        let mut text = String::new();
        for i in 0..30 {
            text.push_str(&format!(
                "Paragraph number {i} has some unique filler content padding it out.\n\n"
            ));
        }
        let profile = IndexProfile {
            mode: ChunkMode::Prose,
            target_tokens: 50, // target_chars = 200
            overlap_pct: 0.2,  // overlap_chars = 40
        };
        let chunks = chunk_file("doc.md", &text, &profile);
        assert!(
            chunks.len() >= 2,
            "expected multiple chunks, got {}",
            chunks.len()
        );
        let overlap_chars = 40usize;
        assert!(chunks[0].text.len() >= overlap_chars);
        let expected_tail = &chunks[0].text[chunks[0].text.len() - overlap_chars..];
        assert!(
            chunks[1].text.starts_with(expected_tail),
            "chunk 1 should open with chunk 0's trailing {overlap_chars} chars;\n\
             chunk0={:?}\nchunk1={:?}",
            chunks[0].text,
            chunks[1].text
        );
    }

    #[test]
    fn code_mode_splits_into_line_blocks_without_overlap() {
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("let x{i} = {i}; // padding line to build length\n"));
        }
        let profile = IndexProfile {
            mode: ChunkMode::Code,
            target_tokens: 50,
            overlap_pct: 0.0,
        };
        let chunks = chunk_file("main.rs", &text, &profile);
        assert!(chunks.len() >= 2, "expected multiple chunks");
        let last_line_of_first = chunks[0].text.lines().last().unwrap();
        let first_line_of_second = chunks[1].text.lines().next().unwrap();
        assert_ne!(
            last_line_of_first, first_line_of_second,
            "code mode must not repeat lines across chunk boundaries"
        );
    }

    #[test]
    fn chunking_enforces_two_hundred_chunk_cap() {
        let mut text = String::new();
        for i in 0..500 {
            text.push_str(&format!("line {i} of a very large generated file padding content\n"));
        }
        let profile = IndexProfile {
            mode: ChunkMode::Code,
            target_tokens: 5, // tiny target -> far more than 200 blocks pre-cap
            overlap_pct: 0.0,
        };
        let chunks = chunk_file("generated.rs", &text, &profile);
        assert_eq!(chunks.len(), CHUNK_CAP);
        assert_eq!(chunks.last().unwrap().seq, CHUNK_CAP - 1);
    }

    #[test]
    fn content_hash_is_stable_and_reflects_text() {
        let profile = IndexProfile::default();
        let a = chunk_file(
            "a.md",
            "Hello world, this is a stable chunk of prose text.",
            &profile,
        );
        let b = chunk_file(
            "a.md",
            "Hello world, this is a stable chunk of prose text.",
            &profile,
        );
        assert_eq!(a[0].content_hash, b[0].content_hash);

        let c = chunk_file(
            "a.md",
            "Hello world, this is a DIFFERENT chunk of prose text.",
            &profile,
        );
        assert_ne!(a[0].content_hash, c[0].content_hash);
    }

    #[test]
    fn token_estimate_is_len_over_four() {
        let profile = IndexProfile::default();
        let text = "0123456789"; // 10 chars
        let chunks = chunk_file("a.txt", text, &profile);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].token_est, 10 / 4);
    }
}
