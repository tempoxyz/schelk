// Command execution utilities
// Provides helpers for running external commands and checking tool availability

use std::ffi::OsStr;
use std::process::Stdio;

use eyre::{Result, WrapErr, eyre};
use tokio::process::Command;

/// Output from a command execution
#[derive(Debug)]
pub struct Output {
    #[allow(dead_code)]
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub code: Option<i32>,
}

/// Check if a command is available in PATH
pub async fn is_available(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run a command and capture its output, ignoring exit code
/// Returns an error only if the command fails to start
pub async fn run_unchecked<I, S>(cmd: &str, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .wrap_err_with(|| format!("Failed to execute {}", cmd))?;

    Ok(Output {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
        code: output.status.code(),
    })
}

/// Run a command and return an error if it fails (non-zero exit)
pub async fn run<I, S>(cmd: &str, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_unchecked(cmd, args).await?;

    if !output.success {
        let stderr = output.stderr.trim();
        let msg = if stderr.is_empty() {
            format!("{} failed with exit code {:?}", cmd, output.code)
        } else {
            format!("{} failed: {}", cmd, stderr)
        };
        return Err(eyre!(msg));
    }

    Ok(output)
}

/// Check that a command exists, returning a descriptive error if not
pub async fn require(cmd: &str, package_hint: &str) -> Result<()> {
    if !is_available(cmd).await {
        return Err(eyre!(
            "{} not found in PATH. Install {}.",
            cmd,
            package_hint
        ));
    }
    Ok(())
}

// TODO: It would be great to centralize checks and execution in here to avoid `Command::new`
// everywhere.
