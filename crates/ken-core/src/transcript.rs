//! Video transcripts: locate an adjacent transcript (a `.vtt`, or a Teams/Zoom
//! `.docx` export) or generate one on-device with Whisper, and normalise it to
//! WebVTT. The pure pieces here — timestamp formatting, WebVTT emission and
//! escaping, docx→VTT conversion, the fuzzy adjacent-file matcher, and the
//! ffmpeg/model availability gate — carry the logic and the tests; the ffmpeg
//! and Whisper calls are thin, untested shells over those.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::project::Project;
use crate::{Error, Result};

/// Is automatic on-device transcription during indexing enabled for this
/// project?
///
/// Persisted like the other per-project toggles — `project.json` extra
/// `"transcribeVideosOnIndex"` — but OFF by default: Whisper transcription is
/// slow and CPU-heavy, so it only runs when the user opts in. The manual
/// "generate transcript" action is unaffected by this flag.
pub fn transcribe_on_index_enabled(project: &Project) -> bool {
    project
        .config
        .extra
        .get("transcribeVideosOnIndex")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// One transcript line with its time window. Untimed sources (a plain docx)
/// still land here with synthetic sequential windows so the emitted track is
/// valid, displayable WebVTT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    pub start: Duration,
    pub end: Duration,
    pub text: String,
}

/// A resolved transcript: WebVTT text plus where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptFile {
    pub vtt: String,
    /// Project-relative path of the adjacent source (`.vtt`/`.docx`); `None`
    /// for a Ken-generated transcript, which has no user-facing source file.
    pub source_rel: Option<String>,
}

/// Untimed paragraphs get this window each, back to back, and a timed cue with
/// no explicit end borrows it as a tail — enough to keep the track valid.
const DEFAULT_CUE: Duration = Duration::from_secs(4);

/// The Whisper model Ken looks for; not shipped in the repo (multi-MB).
pub const MODEL_FILE: &str = "ggml-base.en.bin";

// ---------- WebVTT emission ----------

/// `HH:MM:SS.mmm`, the WebVTT cue-timestamp format.
pub fn format_timestamp(d: Duration) -> String {
    let ms = d.as_millis();
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1_000;
    let milli = ms % 1_000;
    format!("{h:02}:{m:02}:{s:02}.{milli:03}")
}

