//! Child-process spawn hygiene.
//!
//! Ken shells out to `git` from several places (`sync`, `family_sync`, and
//! `src-tauri`'s family create/join helper). On Windows every one of those
//! spawns pops a console window for as long as the child lives — roughly a
//! second for `git remote`, which reads as flickering terminals when the
//! sync engine is doing its job. `CREATE_NO_WINDOW` suppresses it.
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
