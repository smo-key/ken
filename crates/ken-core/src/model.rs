//! Downloading the on-device model(s) from within the app.
//!
//! The available models are a small, curated [`catalog`] rather than a runtime
//! discovery of the upstream repo: each entry carries a [`ModelCategory`]
//! (Transcription / Language), a [`ModelTier`] (Recommended / Advanced), a
//! Settings blurb, and its download identity ([`ModelSpec`]). The recommended
//! transcription model doubles as the offline fallback and the model the
//! transcript feature gates on. A per-category choice is persisted machine-wide
//! in `base_dir/models/selection.json` ([`ModelSelection`]).
//!
//! The network is isolated behind the [`ByteSource`] seam so the streaming,
//! progress, verify, and atomic-install logic is unit-tested against an
//! in-memory fake without ever touching the network. The pure pieces (catalog,
//! progress math, emit throttle, verify) carry the tests.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

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

/// The direct download URL for one model file in the repo.
pub fn resolve_url(file: &str) -> String {
    format!("https://huggingface.co/{REPO}/resolve/main/{file}")
}

/// The advanced transcription model — Whisper Large v3 Turbo.
pub const WHISPER_LARGE_TURBO_FILE: &str = "ggml-large-v3-turbo.bin";
/// Display/offline-floor size only (~1.6 GB); real downloads verify against the
/// server's Content-Length.
pub const WHISPER_LARGE_TURBO_BYTES: u64 = 1_624_555_275;

// ---------- Language models (spec §1 / §10 "Answers & Map") ----------

/// Recommended answers/Map model: Qwen3-4B-Instruct-2507, Q4_K_M GGUF (~2.5 GB).
/// The official `Qwen/...GGUF` repo is gated (needs auth), so we serve the
/// public `unsloth` mirror of the identical quant — Ken downloads without
/// credentials.
pub const LANG_4B_FILE: &str = "Qwen3-4B-Instruct-2507-Q4_K_M.gguf";
pub const LANG_4B_URL: &str = "https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF/resolve/main/Qwen3-4B-Instruct-2507-Q4_K_M.gguf";
pub const LANG_4B_BYTES: u64 = 2_497_281_120;

/// Advanced answers/Map model: Qwen3-8B, Q4_K_M GGUF (~5 GB), official repo.
pub const LANG_8B_FILE: &str = "Qwen3-8B-Q4_K_M.gguf";
pub const LANG_8B_URL: &str = "https://huggingface.co/Qwen/Qwen3-8B-GGUF/resolve/main/Qwen3-8B-Q4_K_M.gguf";
pub const LANG_8B_BYTES: u64 = 5_027_783_488;

fn lang_recommended_spec() -> ModelSpec {
    ModelSpec {
        id: LANG_4B_FILE.to_string(),
        name: "Qwen3 4B".to_string(),
        file: LANG_4B_FILE.to_string(),
        url: LANG_4B_URL.to_string(),
        expected_bytes: LANG_4B_BYTES,
        recommended: true,
    }
}

fn lang_advanced_spec() -> ModelSpec {
    ModelSpec {
        id: LANG_8B_FILE.to_string(),
        name: "Qwen3 8B".to_string(),
        file: LANG_8B_FILE.to_string(),
        url: LANG_8B_URL.to_string(),
        expected_bytes: LANG_8B_BYTES,
        recommended: false,
    }
}

/// One downloadable model's download identity. `id` is the file name, which is
/// stable and unique within the repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: String,
    pub name: String,
    pub file: String,
    pub url: String,
    pub expected_bytes: u64,
    pub recommended: bool,
}

/// Which use a model serves. Serialized lowercase for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelCategory {
    Transcription,
    Language,
}

/// The two tiers offered per category: the safe default and the heavier option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTier {
    Recommended,
    Advanced,
}

/// One curated, downloadable model with its category, tier, and Settings blurb.
/// `spec` is the unchanged download identity (id/file/url/size/recommended).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub category: ModelCategory,
    pub tier: ModelTier,
    pub blurb: &'static str,
    pub spec: ModelSpec,
}

fn spec(file: &str, name: &str, bytes: u64, recommended: bool) -> ModelSpec {
    ModelSpec {
        id: file.to_string(),
        name: name.to_string(),
        file: file.to_string(),
        url: resolve_url(file),
        expected_bytes: bytes,
        recommended,
    }
}

/// The transcription pair. Recommended = Whisper Base (English); Advanced =
/// Whisper Large v3 Turbo.
fn transcription_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry {
            category: ModelCategory::Transcription,
            tier: ModelTier::Recommended,
            blurb: "fast, accurate for meetings",
            spec: spec(RECOMMENDED_FILE, "Whisper Base (English)", RECOMMENDED_BYTES, true),
        },
        CatalogEntry {
            category: ModelCategory::Transcription,
            tier: ModelTier::Advanced,
            blurb: "best accuracy, understands more languages, slower",
            spec: spec(WHISPER_LARGE_TURBO_FILE, "Whisper Large v3 Turbo", WHISPER_LARGE_TURBO_BYTES, false),
        },
    ]
}