/// Escape the three characters WebVTT cue payloads treat as markup. `&` first
/// so the entities we introduce aren't re-escaped.
pub fn escape_cue(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Serialise cues to a WebVTT document.
pub fn emit_webvtt(cues: &[Cue]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for cue in cues {
        out.push_str(&format!(
            "{} --> {}\n",
            format_timestamp(cue.start),
            format_timestamp(cue.end)
        ));
        // A cue body may span lines; escape each and keep the breaks.
        for line in cue.text.trim().lines() {
            out.push_str(&escape_cue(line.trim()));
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// Flatten WebVTT back to plain text for the search index: drop the header,
/// NOTE blocks, numeric cue identifiers and timing lines; unescape entities.
pub fn vtt_to_plain(vtt: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut in_note = false;
    for raw in vtt.lines() {
        let line = raw.trim();
        if line.is_empty() {
            in_note = false;
            continue;
        }
        if in_note {
            continue;
        }
        if line == "WEBVTT" || line.starts_with("WEBVTT ") {
            continue;
        }
        if line.starts_with("NOTE") {
            in_note = true;
            continue;
        }
        if line.contains("-->") {
            continue; // timing (with optional cue settings)
        }
        // A bare integer on its own line is a cue identifier, not content.
        if line.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        lines.push(unescape_cue(line));
    }
    lines.join("\n")
}

fn unescape_cue(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

// ---------- docx → WebVTT ----------

/// Parse a leading timestamp off a paragraph, returning it and the remaining
/// text. Accepts `mm:ss`, `hh:mm:ss`, optional `.mmm`, optionally bracketed —
/// the shapes Teams/Zoom docx exports use.
pub fn parse_leading_timestamp(line: &str) -> Option<(Duration, String)> {
    let line = line.trim();
    let bracketed = line.starts_with('[');
    let body = if bracketed { &line[1..] } else { line };

    // The timestamp token runs until whitespace or a closing bracket.
    let end = body
        .find(|c: char| c.is_whitespace() || c == ']')
        .unwrap_or(body.len());
    let token = &body[..end];
    let dur = parse_timestamp_token(token)?;

    let mut rest = &body[end..];
    rest = rest.trim_start();
    if let Some(stripped) = rest.strip_prefix(']') {
        rest = stripped.trim_start();
    }
    // Speaker labels like "Alex Kim:" often follow the time; keep them.
    Some((dur, rest.to_string()))
}

fn parse_timestamp_token(token: &str) -> Option<Duration> {
    if token.is_empty() {
        return None;
    }
    let parts: Vec<&str> = token.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let mut secs: u64 = 0;
    let mut millis: u64 = 0;
    for (i, part) in parts.iter().enumerate() {
        let is_last = i == parts.len() - 1;
        if is_last {
            let (whole, frac) = match part.split_once('.') {
                Some((w, f)) => (w, Some(f)),
                None => (*part, None),
            };
            if whole.is_empty() || !whole.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            secs = secs * 60 + whole.parse::<u64>().ok()?;
            if let Some(frac) = frac {
                if frac.is_empty() || !frac.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                // Left-align to milliseconds ("5" → 500ms, "05" → 50ms).
                let frac: String = frac.chars().take(3).collect();
                let scale = 10u64.pow(3 - frac.len() as u32);
                millis = frac.parse::<u64>().ok()? * scale;
            }
        } else {
            if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            secs = secs * 60 + part.parse::<u64>().ok()?;
        }
    }
    Some(Duration::from_millis(secs * 1_000 + millis))
}

/// Convert extracted docx text (one paragraph per line, as `extract` yields)
/// to WebVTT. Timestamped paragraphs become timed cues; a document with no
/// timestamps at all becomes one untimed cue per paragraph.
pub fn docx_to_vtt(text: &str) -> String {
    let paragraphs: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    // Gather (timestamp, text) for paragraphs that carry a time; a paragraph
    // whose body is empty (time on its own line) absorbs following untimed
    // paragraphs until the next timestamp.
    let mut timed: Vec<(Duration, String)> = Vec::new();
    let mut any_timed = false;
    for para in &paragraphs {
        if let Some((ts, body)) = parse_leading_timestamp(para) {
            any_timed = true;
            timed.push((ts, body));
        } else if any_timed {
            if let Some(last) = timed.last_mut() {
                if last.1.is_empty() {
                    last.1 = para.to_string();
                } else {
                    last.1.push(' ');
                    last.1.push_str(para);
                }
            }
        }
    }

    let cues: Vec<Cue> = if any_timed {
        timed
            .iter()
            .enumerate()
            .filter(|(_, (_, body))| !body.is_empty())
            .map(|(i, (start, body))| {
                let end = timed
                    .get(i + 1)
                    .map(|(next, _)| *next)
                    .unwrap_or(*start + DEFAULT_CUE);
                // Guard against non-monotonic times in the source.
                let end = if end > *start { end } else { *start + DEFAULT_CUE };
                Cue { start: *start, end, text: body.clone() }
            })
            .collect()
    } else {
        paragraphs
            .iter()
            .enumerate()
            .map(|(i, para)| Cue {
                start: DEFAULT_CUE * i as u32,
                end: DEFAULT_CUE * (i as u32 + 1),
                text: para.to_string(),
            })
            .collect()
    };

    emit_webvtt(&cues)
}

// ---------- fuzzy adjacent-transcript matcher ----------

/// Does a docx filename (stem) plausibly transcribe a video (stem)? Teams and
/// Zoom name the export after the meeting — `"Sync"` → `"Sync transcript"`,
/// `"Weekly-2024"` → `"Weekly-2024.docx"` — so we normalise both (lowercase,
/// alphanumerics only) and accept containment either way. The length guard
/// keeps a one-character stem from matching everything.
pub fn fuzzy_docx_matches(video_stem: &str, docx_stem: &str) -> bool {
    let v = normalize_stem(video_stem);
    let d = normalize_stem(docx_stem);
    if v.len() < 3 || d.len() < 3 {
        return v == d && !v.is_empty();
    }
    v == d || v.contains(&d) || d.contains(&v)
}

fn normalize_stem(stem: &str) -> String {
    stem.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

// ---------- availability gating ----------

/// The user-facing reason a recording at `path` can't be transcribed, or
/// `None` when it can. ffmpeg is needed only for what Ken cannot decode
/// itself ([`decodes_in_process`]). The presence checks are parameters so
/// the gate is testable without the binaries.
pub fn transcription_blocker(
    path: &Path,
    ffmpeg_present: bool,
    model_present: bool,
) -> Option<String> {
    let ffmpeg_ok = ffmpeg_present || decodes_in_process(path);
    match (ffmpeg_ok, model_present) {
        (true, true) => None,
        (false, true) => Some(ffmpeg_help().to_string()),
        (true, false) => Some(MODEL_HELP.to_string()),
        (false, false) => Some(format!("{}\n\n{MODEL_HELP}", ffmpeg_help())),
    }
}

/// Why a recording waiting in an inbox cannot be read yet, or `None` when it
/// can: no transcription model, or ffmpeg missing for a format Ken cannot
/// decode itself.
pub fn waiting_reason(path: &Path, ffmpeg_present: bool, model_present: bool) -> Option<String> {
    if !model_present {
        return Some("Install a transcription model in Settings › AI to read this recording.".into());
    }
    if !ffmpeg_present && !decodes_in_process(path) {
        return Some(ffmpeg_help().to_string());
    }
    None
}

const MODEL_HELP: &str =
    "No transcription model is installed. Install one in Settings › AI, then try again.";

/// How to install ffmpeg here. Ken decodes WAV, MP3, M4A, FLAC, Ogg and the
/// audio of MP4 and MOV itself; ffmpeg is only for the rest.
pub fn ffmpeg_help() -> &'static str {
    if cfg!(windows) {
        "Ken needs ffmpeg to read the audio of this file (WebM, MKV, AVI or Opus). Install it with `winget install Gyan.FFmpeg`, then try again."
    } else if cfg!(target_os = "macos") {
        "Ken needs ffmpeg to read the audio of this file (WebM, MKV, AVI or Opus). Install it with `brew install ffmpeg`, then try again."
    } else {
        "Ken needs ffmpeg to read the audio of this file (WebM, MKV, AVI or Opus). Install it from your package manager, then try again."
    }
}

/// Whether Ken reads this file's audio itself (symphonia), without ffmpeg.
pub fn decodes_in_process(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "wav" | "mp3" | "m4a" | "aac" | "flac" | "ogg" | "oga" | "mp4" | "mov" | "m4v")
}

/// Where Ken keeps its Whisper model, under the app data dir.
pub fn model_path(base_dir: &Path) -> PathBuf {
    base_dir.join("whisper").join(MODEL_FILE)
}

/// Detect a system `ffmpeg`, mirroring how the CLI runner finds `claude`:
/// PATH first, then the install locations a GUI app's slim PATH misses.
pub fn discover_ffmpeg() -> Option<PathBuf> {
    let names = ffmpeg_names();
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in &names {
                let candidate = dir.join(name);
                if crate::runner::is_executable(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    ffmpeg_fallbacks().into_iter().find(|c| crate::runner::is_executable(c))
}

/// The file names `ffmpeg` goes by on PATH: on Windows each executable
/// extension PATHEXT lists (`.exe`, `.com`; a `.cmd` or `.bat` shim would
/// need a shell, so those are left out), elsewhere the bare name.
fn ffmpeg_names() -> Vec<String> {
    if !cfg!(windows) {
        return vec!["ffmpeg".into()];
    }
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE".into());
    let mut exts: Vec<String> = pathext
        .split(';')
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| e == ".exe" || e == ".com")
        .collect();
    if !exts.contains(&".exe".to_string()) {
        exts.push(".exe".into());
    }
    exts.sort_by_key(|e| e != ".exe");
    exts.into_iter().map(|e| format!("ffmpeg{e}")).collect()
}

/// Where package managers put ffmpeg when PATH does not say: winget's links
/// and packages, Chocolatey and Scoop on Windows; Homebrew and the system on
/// macOS and Linux.
fn ffmpeg_fallbacks() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            let winget = local.join("Microsoft").join("WinGet");
            out.push(winget.join("Links").join("ffmpeg.exe"));
            // `winget install Gyan.FFmpeg` without its link: the package's
            // own `<build>/bin/ffmpeg.exe`.
            for pkg in std::fs::read_dir(winget.join("Packages")).into_iter().flatten().flatten() {
                if !pkg.file_name().to_string_lossy().starts_with("Gyan.FFmpeg") {
                    continue;
                }
                for build in std::fs::read_dir(pkg.path()).into_iter().flatten().flatten() {
                    out.push(build.path().join("bin").join("ffmpeg.exe"));
                }
            }
        }
        let program_data = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        out.push(program_data.join("chocolatey").join("bin").join("ffmpeg.exe"));
        if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
            out.push(home.join("scoop").join("shims").join("ffmpeg.exe"));
        }
    } else {
        out.push(PathBuf::from("/opt/homebrew/bin/ffmpeg"));
        out.push(PathBuf::from("/usr/local/bin/ffmpeg"));
        out.push(PathBuf::from("/usr/bin/ffmpeg"));
    }
    out
}

// ---------- filesystem resolution ----------

/// The `.ken/transcripts` cache under a project root, where generated `.vtt`
/// files live. Kept inside `.ken` (which the scanner skips) so generated
/// transcripts never pollute the user's folder or sync to teammates; the
/// video row carries their searchable text instead.
pub fn cache_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ken").join("transcripts")
}

