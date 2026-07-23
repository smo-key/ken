//! Cloud-storage placeholders.
//!
//! OneDrive, iCloud Drive and Dropbox keep files "online-only": the name,
//! size and mtime are on disk but the bytes are not. macOS marks those with
//! the `SF_DATALESS` flag; Windows uses the offline/recall attributes. Any
//! read of such a file blocks while the provider downloads it — seconds to
//! minutes, and frequently `ETIMEDOUT` (os error 60).
//!
//! So Ken never reads a placeholder implicitly. Indexing records it by name
//! (`scan::STATUS_CLOUD_ONLY`) and the bytes are fetched only when the user
//! opens the file, via [`hydrate`].

use std::fs::Metadata;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long we're willing to wait for a provider to materialize one file.
/// Multi-hundred-megabyte decks on a slow link genuinely take minutes.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(300);

/// Gap between polls. Long enough not to hammer the File Provider, short
/// enough that a file landing early is noticed almost immediately.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// macOS `SF_DATALESS` (sys/stat.h): the file's data lives in the cloud.
#[cfg(target_os = "macos")]
const SF_DATALESS: u32 = 0x4000_0000;

/// Windows `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` and `FILE_ATTRIBUTE_OFFLINE`.
#[cfg(windows)]
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
#[cfg(windows)]
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;

/// Is this file a cloud placeholder whose bytes are not on disk yet?
///
/// Cheap: reads only the metadata we already have. Never touches the file.
#[cfg(target_os = "macos")]
pub fn is_dataless(meta: &Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    meta.st_flags() & SF_DATALESS != 0
}

#[cfg(windows)]
pub fn is_dataless(meta: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    let attrs = meta.file_attributes();
    attrs & (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | FILE_ATTRIBUTE_OFFLINE) != 0
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn is_dataless(_meta: &Metadata) -> bool {
    false
}

/// [`is_dataless`] for a path. False when the file can't be stat'd — callers
/// treat "unknown" as local and let the real error surface on read.
pub fn is_placeholder(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| is_dataless(&m)).unwrap_or(false)
}

/// The result of one attempt to touch the file's bytes.
#[derive(Debug)]
enum Attempt {
    /// The bytes are on disk: the placeholder flag is gone and a read worked.
    Ready,
    /// The provider is still fetching. Worth another look shortly.
    Downloading,
    /// Nothing to wait for — this file will never arrive.
    Fatal(std::io::Error),
}

/// A read that times out is *not* a failed download. macOS's File Provider
/// gives up on the blocking read after ~60s (`ETIMEDOUT`) while OneDrive keeps
/// pulling the file in the background, so that errno means "come back later".
/// Anything else — gone, forbidden, corrupt — will not fix itself.
fn classify(e: std::io::Error) -> Attempt {
    if e.kind() == std::io::ErrorKind::TimedOut || e.raw_os_error() == Some(60) {
        Attempt::Downloading
    } else {
        Attempt::Fatal(e)
    }
}

/// One poke at the file: opening and reading a byte is what asks the provider
/// to start (or continue) the download. The dataless flag is the authority on
/// whether it finished — a read can return early on a partially materialized
/// file — so both must agree before we call it done.
fn probe(path: &Path) -> Attempt {
    use std::io::Read;

    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return classify(e),
    };
    let mut byte = [0u8; 1];
    if let Err(e) = f.read(&mut byte) {
        return classify(e);
    }
    if is_placeholder(path) {
        return Attempt::Downloading;
    }
    Attempt::Ready
}

/// The retry loop, with the clock and the file both injected so the matrix of
/// outcomes (immediate, late, never, hopeless) is unit-testable — there is no
/// way to fabricate a real `SF_DATALESS` file in a test.
fn poll_until_hydrated(
    path: &Path,
    deadline: Duration,
    mut attempt: impl FnMut() -> Attempt,
    mut elapsed: impl FnMut() -> Duration,
    mut sleep: impl FnMut(Duration),
) -> crate::Result<()> {
    loop {
        match attempt() {
            Attempt::Ready => return Ok(()),
            Attempt::Fatal(e) => return Err(crate::Error::io(path, e)),
            Attempt::Downloading => {
                if elapsed() >= deadline {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    return Err(crate::Error::Other(format!(
                        "\"{name}\" is still downloading from the cloud. Large files can take \
                         several minutes — the download keeps running in the background, so \
                         try again in a moment."
                    )));
                }
                sleep(POLL_INTERVAL);
            }
        }
    }
}

