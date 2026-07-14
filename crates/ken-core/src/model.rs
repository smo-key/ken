//! Downloading the on-device Whisper model(s) from within the app.
//!
//! There is no hand-maintained model list here: the available models are
//! *discovered* at runtime by listing the whisper.cpp Hugging Face repo's
//! `ggml-*.bin` files, so new models the upstream repo ships appear without a
//! source change. A single named default — the recommended base English model
//! — is the one constant we keep, and it doubles as the offline fallback when
//! the listing can't be fetched.
//!
//! The network is isolated behind the [`ByteSource`] seam so the streaming,
//! progress, verify, and atomic-install logic is unit-tested against an
//! in-memory fake without ever touching the network. The pure pieces
//! (listing-parse, progress math, emit throttle, verify) carry the tests.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{transcript, Error, Result};

/// The whisper.cpp repo the models live in.
pub const REPO: &str = "ggerganov/whisper.cpp";

/// The recommended default model — a single named constant, not a curated
/// array. Kept in sync with the file `transcript.rs` looks for at use-time so a
/// download lands exactly where the transcriber expects it.
pub const RECOMMENDED_FILE: &str = transcript::MODEL_FILE;

/// The recommended model's known size, used only for the offline gate/display
/// and as the no-`Content-Length` verification floor; real downloads verify
/// against the server's advertised length. Approximate is fine here.
pub const RECOMMENDED_BYTES: u64 = 147_951_465;

/// The HF repo tree API — a JSON array of the repo's files, with per-file sizes.
fn tree_url() -> String {
    format!("https://huggingface.co/api/models/{REPO}/tree/main")
}

/// The direct download URL for one model file in the repo.
pub fn resolve_url(file: &str) -> String {
    format!("https://huggingface.co/{REPO}/resolve/main/{file}")
}

/// One downloadable model. Discovered at runtime (or the recommended fallback);
/// `id` is the file name, which is stable and unique within the repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: String,
    pub name: String,
    pub file: String,
    pub url: String,
    pub expected_bytes: u64,
    pub recommended: bool,
}

/// The recommended model, resolved from the stable default constant. This is
/// the offline fallback and the model the transcript feature gates on.
pub fn recommended() -> ModelSpec {
    ModelSpec {
        id: RECOMMENDED_FILE.to_string(),
        name: display_name(RECOMMENDED_FILE),
        file: RECOMMENDED_FILE.to_string(),
        url: resolve_url(RECOMMENDED_FILE),
        expected_bytes: RECOMMENDED_BYTES,
        recommended: true,
    }
}