/// A stable cache filename for one video's generated transcript, derived from
/// its project-relative path (so a moved video re-transcribes, deliberately).
pub fn cache_name(rel_path: &str) -> String {
    // FNV-1a: no crypto needed, just a stable spread over paths.
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in rel_path.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}.vtt")
}

/// Walk up from a file to the project root (the folder holding `.ken`), so
/// indexing can find the generated-transcript cache without the project
/// being plumbed through the extractor.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.parent();
    while let Some(d) = dir {
        if d.join(".ken").is_dir() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Resolve a transcript for a video by the contract's order: adjacent `.vtt`,
/// then a fuzzy-matched adjacent `.docx`, then a previously generated file in
/// the cache. `None` means nothing is available yet.
pub fn resolve_transcript(video_abs: &Path, project_root: &Path) -> Option<TranscriptFile> {
    let parent = video_abs.parent()?;
    let stem = video_abs.file_stem()?.to_string_lossy().to_string();

    // (a) Adjacent <stem>.vtt.
    let adjacent_vtt = parent.join(format!("{stem}.vtt"));
    if let Ok(vtt) = std::fs::read_to_string(&adjacent_vtt) {
        return Some(TranscriptFile {
            vtt,
            source_rel: rel_of(&adjacent_vtt, project_root),
        });
    }

    // (b) Adjacent .docx whose name fuzzily matches the video's.
    if let Some(docx) = find_matching_docx(parent, &stem) {
        if let Ok(extracted) = crate::extract::extract(&docx) {
            return Some(TranscriptFile {
                vtt: docx_to_vtt(&extracted.text),
                source_rel: rel_of(&docx, project_root),
            });
        }
    }

    // (c) A previously generated transcript in the cache.
    if let Ok(rel) = video_abs.strip_prefix(project_root) {
        let rel = rel.to_string_lossy().replace('\\', "/");
        let cached = cache_dir(project_root).join(cache_name(&rel));
        if let Ok(vtt) = std::fs::read_to_string(&cached) {
            return Some(TranscriptFile { vtt, source_rel: None });
        }
    }
    None
}

/// The transcript text to index for a video, or empty when none exists. Finds
/// the project root itself so it composes with the generic extractor.
pub fn indexable_text(video_abs: &Path) -> String {
    let Some(root) = find_project_root(video_abs) else {
        // No project context — fall back to adjacent files only.
        return resolve_adjacent_only(video_abs)
            .map(|t| vtt_to_plain(&t.vtt))
            .unwrap_or_default();
    };
    resolve_transcript(video_abs, &root)
        .map(|t| vtt_to_plain(&t.vtt))
        .unwrap_or_default()
}

fn resolve_adjacent_only(video_abs: &Path) -> Option<TranscriptFile> {
    let parent = video_abs.parent()?;
    let stem = video_abs.file_stem()?.to_string_lossy().to_string();
    let adjacent_vtt = parent.join(format!("{stem}.vtt"));
    if let Ok(vtt) = std::fs::read_to_string(&adjacent_vtt) {
        return Some(TranscriptFile { vtt, source_rel: None });
    }
    let docx = find_matching_docx(parent, &stem)?;
    let extracted = crate::extract::extract(&docx).ok()?;
    Some(TranscriptFile { vtt: docx_to_vtt(&extracted.text), source_rel: None })
}

fn find_matching_docx(dir: &Path, video_stem: &str) -> Option<PathBuf> {
    let mut best: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let is_docx = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("docx"));
        if !is_docx {
            continue;
        }
        let Some(docx_stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if docx_stem.starts_with("~$") {
            continue; // Office lock file
        }
        if fuzzy_docx_matches(video_stem, docx_stem) {
            // Prefer an exact stem match over a looser containment one.
            if docx_stem.eq_ignore_ascii_case(video_stem) {
                return Some(path);
            }
            best.get_or_insert(path);
        }
    }
    best
}