/// The curated "Answers & Map" language models (spec §1 / §10) — the seam
/// `catalog()` concatenates. Recommended = Qwen3 4B (instant); Advanced =
/// Qwen3 8B (smarter, heavier).
fn language_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry {
            category: ModelCategory::Language,
            tier: ModelTier::Recommended,
            blurb: "instant answers, builds your map",
            spec: lang_recommended_spec(),
        },
        CatalogEntry {
            category: ModelCategory::Language,
            tier: ModelTier::Advanced,
            blurb: "smarter answers, needs more memory",
            spec: lang_advanced_spec(),
        },
    ]
}

/// Every curated model, in display order (Transcription, then Language).
pub fn catalog() -> Vec<CatalogEntry> {
    let mut entries = transcription_catalog();
    entries.extend(language_catalog());
    entries
}

/// The specs in one category, in catalog order.
pub fn category_specs(category: ModelCategory) -> Vec<ModelSpec> {
    catalog()
        .into_iter()
        .filter(|e| e.category == category)
        .map(|e| e.spec)
        .collect()
}

/// Locate a spec by its file-name id across the whole catalog.
pub fn find_spec(id: &str) -> Option<ModelSpec> {
    catalog().into_iter().map(|e| e.spec).find(|s| s.id == id)
}

/// The recommended transcription model — the offline fallback and the model the
/// transcript feature gates on when nothing is selected/installed.
pub fn recommended() -> ModelSpec {
    category_specs(ModelCategory::Transcription)
        .into_iter()
        .find(|s| s.recommended)
        .expect("transcription catalog has a recommended entry")
}

/// A readable label derived from the file name (`ggml-base.en.bin` → "Base
/// (English)") — a derivation, not a maintained mapping, so unknown models
/// still get a sensible name. Retained for building specs from a bare file id.
#[allow(dead_code)]
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

// ---------- persisted per-category selection ----------

/// Machine-level, per-category model selection. Persisted under
/// `base_dir/models/selection.json` (models install machine-wide, so the choice
/// is machine-wide too). Best-effort like the registry: missing/corrupt → default.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelSelection {
    #[serde(default)]
    pub transcription: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
}

fn selection_path(base_dir: &Path) -> PathBuf {
    base_dir.join("models").join("selection.json")
}

impl ModelSelection {
    pub fn load(base_dir: &Path) -> ModelSelection {
        match std::fs::read_to_string(selection_path(base_dir)) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => ModelSelection::default(),
        }
    }

    pub fn save(&self, base_dir: &Path) -> Result<()> {
        let path = selection_path(base_dir);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| Error::Other(e.to_string()))?;
        std::fs::write(&path, json + "\n").map_err(|e| Error::io(&path, e))
    }

    fn get(&self, category: ModelCategory) -> Option<&str> {
        match category {
            ModelCategory::Transcription => self.transcription.as_deref(),
            ModelCategory::Language => self.language.as_deref(),
        }
    }

    fn set(&mut self, category: ModelCategory, id: &str) {
        match category {
            ModelCategory::Transcription => self.transcription = Some(id.to_string()),
            ModelCategory::Language => self.language = Some(id.to_string()),
        }
    }
}

/// The spec the UI treats as selected for a category: the persisted choice if it
/// is a real catalog entry, else the category's Recommended.
pub fn selected(base_dir: &Path, category: ModelCategory) -> ModelSpec {
    let specs = category_specs(category);
    if let Some(id) = ModelSelection::load(base_dir).get(category) {
        if let Some(hit) = specs.iter().find(|s| s.id == id) {
            return hit.clone();
        }
    }
    specs
        .into_iter()
        .find(|s| s.recommended)
        .expect("every category has a recommended entry")
}

/// The on-disk path of the model to actually USE for a category: the selected
/// model if installed, else any installed model in the category, else None.
pub fn selected_model_path(base_dir: &Path, category: ModelCategory) -> Option<PathBuf> {
    let chosen = selected(base_dir, category);
    if installed_size(base_dir, &chosen).is_some() {
        return Some(target_path(base_dir, &chosen));
    }
    category_specs(category)
        .into_iter()
        .find(|s| installed_size(base_dir, s).is_some())
        .map(|s| target_path(base_dir, &s))
}

