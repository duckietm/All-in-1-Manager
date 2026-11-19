//! Scripts on the server the Manager is logged in to (permission `ssh`, or `ssh:root` for [`run_as_root`]).
//!
//! Before a script runs, the user sees all of it and approves it; a user who says no gives
//! [`ErrorKind::Rejected`](crate::ErrorKind::Rejected). The Manager's rules:
//! - a script has 1 byte to 64 KiB, with Unix line endings: a carriage return, any other control character but tab
//!   and newline, and invisible or text-direction characters are refused, so that the user sees exactly what runs;
//! - a missing permission or a refused script is [`ErrorKind::Denied`](crate::ErrorKind::Denied); the SDK refuses
//!   an empty or too large script and control characters itself, before asking the Manager;
//! - a script has 10 minutes once approved ([`ErrorKind::Timeout`](crate::ErrorKind::Timeout)); the time the user
//!   takes to decide does not count.
//!
//! ```no_run
//! # fn main() -> aio_plugin_sdk::Result<()> {
//! let result = aio_plugin_sdk::script::run("uptime\n")?;
//! if result.success() {
//!     aio_plugin_sdk::host::status(result.output.trim());
//! }
//! # Ok(())
//! # }
//! ```

use crate::bindings::aio::plugin::host;
use crate::error::{host_result, Error, Result};

/// The largest script (bytes, the Manager's limit).
pub const MAX_SCRIPT: usize = 64 * 1024;

/// What a script did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScriptResult {
    /// The exit code (0 for success).
    pub exit_code: i32,
    /// What the script wrote to stdout.
    pub output: String,
    /// What the script wrote to stderr.
    pub error: String,
}

impl ScriptResult {
    /// `true` if the exit code is 0.
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Runs `script` as the logged-in user, after the user approves it (permission `ssh`).
pub fn run(script: &str) -> Result<ScriptResult> {
    run_script(script, false)
}

/// Runs `script` with root rights, after the user approves it (permission `ssh:root`).
pub fn run_as_root(script: &str) -> Result<ScriptResult> {
    run_script(script, true)
}

fn run_script(script: &str, as_root: bool) -> Result<ScriptResult> {
    check(script)?;
    let r = host_result(host::run_script(script, as_root))?;
    Ok(ScriptResult { exit_code: r.exit_code, output: r.output, error: r.error })
}

/// The refusals of the host (approvals.rs `check_script`) the SDK can see on its own: an empty or too large
/// script, and control characters other than tab and newline. The host also refuses invisible and text-direction
/// characters.
fn check(script: &str) -> Result<()> {
    if script.is_empty() {
        return Err(Error::denied("an empty script"));
    }
    if script.len() > MAX_SCRIPT {
        return Err(Error::denied(format!("the script is larger than {} KiB", MAX_SCRIPT / 1024)));
    }
    if script.chars().any(|c| c.is_control() && c != '\t' && c != '\n') {
        return Err(Error::denied(
            "the script holds control or invisible characters (only tab and newline are allowed), so it could not be \
             shown exactly as it would run",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    #[test]
    fn scripts_the_host_would_refuse_are_refused_early() {
        let refused = |script: &str| {
            let error = check(script).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Denied);
            error.message().to_string()
        };
        // The host's texts (approvals.rs `check_script`).
        assert_eq!(refused(""), "permission denied: an empty script");
        assert_eq!(refused(&"a".repeat(MAX_SCRIPT + 1)), "permission denied: the script is larger than 64 KiB");
        let control = "permission denied: the script holds control or invisible characters (only tab and newline are \
                       allowed), so it could not be shown exactly as it would run";
        assert_eq!(refused("echo a\r\necho b\r\n"), control);
        assert_eq!(refused("echo \u{1b}[31m"), control);
        assert_eq!(refused("echo \u{7f}"), control);

        check(&"a".repeat(MAX_SCRIPT)).unwrap();
        check("if true; then\n\techo ok\nfi\n").unwrap();
    }
}