fn rel_of(path: &Path, project_root: &Path) -> Option<String> {
    path.strip_prefix(project_root)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
}

// ---------- ffmpeg + Whisper (thin, untested shells) ----------

/// Decode a recording's audio to 16 kHz mono `f32` PCM, what Whisper
/// expects: in-process first (symphonia: WAV, MP3, AAC/M4A, the audio of
/// MP4/MOV, FLAC, Ogg Vorbis), then ffmpeg when it is installed and the file
/// is one symphonia cannot read.
pub fn decode_audio(path: &Path, ffmpeg: Option<&Path>) -> Result<Vec<f32>> {
    match decode_in_process(path) {
        Ok(samples) => Ok(samples),
        Err(own) => match ffmpeg {
            Some(ffmpeg) => extract_audio_f32(ffmpeg, path),
            None if decodes_in_process(path) => Err(own),
            None => Err(Error::Other(format!("Ken could not read this file's audio itself ({own}). {}", ffmpeg_help()))),
        },
    }
}

/// [`decode_audio`] through symphonia only: the first track it can decode,
/// downmixed and resampled to 16 kHz.
pub fn decode_in_process(path: &Path) -> Result<Vec<f32>> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error as DecodeError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let fail = |e: DecodeError| Error::Other(format!("could not read the audio: {e}"));
    let file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(fail)?;
    let mut format = probed.format;
    // The default track when it decodes, else the first one that does (a
    // video's picture track does not).
    let codecs = symphonia::default::get_codecs();
    let mut candidates: Vec<&symphonia::core::formats::Track> = format.default_track().into_iter().collect();
    candidates.extend(format.tracks().iter());
    let (track_id, mut decoder) = candidates
        .into_iter()
        .filter(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .find_map(|t| codecs.make(&t.codec_params, &DecoderOptions::default()).ok().map(|d| (t.id, d)))
        .ok_or_else(|| Error::Other("this file has no audio Ken can read".into()))?;

    let mut out = Vec::new();
    let mut resampler: Option<(u32, crate::record::LinearResampler)> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(DecodeError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(DecodeError::ResetRequired) => break,
            Err(e) => return Err(fail(e)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // One bad frame is skipped, as players do.
            Err(DecodeError::DecodeError(_)) => continue,
            Err(e) => return Err(fail(e)),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1) as u16;
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        let mono = crate::record::downmix_to_mono(buf.samples(), channels);
        if resampler.as_ref().is_none_or(|(rate, _)| *rate != spec.rate) {
            if let Some((_, mut old)) = resampler.take() {
                out.extend(old.finish());
            }
            resampler = Some((spec.rate, crate::record::LinearResampler::new(spec.rate, crate::record::TARGET_RATE)));
        }
        if let Some((_, r)) = resampler.as_mut() {
            out.extend(r.process(&mono));
        }
    }
    if let Some((_, mut r)) = resampler {
        out.extend(r.finish());
    }
    Ok(out)
}

/// Decode a video's audio to 16 kHz mono `f32` PCM via ffmpeg — exactly what
/// Whisper expects. Streams raw `f32le` on stdout so no WAV parser is needed.
pub fn extract_audio_f32(ffmpeg: &Path, video: &Path) -> Result<Vec<f32>> {
    let mut cmd = std::process::Command::new(ffmpeg);
    let output = crate::proc::quiet(&mut cmd)
        .args(["-nostdin", "-i"])
        .arg(video)
        .args(["-ar", "16000", "-ac", "1", "-f", "f32le", "-"])
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| Error::Other(format!("couldn't run ffmpeg: {e}")))?;
    if !output.status.success() {
        return Err(Error::Other(
            "ffmpeg couldn't read audio from this video".into(),
        ));
    }
    Ok(output
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// Progress from the transcription pipeline: a quick unmeterable ffmpeg
/// extraction, then Whisper's native 0–100 percent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptPhase {
    Extracting,
    Transcribing(u8),
}

/// Shared, thread-safe progress sink. `Arc` because whisper-rs demands a
/// `'static` callback, and the same closure is observed from the caller side.
pub type ProgressFn = std::sync::Arc<dyn Fn(TranscriptPhase) + Send + Sync>;