/// Persist a category's selection.
pub fn set_selected(base_dir: &Path, category: ModelCategory, id: &str) -> Result<()> {
    let mut sel = ModelSelection::load(base_dir);
    sel.set(category, id);
    sel.save(base_dir)
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
    }

    impl ByteSource for FakeSource {
        fn open(&self, _url: &str) -> Result<(Option<u64>, Box<dyn Read + Send>)> {
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
        assert_eq!(r.name, "Whisper Base (English)");
    }

    #[test]
    fn catalog_has_the_transcription_pair() {
        let cat = catalog();
        let trans: Vec<_> = cat
            .iter()
            .filter(|e| e.category == ModelCategory::Transcription)
            .collect();
        assert_eq!(trans.len(), 2, "one recommended + one advanced transcription model");
        let rec = trans.iter().find(|e| e.tier == ModelTier::Recommended).unwrap();
        assert_eq!(rec.spec.file, "ggml-base.en.bin");
        assert_eq!(rec.spec.name, "Whisper Base (English)");
        assert_eq!(rec.spec.expected_bytes, 147_951_465);
        assert!(rec.spec.recommended);
        let adv = trans.iter().find(|e| e.tier == ModelTier::Advanced).unwrap();
        assert_eq!(adv.spec.file, "ggml-large-v3-turbo.bin");
        assert_eq!(adv.spec.name, "Whisper Large v3 Turbo");
        assert!(!adv.spec.recommended);
        // Blurbs are the exact Settings copy.
        assert_eq!(rec.blurb, "fast, accurate for meetings");
        assert_eq!(adv.blurb, "best accuracy, understands more languages, slower");
    }

    #[test]
    fn language_catalog_has_qwen3_4b_and_8b() {
        let lang: Vec<_> = catalog()
            .into_iter()
            .filter(|e| e.category == ModelCategory::Language)
            .collect();
        assert_eq!(lang.len(), 2);
        let rec = lang.iter().find(|e| e.tier == ModelTier::Recommended).unwrap();
        assert_eq!(rec.spec.file, "Qwen3-4B-Instruct-2507-Q4_K_M.gguf");
        assert_eq!(rec.spec.expected_bytes, 2_497_281_120);
        assert!(rec.spec.url.starts_with("https://huggingface.co/unsloth/"));
        assert_eq!(rec.blurb, "instant answers, builds your map");
        let adv = lang.iter().find(|e| e.tier == ModelTier::Advanced).unwrap();
        assert_eq!(adv.spec.file, "Qwen3-8B-Q4_K_M.gguf");
        assert_eq!(adv.spec.expected_bytes, 5_027_783_488);
        assert_eq!(adv.blurb, "smarter answers, needs more memory");

        // With no selection.json in a fresh base_dir, selected(Language) defaults
        // to the recommended 4B; nothing installed → no loadable path.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            selected(dir.path(), ModelCategory::Language).file,
            rec.spec.file
        );
        assert_eq!(
            selected_model_path(dir.path(), ModelCategory::Language),
            None
        );
    }

    #[test]
    fn recommended_still_resolves_to_base_english() {
        let r = recommended();
        assert_eq!(r.file, "ggml-base.en.bin");
        assert!(r.recommended);
        assert!(r.url.contains("ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin"));
    }

    #[test]
    fn find_spec_locates_catalog_models_and_rejects_unknown() {
        assert_eq!(find_spec("ggml-large-v3-turbo.bin").unwrap().name, "Whisper Large v3 Turbo");
        assert!(find_spec("ggml-nonsense.bin").is_none());
    }

    #[test]
    fn selected_defaults_to_recommended_then_honours_a_valid_choice() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        // No selection saved → the category's Recommended.
        assert_eq!(selected(base, ModelCategory::Transcription).file, "ggml-base.en.bin");
        // A valid choice persists and is returned.
        set_selected(base, ModelCategory::Transcription, "ggml-large-v3-turbo.bin").unwrap();
        assert_eq!(selected(base, ModelCategory::Transcription).file, "ggml-large-v3-turbo.bin");
        // An unknown persisted id degrades back to Recommended (never a broken spec).
        set_selected(base, ModelCategory::Transcription, "ggml-bogus.bin").unwrap();
        assert_eq!(selected(base, ModelCategory::Transcription).file, "ggml-base.en.bin");
    }

    #[test]
    fn selected_model_path_prefers_selection_then_any_installed() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        // Nothing installed → None (gating shows the download help).
        assert!(selected_model_path(base, ModelCategory::Transcription).is_none());

        // Install the ADVANCED model only, but leave the selection at Recommended.
        let adv = find_spec("ggml-large-v3-turbo.bin").unwrap();
        let p = target_path(base, &adv);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"weights").unwrap();
        // Selection (base.en) isn't installed → fall back to the installed advanced one.
        assert_eq!(selected_model_path(base, ModelCategory::Transcription), Some(p.clone()));

        // Now select the installed advanced model → its path.
        set_selected(base, ModelCategory::Transcription, "ggml-large-v3-turbo.bin").unwrap();
        assert_eq!(selected_model_path(base, ModelCategory::Transcription), Some(p));
    }

    #[test]
    fn model_selection_roundtrips_and_defaults_on_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        set_selected(base, ModelCategory::Transcription, "ggml-large-v3-turbo.bin").unwrap();
        let loaded = ModelSelection::load(base);
        assert_eq!(loaded.transcription.as_deref(), Some("ggml-large-v3-turbo.bin"));
        // Corrupt file → defaults, never an error.
        std::fs::write(base.join("models").join("selection.json"), "{ not json").unwrap();
        assert_eq!(ModelSelection::load(base), ModelSelection::default());
    }

    #[test]
    fn target_path_matches_transcript_resolution() {
        let base = Path::new("/data/ken");
        // The download must land exactly where the transcriber looks.
        assert_eq!(target_path(base, &recommended()), transcript::model_path(base));
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