/// A readable label derived from the file name (`ggml-base.en.bin` → "Base
/// (English)") — a derivation, not a maintained mapping, so unknown models
/// still get a sensible name.
fn display_name(file: &str) -> String {
    let tag = file
        .strip_prefix("ggml-")
        .unwrap_or(file)
        .strip_suffix(".bin")
        .unwrap_or(file);
    // `base.en` → size "base", lang "en"; `large-v3` → just the size.
    let (size, lang) = match tag.split_once('.') {
        Some((s, l)) => (s, Some(l)),
        None => (tag, None),
    };
    let mut label = capitalize(size);
    match lang {
        Some("en") => label.push_str(" (English)"),
        Some(other) => {
            label.push_str(" (");
            label.push_str(other);
            label.push(')');
        }
        None => {}
    }
    label
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

// ---------- runtime discovery (listing behind the seam) ----------

/// One entry of the HF tree API. LFS-tracked binaries carry their real size and
/// sha under `lfs`; the top-level `size` is the small pointer file for those.
#[derive(Deserialize)]
struct TreeEntry {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    lfs: Option<Lfs>,
}

#[derive(Deserialize)]
struct Lfs {
    #[serde(default)]
    size: u64,
}

/// Parse the HF tree JSON into model specs — pure, so it's tested without the
/// network. Keeps only `ggml-*.bin` files; prefers the LFS size (the real
/// bytes) over the pointer size. The recommended default is flagged.
pub fn parse_model_listing(json: &str) -> Result<Vec<ModelSpec>> {
    let entries: Vec<TreeEntry> = serde_json::from_str(json)
        .map_err(|e| Error::Other(format!("couldn't read the model listing: {e}")))?;
    let mut specs: Vec<ModelSpec> = entries
        .into_iter()
        .filter(|e| e.kind == "file" && is_model_file(&e.path))
        .map(|e| {
            let bytes = e.lfs.as_ref().map(|l| l.size).filter(|s| *s > 0).unwrap_or(e.size);
            ModelSpec {
                id: e.path.clone(),
                name: display_name(&e.path),
                file: e.path.clone(),
                url: resolve_url(&e.path),
                expected_bytes: bytes,
                recommended: e.path == RECOMMENDED_FILE,
            }
        })
        .collect();
    // Surface the recommended model first so the UI can pre-select it.
    specs.sort_by(|a, b| b.recommended.cmp(&a.recommended).then(a.file.cmp(&b.file)));
    Ok(specs)
}

fn is_model_file(path: &str) -> bool {
    path.starts_with("ggml-") && path.ends_with(".bin") && !path.contains('/')
}

/// Discover the available models by listing the repo. Falls back to just the
/// recommended model if the listing can't be fetched or parsed, so the feature
/// works offline. Guarantees the recommended model is present.
pub fn discover_models<S: ByteSource>(source: &S) -> Vec<ModelSpec> {
    let discovered = fetch_listing(source).and_then(|json| parse_model_listing(&json).ok());
    match discovered {
        Some(mut specs) if !specs.is_empty() => {
            if !specs.iter().any(|s| s.file == RECOMMENDED_FILE) {
                specs.insert(0, recommended());
            }
            specs
        }
        _ => vec![recommended()],
    }
}

fn fetch_listing<S: ByteSource>(source: &S) -> Option<String> {
    let (_, mut reader) = source.open(&tree_url()).ok()?;
    let mut buf = String::new();
    reader.read_to_string(&mut buf).ok()?;
    Some(buf)
}

// ---------- installed-state + target path ----------

/// Where a model file installs to. Derived from `transcript.rs`'s own path
/// resolution (its whisper dir) so downloads land exactly where the transcriber
/// looks — no second hardcoded copy of that join.
pub fn target_path(base_dir: &Path, spec: &ModelSpec) -> PathBuf {
    let whisper_dir = transcript::model_path(base_dir)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| base_dir.join("whisper"));
    whisper_dir.join(&spec.file)
}

/// Whether a model is installed and, if so, its on-disk size.
pub fn installed_size(base_dir: &Path, spec: &ModelSpec) -> Option<u64> {
    std::fs::metadata(target_path(base_dir, spec))
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len())
}

/// Delete an installed model file. Missing is not an error (idempotent remove).
pub fn remove(base_dir: &Path, spec: &ModelSpec) -> Result<()> {
    let path = target_path(base_dir, spec);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(&path, e)),
    }
}

// ---------- progress math + emit throttle (pure) ----------

/// Download completion as a 0–100 percent, clamped; unknown total → 0.
pub fn progress_percent(downloaded: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    ((downloaded.min(total) as u128 * 100) / total as u128) as u8
}

/// Rate-limits progress emits so a fast download doesn't flood the UI: fire on
/// the first sample, then only when the percent advances by a step or enough
/// time passes. Time is passed in (a monotonic millisecond clock) so the rule
/// is deterministic and testable.
pub struct ProgressThrottle {
    last_pct: i32,
    last_emit_ms: u64,
    min_pct_step: i32,
    min_interval_ms: u64,
}

impl Default for ProgressThrottle {
    fn default() -> Self {
        // ~1% or 250ms, whichever comes first.
        ProgressThrottle { last_pct: -1, last_emit_ms: 0, min_pct_step: 1, min_interval_ms: 250 }
    }
}