/// Pull a placeholder's bytes down from the cloud provider, blocking until they
/// arrive or [`DEFAULT_DEADLINE`] passes.
///
/// Blocks for as long as the download takes, so callers must run this off any
/// thread that holds a lock or serves the UI.
pub fn hydrate(path: &Path) -> crate::Result<()> {
    hydrate_with_deadline(path, DEFAULT_DEADLINE)
}

/// [`hydrate`] with an explicit budget. Giving up here abandons only the wait,
/// not the download: the provider carries on, so a later call usually returns
/// straight away.
pub fn hydrate_with_deadline(path: &Path, deadline: Duration) -> crate::Result<()> {
    hydrate_with_progress(path, deadline, |_, _| {})
}

/// [`hydrate_with_deadline`] that also reports download progress. The provider
/// owns the transfer, so bytes can't be counted as they're read — instead each
/// poll tick samples how much of the file is *allocated* on disk against its
/// logical size (macOS reports a dataless file's full `len()` up front). The
/// terminal `(total, total)` sample fires once the bytes have fully landed.
pub fn hydrate_with_progress(
    path: &Path,
    deadline: Duration,
    mut on_progress: impl FnMut(u64, u64),
) -> crate::Result<()> {
    let started = Instant::now();
    poll_until_hydrated(
        path,
        deadline,
        || {
            let attempt = probe(path);
            match &attempt {
                Attempt::Downloading => {
                    if let Some((got, total)) = hydration_sample(path) {
                        on_progress(got, total);
                    }
                }
                Attempt::Ready => {
                    if let Ok(m) = std::fs::metadata(path) {
                        on_progress(m.len(), m.len());
                    }
                }
                Attempt::Fatal(_) => {}
            }
            attempt
        },
        || started.elapsed(),
        std::thread::sleep,
    )
}

/// One (allocated, logical) size sample for a file mid-hydration, or `None`
/// when it can't be read or has no known size. Allocation is block-granular,
/// so it's clamped to the logical size to never overshoot 100%.
fn hydration_sample(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let total = meta.len();
    if total == 0 {
        return None;
    }
    Some((allocated_bytes(&meta).min(total), total))
}