/// The do-nothing sink for callers that don't surface progress.
pub fn noop_progress() -> ProgressFn {
    std::sync::Arc::new(|_| {})
}

/// Map one channel's 0–100 into an overall percent across `count` sequential
/// channels (a two-channel recording transcribes Me then Them; the bar must
/// not jump back to 0 in between).
pub fn scale_channel_pct(idx: usize, count: usize, pct: u8) -> u8 {
    if count == 0 {
        return 0;
    }
    (((idx.min(count - 1) * 100) + pct.min(100) as usize) / count).min(100) as u8
}

/// The language Whisper is told: "en" for an English-only model file
/// (`ggml-base.en.bin`, `ggml-small.en-q5_1.bin`), "auto" otherwise.
pub fn whisper_language(model: &Path) -> &'static str {
    let name = model.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if name.contains(".en.") || name.contains(".en-") {
        "en"
    } else {
        "auto"
    }
}

/// Transcribe 16 kHz mono samples to timed cues with Whisper. Compiled only
/// with the `whisper` feature; the fallback returns an actionable error so the
/// rest of Ken builds without the native toolchain.
#[cfg(feature = "whisper")]
pub fn transcribe_with_progress(
    model: &Path,
    samples: &[f32],
    on_progress: ProgressFn,
) -> Result<Vec<Cue>> {
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    // whisper-rs 0.16 accepts any `AsRef<Path>`, so the model path passes through
    // directly (no UTF-8 round-trip needed).
    // The graphics card when it is on, with flash attention; a model the
    // card cannot take is loaded again on the CPU (see `compute`).
    let load = |gpu: bool| {
        let mut cp = WhisperContextParameters::default();
        cp.use_gpu(gpu).flash_attn(gpu);
        WhisperContext::new_with_params(model, cp)
    };
    let gpu = crate::compute::use_gpu();
    let ctx = match load(gpu) {
        Ok(c) => Ok(c),
        Err(e) if gpu => {
            eprintln!("Whisper on the graphics card failed ({e}); loading it on the CPU");
            load(false)
        }
        Err(e) => Err(e),
    }
    .map_err(|e| Error::Other(format!("couldn't load the Whisper model: {e}")))?;
    let mut state = ctx
        .create_state()
        .map_err(|e| Error::Other(format!("Whisper init failed: {e}")))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(crate::compute::threads());
    // An English-only model (`*.en*`) is told the language; a multilingual
    // one detects it, so a meeting in another language is not forced into
    // English.
    params.set_language(Some(whisper_language(model)));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    {
        let cb = on_progress.clone();
        params.set_progress_callback_safe(move |pct: i32| {
            cb(TranscriptPhase::Transcribing(pct.clamp(0, 100) as u8));
        });
    }

    state
        .full(params, samples)
        .map_err(|e| Error::Other(format!("transcription failed: {e}")))?;

    // whisper-rs 0.16 exposes segments through an iterator of `WhisperSegment`
    // (the old `full_get_segment_*` index getters are gone). Times are still
    // reported in centiseconds; `to_str_lossy` tolerates the odd invalid byte.
    let mut cues = Vec::new();
    for seg in state.as_iter() {
        let text = seg
            .to_str_lossy()
            .map_err(|e| Error::Other(e.to_string()))?
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        let t0 = seg.start_timestamp();
        let t1 = seg.end_timestamp();
        cues.push(Cue {
            start: Duration::from_millis((t0.max(0) as u64) * 10),
            end: Duration::from_millis((t1.max(0) as u64) * 10),
            text,
        });
    }
    Ok(cues)
}

#[cfg(not(feature = "whisper"))]
pub fn transcribe_with_progress(
    _model: &Path,
    _samples: &[f32],
    _on_progress: ProgressFn,
) -> Result<Vec<Cue>> {
    Err(Error::Other(
        "This build of Ken was compiled without on-device transcription support.".into(),
    ))
}

/// [`transcribe_with_progress`] without a progress sink.
pub fn transcribe(model: &Path, samples: &[f32]) -> Result<Vec<Cue>> {
    transcribe_with_progress(model, samples, noop_progress())
}

/// Full pipeline: recording → 16 kHz audio ([`decode_audio`]) → Whisper →
/// WebVTT. Blocking; call from a worker thread.
pub fn generate_vtt(ffmpeg: Option<&Path>, model: &Path, video: &Path) -> Result<String> {
    generate_vtt_with_progress(ffmpeg, model, video, noop_progress())
}

/// [`generate_vtt`] reporting phase progress: `Extracting` while the audio is
/// decoded, then Whisper's percent stream.
pub fn generate_vtt_with_progress(
    ffmpeg: Option<&Path>,
    model: &Path,
    video: &Path,
    on_progress: ProgressFn,
) -> Result<String> {
    on_progress(TranscriptPhase::Extracting);
    let samples = decode_audio(video, ffmpeg)?;
    if samples.is_empty() {
        return Err(Error::Other("this recording has no audio to transcribe".into()));
    }
    let cues = transcribe_with_progress(model, &samples, on_progress)?;
    if cues.iter().all(|c| c.text.trim().is_empty()) {
        return Err(Error::Other("nothing was said in this recording that Whisper could hear".into()));
    }
    Ok(emit_webvtt(&cues))
}