impl ProgressThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether this sample should be emitted, recording it when so.
    pub fn should_emit(&mut self, downloaded: u64, total: u64, now_ms: u64) -> bool {
        let pct = progress_percent(downloaded, total) as i32;
        let first = self.last_pct < 0;
        let stepped = pct - self.last_pct >= self.min_pct_step;
        let timed = now_ms.saturating_sub(self.last_emit_ms) >= self.min_interval_ms;
        if first || stepped || timed {
            self.last_pct = pct;
            self.last_emit_ms = now_ms;
            true
        } else {
            false
        }
    }
}

// ---------- streaming download → verify → atomic install ----------

/// The network seam: opens a URL and yields its `Content-Length` (if the server
/// gives one) plus a streaming reader over the body. The real implementation
/// does an HTTPS GET; tests supply an in-memory reader.
pub trait ByteSource {
    fn open(&self, url: &str) -> Result<(Option<u64>, Box<dyn Read + Send>)>;
}

/// Verify a finished download. Guards the real failure mode — a truncated or
/// interrupted transfer — by requiring the file match the length the server
/// promised; without a `Content-Length`, it must be at least the model's known
/// size.
pub fn verify_download(actual: u64, server_total: Option<u64>, spec: &ModelSpec) -> Result<()> {
    match server_total {
        Some(total) if actual != total => Err(Error::Other(format!(
            "the download was incomplete ({actual} of {total} bytes) — check your connection and try again"
        ))),
        Some(_) => Ok(()),
        None if actual < spec.expected_bytes => Err(Error::Other(format!(
            "the download was incomplete ({actual} bytes) — check your connection and try again"
        ))),
        None => Ok(()),
    }
}