#[cfg(unix)]
fn allocated_bytes(meta: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(_meta: &Metadata) -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole feature rests on this predicate being precise: a false
    /// positive would silently stop Ken indexing ordinary local files.
    #[test]
    fn ordinary_local_file_is_not_dataless() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local.md");
        std::fs::write(&path, "# Local\nreal bytes on disk\n").unwrap();

        assert!(!is_dataless(&std::fs::metadata(&path).unwrap()));
        assert!(!is_placeholder(&path));
    }

    #[test]
    fn hydrating_a_local_file_is_a_no_op_that_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local.md");
        std::fs::write(&path, "bytes").unwrap();

        assert!(hydrate(&path).is_ok());
    }

    #[test]
    fn missing_file_is_not_reported_as_a_placeholder() {
        assert!(!is_placeholder(Path::new("/nonexistent/never/here.md")));
    }

    /// A fake clock: `sleep` only advances it, so the retry matrix below runs
    /// in microseconds instead of minutes.
    #[derive(Default)]
    struct FakeClock {
        elapsed: std::cell::Cell<Duration>,
        sleeps: std::cell::Cell<u32>,
    }

    impl FakeClock {
        fn sleep(&self, d: Duration) {
            self.elapsed.set(self.elapsed.get() + d);
            self.sleeps.set(self.sleeps.get() + 1);
        }
    }

    fn probe_path() -> &'static Path {
        Path::new("/OneDrive/02- Op Model/Use Case One-Pager Snapshots.pptx")
    }

    #[test]
    fn hydration_succeeds_on_the_first_attempt_when_the_bytes_are_already_local() {
        let clock = FakeClock::default();
        let attempts = std::cell::Cell::new(0);

        let out = poll_until_hydrated(
            probe_path(),
            Duration::from_secs(300),
            || {
                attempts.set(attempts.get() + 1);
                Attempt::Ready
            },
            || clock.elapsed.get(),
            |d| clock.sleep(d),
        );

        assert!(out.is_ok());
        assert_eq!(attempts.get(), 1);
        assert_eq!(clock.sleeps.get(), 0, "no sleeping when nothing to wait for");
    }

    /// The bug this whole module exists for: OneDrive's File Provider times the
    /// *read* out at ~60s while the download keeps running behind it.
    #[test]
    fn hydration_rides_out_provider_timeouts_and_succeeds_when_the_bytes_land() {
        let clock = FakeClock::default();
        let attempts = std::cell::Cell::new(0);

        let out = poll_until_hydrated(
            probe_path(),
            Duration::from_secs(300),
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() <= 3 {
                    Attempt::Downloading
                } else {
                    Attempt::Ready
                }
            },
            || clock.elapsed.get(),
            |d| clock.sleep(d),
        );

        assert!(out.is_ok(), "expected success after the download finished: {out:?}");
        assert_eq!(attempts.get(), 4);
        assert_eq!(clock.sleeps.get(), 3, "one sleep between each retry");
    }

    #[test]
    fn hydration_gives_up_at_the_deadline_with_a_message_the_user_can_act_on() {
        let clock = FakeClock::default();

        let out = poll_until_hydrated(
            probe_path(),
            Duration::from_secs(30),
            || Attempt::Downloading,
            || clock.elapsed.get(),
            |d| clock.sleep(d),
        );

        let msg = out.unwrap_err().to_string();
        assert!(msg.contains("still downloading"), "unhelpful message: {msg}");
        assert!(msg.contains("try again"), "no next step offered: {msg}");
        assert!(msg.contains("Use Case One-Pager Snapshots.pptx"), "no file named: {msg}");
        assert!(!msg.contains("os error"), "leaked an errno at the user: {msg}");
        assert!(clock.elapsed.get() >= Duration::from_secs(30));
    }

    /// A deleted or unreadable file is never going to arrive, so waiting out a
    /// multi-minute deadline would just be a hang with extra steps.
    #[test]
    fn a_missing_or_forbidden_file_fails_fast_instead_of_waiting() {
        for kind in [std::io::ErrorKind::NotFound, std::io::ErrorKind::PermissionDenied] {
            let clock = FakeClock::default();
            let out = poll_until_hydrated(
                probe_path(),
                Duration::from_secs(300),
                || Attempt::Fatal(std::io::Error::new(kind, "nope")),
                || clock.elapsed.get(),
                |d| clock.sleep(d),
            );

            assert!(out.is_err(), "{kind:?} should be an error");
            assert_eq!(clock.sleeps.get(), 0, "{kind:?} must not be retried");
        }
    }

    #[test]
    fn provider_timeouts_are_retryable_and_other_io_errors_are_not() {
        assert!(matches!(
            classify(std::io::Error::from(std::io::ErrorKind::TimedOut)),
            Attempt::Downloading
        ));
        // What macOS actually hands back: ETIMEDOUT from the File Provider.
        assert!(matches!(
            classify(std::io::Error::from_raw_os_error(60)),
            Attempt::Downloading
        ));
        assert!(matches!(
            classify(std::io::Error::from(std::io::ErrorKind::NotFound)),
            Attempt::Fatal(_)
        ));
    }

    #[test]
    fn hydration_sample_reports_allocated_vs_logical_and_skips_missing_files() {
        // A real file we just wrote is fully allocated: the sample must be
        // (total, total)-ish — allocated is block-rounded, so we clamp to len.
        let dir = std::env::temp_dir().join(format!("ken-hydration-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("sample.bin");
        std::fs::write(&f, vec![7u8; 10_000]).unwrap();
        let (got, total) = hydration_sample(&f).expect("sample for an existing file");
        assert_eq!(total, 10_000);
        assert_eq!(got, 10_000, "allocated bytes are clamped to the logical size");
        assert!(hydration_sample(&dir.join("missing.bin")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hydrate_with_progress_emits_a_final_full_sample_for_a_local_file() {
        // A plain local file hydrates on the first probe; the callback still gets
        // the terminal (total, total) so UIs can treat 100% as "done".
        let dir = std::env::temp_dir().join(format!("ken-hydrate-prog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("local.txt");
        std::fs::write(&f, b"already here").unwrap();
        let mut samples: Vec<(u64, u64)> = Vec::new();
        hydrate_with_progress(&f, Duration::from_secs(1), |d, t| samples.push((d, t))).unwrap();
        assert_eq!(samples.last().copied(), Some((12, 12)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