/// Generate a transcript and store it in the project's cache, returning the
/// path written. The caller re-indexes the video so its transcript becomes
/// searchable and the `index-updated` event fires.
pub fn generate_and_cache(
    ffmpeg: Option<&Path>,
    model: &Path,
    project_root: &Path,
    video_rel: &str,
) -> Result<PathBuf> {
    generate_and_cache_with_progress(ffmpeg, model, project_root, video_rel, noop_progress())
}

/// [`generate_and_cache`] with a progress sink (see [`TranscriptPhase`]).
pub fn generate_and_cache_with_progress(
    ffmpeg: Option<&Path>,
    model: &Path,
    project_root: &Path,
    video_rel: &str,
    on_progress: ProgressFn,
) -> Result<PathBuf> {
    let video_abs = project_root.join(video_rel);
    let vtt = generate_vtt_with_progress(ffmpeg, model, &video_abs, on_progress)?;
    let dir = cache_dir(project_root);
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let out = dir.join(cache_name(video_rel));
    std::fs::write(&out, vtt).map_err(|e| Error::io(&out, e))?;
    Ok(out)
}

/// Whether a recording at `rel` (inside `project_root`) has a transcript
/// already: adjacent, or generated into the cache.
pub fn has_transcript(project_root: &Path, rel: &str) -> bool {
    resolve_transcript(&project_root.join(rel), project_root).is_some()
}