/// Stream a model download to a temp file, reporting progress, verify it, then
/// atomically rename it into place. On any failure the temp file is removed so
/// a broken download never masquerades as an installed model. `on_progress`
/// receives `(downloaded, total)` for every chunk (the caller throttles emits)
/// and once more with the final size after a successful install.
pub fn download_to<S, P>(
    source: &S,
    spec: &ModelSpec,
    base_dir: &Path,
    mut on_progress: P,
) -> Result<()>
where
    S: ByteSource,
    P: FnMut(u64, u64),
{
    let target = target_path(base_dir, spec);
    let dir = target
        .parent()
        .ok_or_else(|| Error::Other("invalid model install path".into()))?
        .to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;

    let (server_total, mut reader) = source.open(&spec.url)?;
    let total = server_total.unwrap_or(spec.expected_bytes);
    // Distinct temp name so a concurrent install of a *different* model can't
    // collide; the caller already guards against two of the same id.
    let tmp = dir.join(format!(".{}.part", spec.file));

    let streamed = (|| -> Result<()> {
        let mut file = std::fs::File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
        let mut buf = [0u8; 64 * 1024];
        let mut downloaded: u64 = 0;
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| Error::Other(format!("download interrupted: {e}")))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| Error::io(&tmp, e))?;
            downloaded += n as u64;
            // Hold back the final 100% sample: it's emitted only once, *after* a
            // successful verify + install, so a 100% event unambiguously means
            // "installed" (never "downloaded but still verifying/failed").
            if downloaded < total {
                on_progress(downloaded, total);
            }
        }
        file.flush().map_err(|e| Error::io(&tmp, e))?;
        drop(file);
        let actual = std::fs::metadata(&tmp).map_err(|e| Error::io(&tmp, e))?.len();
        verify_download(actual, server_total, spec)
    })();

    match streamed {
        Ok(()) => {
            std::fs::rename(&tmp, &target).map_err(|e| Error::io(&target, e))?;
            let final_len = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(total);
            on_progress(final_len, final_len);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

// ---------- real HTTP source ----------

/// The production [`ByteSource`]: a blocking, streamed HTTPS GET via `ureq`
/// (rustls). This is the only network the app performs, kept isolated to this
/// module.
pub struct HttpSource;

impl ByteSource for HttpSource {
    fn open(&self, url: &str) -> Result<(Option<u64>, Box<dyn Read + Send>)> {
        let resp = ureq::get(url)
            .call()
            .map_err(|e| Error::Other(format!("couldn't reach the model server: {e}")))?;
        let total = resp
            .header("Content-Length")
            .and_then(|s| s.parse::<u64>().ok());
        Ok((total, Box::new(resp.into_reader())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Mutex;

    /// An in-memory [`ByteSource`] — the network seam for tests. Serves fixed
    /// bytes with an optional advertised length so streaming/verify/rename run
    /// with no network.
    struct FakeSource {
        body: Vec<u8>,
        content_length: Option<u64>,
        /// A canned listing JSON returned for the tree URL, if set.
        listing: Option<String>,
    }

    impl ByteSource for FakeSource {
        fn open(&self, url: &str) -> Result<(Option<u64>, Box<dyn Read + Send>)> {
            if url.contains("/api/models/") {
                let json = self
                    .listing
                    .clone()
                    .ok_or_else(|| Error::Other("no listing".into()))?;
                return Ok((None, Box::new(Cursor::new(json.into_bytes()))));
            }
            Ok((self.content_length, Box::new(Cursor::new(self.body.clone()))))
        }
    }

    fn spec_for(file: &str, bytes: u64) -> ModelSpec {
        ModelSpec {
            id: file.into(),
            name: display_name(file),
            file: file.into(),
            url: resolve_url(file),
            expected_bytes: bytes,
            recommended: file == RECOMMENDED_FILE,
        }
    }

    #[test]
    fn recommended_default_is_the_base_english_model() {
        let r = recommended();
        assert_eq!(r.file, "ggml-base.en.bin");
        assert!(r.recommended);
        assert!(r.url.contains("ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin"));
        assert_eq!(r.name, "Base (English)");
    }

    #[test]
    fn target_path_matches_transcript_resolution() {
        let base = Path::new("/data/ken");
        // The download must land exactly where the transcriber looks.
        assert_eq!(target_path(base, &recommended()), transcript::model_path(base));
    }

    #[test]
    fn listing_parse_keeps_ggml_bins_prefers_lfs_size_and_flags_recommended() {
        let json = r#"[
            {"type":"file","path":"README.md","size":1200},
            {"type":"file","path":"ggml-tiny.en.bin","size":133,"lfs":{"size":77704715}},
            {"type":"file","path":"ggml-base.en.bin","size":133,"lfs":{"size":147951465}},
            {"type":"directory","path":"examples"},
            {"type":"file","path":"models/extra.txt","size":10}
        ]"#;
        let specs = parse_model_listing(json).unwrap();
        assert_eq!(specs.len(), 2, "only top-level ggml-*.bin files");
        // Recommended sorts first for pre-selection.
        assert_eq!(specs[0].file, "ggml-base.en.bin");
        assert!(specs[0].recommended);
        // Real (LFS) size, not the pointer size.
        assert_eq!(specs[0].expected_bytes, 147951465);
        assert!(!specs[1].recommended);
        assert_eq!(specs[1].expected_bytes, 77704715);
    }

    #[test]
    fn discover_falls_back_to_recommended_when_listing_unavailable() {
        let source = FakeSource { body: vec![], content_length: None, listing: None };
        let specs = discover_models(&source);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].file, RECOMMENDED_FILE);
    }

    #[test]
    fn discover_uses_the_listing_when_present() {
        let listing = r#"[
            {"type":"file","path":"ggml-base.en.bin","size":1,"lfs":{"size":147951465}},
            {"type":"file","path":"ggml-small.en.bin","size":1,"lfs":{"size":487601967}}
        ]"#;
        let source = FakeSource {
            body: vec![],
            content_length: None,
            listing: Some(listing.into()),
        };
        let specs = discover_models(&source);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].file, "ggml-base.en.bin");
    }

    #[test]
    fn progress_percent_clamps_and_handles_unknown_total() {
        assert_eq!(progress_percent(0, 0), 0);
        assert_eq!(progress_percent(50, 200), 25);
        assert_eq!(progress_percent(200, 200), 100);
        assert_eq!(progress_percent(300, 200), 100); // clamped
    }

    #[test]
    fn throttle_emits_on_first_then_step_or_interval() {
        let mut t = ProgressThrottle::new();
        assert!(t.should_emit(0, 100, 0)); // first always emits
        assert!(!t.should_emit(0, 100, 10)); // same %, <250ms
        assert!(t.should_emit(1, 100, 20)); // +1% step
        assert!(!t.should_emit(1, 100, 30)); // same %, <250ms since last
        assert!(t.should_emit(1, 100, 300)); // >=250ms since last emit
    }

    #[test]
    fn verify_accepts_a_complete_download() {
        let spec = spec_for(RECOMMENDED_FILE, 1000);
        assert!(verify_download(1000, Some(1000), &spec).is_ok());
    }

    #[test]
    fn verify_rejects_a_truncated_download() {
        let spec = spec_for(RECOMMENDED_FILE, 1000);
        assert!(verify_download(600, Some(1000), &spec).is_err());
    }

    #[test]
    fn verify_falls_back_to_expected_size_without_content_length() {
        let spec = spec_for(RECOMMENDED_FILE, 1000);
        assert!(verify_download(999, None, &spec).is_err());
        assert!(verify_download(1000, None, &spec).is_ok());
    }

    #[test]
    fn download_streams_verifies_and_installs_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let body = vec![7u8; 5000];
        let source = FakeSource {
            body: body.clone(),
            content_length: Some(5000),
            listing: None,
        };
        let spec = spec_for(RECOMMENDED_FILE, 5000);

        let calls = Mutex::new(Vec::<(u64, u64)>::new());
        download_to(&source, &spec, base, |d, tot| calls.lock().unwrap().push((d, tot))).unwrap();

        // File installed at exactly the transcriber's path, no leftover temp.
        let installed = target_path(base, &spec);
        assert_eq!(std::fs::read(&installed).unwrap(), body);
        assert_eq!(installed_size(base, &spec), Some(5000));
        assert!(!installed.with_file_name(format!(".{}.part", spec.file)).exists());

        let calls = calls.lock().unwrap();
        assert!(!calls.is_empty());
        // 100% fires exactly once, and last — only after the atomic install, so
        // the UI can treat a 100% event as "installed".
        assert_eq!(calls.last().copied(), Some((5000, 5000)));
        assert_eq!(
            calls.iter().filter(|(d, t)| d >= t).count(),
            1,
            "the terminal 100% sample is emitted only post-install"
        );
    }

    #[test]
    fn download_cleans_up_temp_and_installs_nothing_on_verify_failure() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        // Server promises 5000 but only 3000 arrive → verify fails.
        let source = FakeSource {
            body: vec![1u8; 3000],
            content_length: Some(5000),
            listing: None,
        };
        let spec = spec_for(RECOMMENDED_FILE, 5000);

        let err = download_to(&source, &spec, base, |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("incomplete"));
        assert_eq!(installed_size(base, &spec), None);
        let tmp = target_path(base, &spec).with_file_name(format!(".{}.part", spec.file));
        assert!(!tmp.exists(), "temp file cleaned up");
    }

    #[test]
    fn installed_size_and_remove_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let spec = recommended();
        assert_eq!(installed_size(base, &spec), None);

        let path = target_path(base, &spec);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"model-bytes").unwrap();
        assert_eq!(installed_size(base, &spec), Some(11));

        remove(base, &spec).unwrap();
        assert_eq!(installed_size(base, &spec), None);
        // Removing an absent model is a no-op, not an error.
        assert!(remove(base, &spec).is_ok());
    }
}
