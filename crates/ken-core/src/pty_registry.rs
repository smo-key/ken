//! Registry of live hidden-PTY sessions (ingest runs). Lets the chat drawer
//! attach to a session the runner already owns — watch output live, type
//! into it — instead of spawning a second process on the same session.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};

const BACKLOG_CAP: usize = 200 * 1024;

type Tap = Box<dyn Fn(&[u8]) + Send>;

struct Live {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    tap: Arc<Mutex<Option<Tap>>>,
    backlog: Arc<Mutex<VecDeque<u8>>>,
}

fn registry() -> &'static Mutex<HashMap<String, Live>> {
    static REG: OnceLock<Mutex<HashMap<String, Live>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Handles the runner keeps while its session lives.
pub struct Registration {
    session_id: String,
    tap: Arc<Mutex<Option<Tap>>>,
    backlog: Arc<Mutex<VecDeque<u8>>>,
}

impl Registration {
    /// Called by the runner's reader thread for every output chunk.
    pub fn broadcast(&self, bytes: &[u8]) {
        {
            let mut b = self.backlog.lock().unwrap();
            b.extend(bytes);
            while b.len() > BACKLOG_CAP {
                b.pop_front();
            }
        }
        if let Some(tap) = self.tap.lock().unwrap().as_ref() {
            tap(bytes);
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        registry().lock().unwrap().remove(&self.session_id);
    }
}

/// Register a live session. The returned handle broadcasts output and
/// unregisters on drop.
pub fn register(
    session_id: &str,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
) -> Registration {
    let tap: Arc<Mutex<Option<Tap>>> = Arc::new(Mutex::new(None));
    let backlog = Arc::new(Mutex::new(VecDeque::new()));
    registry().lock().unwrap().insert(
        session_id.to_string(),
        Live {
            writer,
            tap: tap.clone(),
            backlog: backlog.clone(),
        },
    );
    Registration {
        session_id: session_id.to_string(),
        tap,
        backlog,
    }
}

pub fn is_live(session_id: &str) -> bool {
    registry().lock().unwrap().contains_key(session_id)
}

/// Attach a viewer: replays the backlog immediately, then receives live
/// chunks. Replaces any previous viewer. Returns false if not live.
pub fn attach(session_id: &str, on_data: Tap) -> bool {
    let reg = registry().lock().unwrap();
    let Some(live) = reg.get(session_id) else {
        return false;
    };
    {
        let b = live.backlog.lock().unwrap();
        if !b.is_empty() {
            let bytes: Vec<u8> = b.iter().copied().collect();
            on_data(&bytes);
        }
    }
    *live.tap.lock().unwrap() = Some(on_data);
    true
}

pub fn detach(session_id: &str) {
    if let Some(live) = registry().lock().unwrap().get(session_id) {
        *live.tap.lock().unwrap() = None;
    }
}

/// Send input to a live session (e.g. answering a blocked ingest's
/// question). Returns false if not live.
pub fn input(session_id: &str, bytes: &[u8]) -> bool {
    let reg = registry().lock().unwrap();
    let Some(live) = reg.get(session_id) else {
        return false;
    };
    let mut w = live.writer.lock().unwrap();
    w.write_all(bytes).and_then(|_| w.flush()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn register_attach_input_lifecycle() {
        let sink: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        struct VecWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for VecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(VecWriter(sink.clone()))));

        let reg = register("sess-live", writer);
        assert!(is_live("sess-live"));

        // Output before a viewer lands in the backlog…
        reg.broadcast(b"early output ");

        // …and replays on attach.
        let (tx, rx) = channel::<Vec<u8>>();
        assert!(attach("sess-live", Box::new(move |b| {
            let _ = tx.send(b.to_vec());
        })));
        let replay = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(replay, b"early output ");

        reg.broadcast(b"live chunk");
        let live = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(live, b"live chunk");

        assert!(input("sess-live", b"y\r"));
        assert_eq!(sink.lock().unwrap().as_slice(), b"y\r");

        drop(reg);
        assert!(!is_live("sess-live"));
        assert!(!input("sess-live", b"x"));
    }
}