/// Carry a generated transcript along when its recording moves inside the
/// project (the cache is keyed by path). A no-op when there is none.
pub fn move_cached(project_root: &Path, from_rel: &str, to_rel: &str) {
    let dir = cache_dir(project_root);
    let from = dir.join(cache_name(from_rel));
    if from.is_file() {
        let _ = std::fs::copy(&from, dir.join(cache_name(to_rel)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_models_are_told_english_and_the_others_detect() {
        assert_eq!(whisper_language(Path::new("ggml-base.en.bin")), "en");
        assert_eq!(whisper_language(Path::new("ggml-small.en-q5_1.bin")), "en");
        assert_eq!(whisper_language(Path::new("ggml-large-v3-turbo-q5_0.bin")), "auto");
    }

    #[test]
    fn timestamp_formatting() {
        assert_eq!(format_timestamp(Duration::from_millis(0)), "00:00:00.000");
        assert_eq!(format_timestamp(Duration::from_millis(5250)), "00:00:05.250");
        assert_eq!(
            format_timestamp(Duration::from_millis(3_661_007)),
            "01:01:01.007"
        );
    }

    #[test]
    fn cue_text_is_escaped() {
        assert_eq!(escape_cue("a & b < c > d"), "a &amp; b &lt; c &gt; d");
        // Ampersand escaped first so we don't double-escape.
        assert_eq!(escape_cue("<tag>"), "&lt;tag&gt;");
    }

    #[test]
    fn webvtt_emission_shape() {
        let cues = vec![
            Cue {
                start: Duration::from_millis(0),
                end: Duration::from_millis(2000),
                text: "Hello <world>".into(),
            },
            Cue {
                start: Duration::from_millis(2000),
                end: Duration::from_millis(4500),
                text: "second line".into(),
            },
        ];
        let vtt = emit_webvtt(&cues);
        assert!(vtt.starts_with("WEBVTT\n\n"));
        assert!(vtt.contains("00:00:00.000 --> 00:00:02.000\nHello &lt;world&gt;\n"));
        assert!(vtt.contains("00:00:02.000 --> 00:00:04.500\nsecond line\n"));
    }

    #[test]
    fn vtt_round_trips_to_plain_text() {
        let vtt = "WEBVTT\n\nNOTE this is a comment\n\n1\n00:00:00.000 --> 00:00:02.000\nHello &amp; welcome\n\n00:00:02.000 --> 00:00:04.000\nsecond &lt;line&gt;\n";
        let plain = vtt_to_plain(vtt);
        assert_eq!(plain, "Hello & welcome\nsecond <line>");
        assert!(!plain.contains("WEBVTT"));
        assert!(!plain.contains("-->"));
        assert!(!plain.contains("NOTE"));
    }

    #[test]
    fn parse_timestamps_in_several_shapes() {
        assert_eq!(
            parse_leading_timestamp("00:00:05.000 Hello there"),
            Some((Duration::from_millis(5000), "Hello there".into()))
        );
        assert_eq!(
            parse_leading_timestamp("[0:05] quick"),
            Some((Duration::from_millis(5000), "quick".into()))
        );
        assert_eq!(
            parse_leading_timestamp("1:02:03 later"),
            Some((Duration::from_millis(3_723_000), "later".into()))
        );
        // mm:ss with a speaker label kept in the body.
        assert_eq!(
            parse_leading_timestamp("12:30 Alex Kim: hi"),
            Some((Duration::from_millis(750_000), "Alex Kim: hi".into()))
        );
        // Not timestamps.
        assert_eq!(parse_leading_timestamp("Just some prose."), None);
        assert_eq!(parse_leading_timestamp("12345 no colon"), None);
    }

    #[test]
    fn docx_with_timestamps_becomes_timed_cues() {
        let text = "Meeting transcript\n0:00:00 Welcome everyone\n0:00:05 Let's begin the review\n0:00:12 Any questions?";
        let vtt = docx_to_vtt(text);
        assert!(vtt.starts_with("WEBVTT"));
        // First cue spans to the next timestamp.
        assert!(vtt.contains("00:00:00.000 --> 00:00:05.000\nWelcome everyone\n"));
        assert!(vtt.contains("00:00:05.000 --> 00:00:12.000\nLet's begin the review\n"));
        // The intro line, having no timestamp before any timed line, is dropped.
        assert!(!vtt.contains("Meeting transcript"));
        // Last cue gets a synthetic tail.
        assert!(vtt.contains("00:00:12.000 --> 00:00:16.000\nAny questions?\n"));
    }

    #[test]
    fn docx_timestamp_on_its_own_line_absorbs_following_text() {
        let text = "00:00:03\nAlex: the budget is approved\n00:00:09\nPriya: thanks";
        let vtt = docx_to_vtt(text);
        assert!(vtt.contains("00:00:03.000 --> 00:00:09.000\nAlex: the budget is approved\n"));
        assert!(vtt.contains("00:00:09.000 --> 00:00:13.000\nPriya: thanks\n"));
    }

    #[test]
    fn docx_without_timestamps_becomes_untimed_paragraph_cues() {
        let text = "First paragraph of notes.\nSecond paragraph.\n\nThird.";
        let vtt = docx_to_vtt(text);
        // One valid, displayable cue per paragraph, back to back.
        assert!(vtt.contains("00:00:00.000 --> 00:00:04.000\nFirst paragraph of notes.\n"));
        assert!(vtt.contains("00:00:04.000 --> 00:00:08.000\nSecond paragraph.\n"));
        assert!(vtt.contains("00:00:08.000 --> 00:00:12.000\nThird.\n"));
    }

    #[test]
    fn fuzzy_matcher_handles_teams_zoom_naming() {
        // Exact and suffixed exports match.
        assert!(fuzzy_docx_matches("Weekly Sync", "Weekly Sync"));
        assert!(fuzzy_docx_matches("Weekly Sync", "Weekly Sync transcript"));
        assert!(fuzzy_docx_matches("Meeting-2024-01-05", "Meeting 2024 01 05"));
        // Video name contained in a longer docx name.
        assert!(fuzzy_docx_matches("Q3 Review", "Q3 Review - Notes and Transcript"));
        // Unrelated names don't match.
        assert!(!fuzzy_docx_matches("Weekly Sync", "Budget Spreadsheet Notes"));
        // A too-short stem never matches loosely.
        assert!(!fuzzy_docx_matches("a", "a big long document"));
    }

    #[test]
    fn availability_gate_reports_each_missing_prerequisite() {
        let webm = Path::new("calls/sync.webm");
        let mp4 = Path::new("calls/sync.mp4");
        assert_eq!(transcription_blocker(webm, true, true), None);
        // Ken decodes an mp4's audio itself: no ffmpeg needed.
        assert_eq!(transcription_blocker(mp4, false, true), None);

        let ffmpeg_only = transcription_blocker(webm, false, true).unwrap();
        assert!(ffmpeg_only.contains("ffmpeg"));
        assert!(!ffmpeg_only.contains("transcription model"));

        let model_only = transcription_blocker(mp4, false, false).unwrap();
        assert!(model_only.contains("transcription model"));
        assert!(!model_only.contains("ffmpeg"));

        let both = transcription_blocker(webm, false, false).unwrap();
        assert!(both.contains("ffmpeg"));
        assert!(both.contains("transcription model"));
    }

    #[test]
    fn a_recording_waits_for_the_model_and_for_ffmpeg_only_when_needed() {
        let m4a = Path::new("Raw/standup.m4a");
        let mkv = Path::new("Raw/standup.mkv");
        assert!(waiting_reason(m4a, false, false).unwrap().contains("Install a transcription model in Settings"));
        assert_eq!(waiting_reason(m4a, false, true), None);
        assert!(waiting_reason(mkv, false, true).unwrap().contains("ffmpeg"));
        assert_eq!(waiting_reason(mkv, true, true), None);
        let help = ffmpeg_help();
        if cfg!(windows) {
            assert!(help.contains("winget install Gyan.FFmpeg"));
        } else if cfg!(target_os = "macos") {
            assert!(help.contains("brew install ffmpeg"));
        }
    }

    #[test]
    fn ffmpeg_is_looked_for_under_its_windows_name() {
        let names = ffmpeg_names();
        if cfg!(windows) {
            assert_eq!(names.first().map(String::as_str), Some("ffmpeg.exe"));
            assert!(names.iter().all(|n| !n.ends_with(".cmd") && !n.ends_with(".bat")));
            assert!(ffmpeg_fallbacks().iter().any(|p| p.ends_with("WinGet/Links/ffmpeg.exe") || p.ends_with(r"WinGet\Links\ffmpeg.exe")));
        } else {
            assert_eq!(names, vec!["ffmpeg".to_string()]);
        }
    }

    #[test]
    fn a_wav_is_decoded_in_process_to_16k_mono() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        // One second of a 440 Hz tone, 44.1 kHz stereo, 16-bit.
        let spec = hound::WavSpec { channels: 2, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..44_100 {
            let s = ((i as f32 * 440.0 * std::f32::consts::TAU / 44_100.0).sin() * 0.5 * 32767.0) as i16;
            w.write_sample(s).unwrap();
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
        let samples = decode_audio(&path, None).unwrap();
        assert!((samples.len() as i64 - 16_000).abs() < 50, "len {}", samples.len());
        let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((0.4..0.6).contains(&peak), "peak {peak}");
    }

    #[test]
    fn a_file_with_no_audio_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-audio.mp3");
        std::fs::write(&path, b"this is not an mp3 at all").unwrap();
        assert!(decode_audio(&path, None).is_err());
    }

    #[test]
    fn a_generated_transcript_follows_its_recording() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(cache_dir(root)).unwrap();
        std::fs::write(cache_dir(root).join(cache_name("Raw/a.m4a")), "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nhello\n").unwrap();
        assert!(!has_transcript(root, "Filed/a.m4a"));
        move_cached(root, "Raw/a.m4a", "Filed/a.m4a");
        assert!(has_transcript(root, "Filed/a.m4a"));
    }

    #[test]
    fn transcribe_on_index_defaults_off_and_honors_flag() {
        use crate::project::{Project, ProjectConfig};
        let mut config = ProjectConfig {
            name: "t".into(),
            id: uuid::Uuid::new_v4(),
            excluded: Vec::new(),
            features: serde_json::Map::new(),
            extra: serde_json::Map::new(),
        };
        let project = Project { root: PathBuf::from("/tmp/x"), config: config.clone() };
        // Absent → off.
        assert!(!transcribe_on_index_enabled(&project));

        config
            .extra
            .insert("transcribeVideosOnIndex".into(), serde_json::Value::Bool(true));
        let on = Project { root: PathBuf::from("/tmp/x"), config: config.clone() };
        assert!(transcribe_on_index_enabled(&on));

        config
            .extra
            .insert("transcribeVideosOnIndex".into(), serde_json::Value::Bool(false));
        let off = Project { root: PathBuf::from("/tmp/x"), config };
        assert!(!transcribe_on_index_enabled(&off));
    }

    #[test]
    fn cache_name_is_stable_and_path_specific() {
        assert_eq!(cache_name("videos/demo.mp4"), cache_name("videos/demo.mp4"));
        assert_ne!(cache_name("videos/demo.mp4"), cache_name("videos/other.mp4"));
        assert!(cache_name("videos/demo.mp4").ends_with(".vtt"));
    }

    #[test]
    fn resolves_adjacent_vtt_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ken")).unwrap();
        std::fs::create_dir_all(root.join("videos")).unwrap();
        std::fs::write(root.join("videos/demo.mp4"), b"fake").unwrap();
        std::fs::write(
            root.join("videos/demo.vtt"),
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nhi there\n",
        )
        .unwrap();

        let resolved = resolve_transcript(&root.join("videos/demo.mp4"), root).unwrap();
        assert!(resolved.vtt.contains("hi there"));
        assert_eq!(resolved.source_rel.as_deref(), Some("videos/demo.vtt"));
        assert_eq!(indexable_text(&root.join("videos/demo.mp4")), "hi there");
    }

    #[test]
    fn resolves_fuzzy_docx_when_no_vtt() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ken")).unwrap();
        std::fs::write(root.join("Weekly Sync.mp4"), b"fake").unwrap();
        // Build a minimal .docx (zip with word/document.xml) so extract works.
        write_docx(
            &root.join("Weekly Sync transcript.docx"),
            "0:00:00 Kickoff\n0:00:04 Budget approved",
        );

        let resolved = resolve_transcript(&root.join("Weekly Sync.mp4"), root).unwrap();
        assert!(resolved.vtt.contains("Kickoff"));
        assert!(resolved.vtt.contains("Budget approved"));
        assert_eq!(
            resolved.source_rel.as_deref(),
            Some("Weekly Sync transcript.docx")
        );
    }

    #[test]
    fn resolves_generated_cache_last() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("videos")).unwrap();
        std::fs::write(root.join("videos/demo.mp4"), b"fake").unwrap();
        let cache = cache_dir(root);
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join(cache_name("videos/demo.mp4")),
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\ngenerated words\n",
        )
        .unwrap();

        let resolved = resolve_transcript(&root.join("videos/demo.mp4"), root).unwrap();
        assert!(resolved.vtt.contains("generated words"));
        // Generated transcripts have no user-facing source file.
        assert_eq!(resolved.source_rel, None);
    }

    #[test]
    fn no_transcript_resolves_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ken")).unwrap();
        std::fs::write(root.join("lonely.mp4"), b"fake").unwrap();
        assert!(resolve_transcript(&root.join("lonely.mp4"), root).is_none());
        assert_eq!(indexable_text(&root.join("lonely.mp4")), "");
    }

    /// Minimal .docx writer for tests: a zip whose `word/document.xml` holds one
    /// `<w:p><w:t>` paragraph per source line, matching what `extract` reads.
    fn write_docx(path: &Path, text: &str) {
        use std::io::Write as _;
        use zip::write::SimpleFileOptions;
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("word/document.xml", SimpleFileOptions::default())
            .unwrap();
        let mut xml =
            String::from("<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body>");
        for line in text.lines() {
            xml.push_str(&format!("<w:p><w:t>{line}</w:t></w:p>"));
        }
        xml.push_str("</w:body></w:document>");
        zip.write_all(xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn channel_percentages_scale_into_one_overall_bar() {
        // Two channels: channel 0 covers 0–50, channel 1 covers 50–100.
        assert_eq!(scale_channel_pct(0, 2, 0), 0);
        assert_eq!(scale_channel_pct(0, 2, 100), 50);
        assert_eq!(scale_channel_pct(1, 2, 0), 50);
        assert_eq!(scale_channel_pct(1, 2, 100), 100);
        // One channel passes through; degenerate inputs stay in range.
        assert_eq!(scale_channel_pct(0, 1, 37), 37);
        assert_eq!(scale_channel_pct(0, 0, 50), 0);
        assert_eq!(scale_channel_pct(3, 2, 100), 100);
    }
}
