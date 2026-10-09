//! Child-process spawn hygiene.
//!
//! Ken shells out to `git`, `claude` and `ffmpeg` from several places. On
//! Windows every one of those spawns pops a console window for as long as the
//! child lives (the release app is a GUI-subsystem exe, the children are
//! console exes), and closing that window kills the child. `CREATE_NO_WINDOW`
//! gives the child a hidden console instead, which its own children (node,
//! ken-mcp) inherit. PTY spawns (ConPTY) need none of this.
//!
//! This lives in its own module rather than in `sync.rs` because
//! `family_sync.rs` and the two downstream crates need it too, and none of
//! them should have to depend on the sync engine to spawn a quiet child.

use std::process::Command;

/// Windows `CREATE_NO_WINDOW` — no console is allocated for the child.
/// Defined here rather than pulled from `winapi`/`windows-sys` to avoid a
/// dependency for a single constant.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Suppress the console window a child process would otherwise get.
///
/// No-op off Windows, so callers can apply it unconditionally rather than
/// scattering `#[cfg(windows)]` through their spawn sites.
pub fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Spawn `cmd` with `input` written to its stdin, which is then closed: the
/// way to hand `claude -p` a prompt. On Windows the CLI is the `.cmd`
/// launcher npm installs, and std refuses any argument to a batch file that
/// holds a line break ("batch file arguments are invalid"), so a prompt
/// passed as an argument fails to spawn at all. Written on its own thread so
/// a child that reads slowly can never deadlock the caller. The child gets no
/// console window ([`quiet`]): Ken's release build is a GUI app, so a console
/// child would otherwise open a window of its own, and closing that window
/// kills the run.
pub fn spawn_with_input(cmd: &mut Command, input: &str) -> std::io::Result<std::process::Child> {
    use std::io::Write;
    quiet(cmd);
    cmd.stdin(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    track(&child);
    if let Some(mut stdin) = child.stdin.take() {
        let text = input.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    Ok(child)
}

/// Put a just-spawned child in its own Windows job object, so `kill_tree`
/// can end everything it starts. A `.cmd` launcher's real work runs in a
/// grandchild (node, for claude) that `Child::kill` would leave running with
/// the pipes open. The job is a kernel object, not another process: no
/// `taskkill`, which endpoint security reads as hostile. The job is set to
/// kill on close, so the children also end if Ken exits. No-op off Windows.
pub fn track(child: &std::process::Child) {
    #[cfg(windows)]
    jobs::track(child);
    #[cfg(not(windows))]
    let _ = child;
}

/// Kill a child and everything it started (see `track`). A child that was
/// never tracked is killed alone.
pub fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    jobs::terminate(child.id());
    let _ = child.kill();
}

#[cfg(windows)]
mod jobs {
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// Job handles by the child's pid, kept as integers so the map is `Send`.
    static JOBS: Mutex<Option<HashMap<u32, isize>>> = Mutex::new(None);

    pub fn track(child: &std::process::Child) {
        // SAFETY: plain Win32 calls on a handle we own; every failure path
        // closes it and leaves the child untracked (killed alone, as before).
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set == 0 || AssignProcessToJobObject(job, child.as_raw_handle() as _) == 0 {
                CloseHandle(job);
                return;
            }
            let mut guard = JOBS.lock().unwrap_or_else(|e| e.into_inner());
            let map = guard.get_or_insert_with(HashMap::new);
            // Drop the jobs whose processes have all ended, so a long session
            // does not collect handles.
            map.retain(|_, h| {
                let alive = active_processes(*h as _) != Some(0);
                if !alive {
                    CloseHandle(*h as _);
                }
                alive
            });
            if let Some(old) = map.insert(child.id(), job as isize) {
                CloseHandle(old as _);
            }
        }
    }

    pub fn terminate(pid: u32) {
        let job = JOBS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .and_then(|m| m.remove(&pid));
        if let Some(h) = job {
            // SAFETY: the handle came from `track` and is closed exactly once.
            unsafe {
                TerminateJobObject(h as _, 1);
                CloseHandle(h as _);
            }
        }
    }

    unsafe fn active_processes(job: *mut c_void) -> Option<u32> {
        let mut acct: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
        let ok = QueryInformationJobObject(
            job,
            JobObjectBasicAccountingInformation,
            &mut acct as *mut _ as *mut c_void,
            std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            std::ptr::null_mut(),
        );
        (ok != 0).then_some(acct.ActiveProcesses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `quiet` must stay chainable and must not disturb the command it is
    /// handed — the spawn sites apply it inline in the middle of a builder
    /// chain.
    #[test]
    fn quiet_is_chainable_and_preserves_the_command() {
        let mut cmd = Command::new("git");
        cmd.arg("--version");
        let built = quiet(&mut cmd);
        assert_eq!(built.get_program(), "git");
        let args: Vec<_> = built.get_args().collect();
        assert_eq!(args, ["--version"]);
    }
}
