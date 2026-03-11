// User confirmation prompts
// Handles interactive confirmation for destructive operations

use std::io::{self, BufRead, Write};

use eyre::{Result, eyre};

/// Prompt the user for confirmation
/// Returns true if user confirms (y/yes), false otherwise
pub fn prompt(message: &str) -> Result<bool> {
    print!("{} [y/N] ", message);
    io::stdout().flush()?;

    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;

    let response = line.trim().to_lowercase();
    Ok(response == "y" || response == "yes")
}

/// Require user confirmation for a destructive operation.
///
/// If `skip` is true (from a CLI flag), prints `[auto-confirmed]` and returns
/// `Ok` without prompting. Otherwise prompts interactively and returns an error
/// if the user declines.
pub fn require(message: &str, skip: bool) -> Result<()> {
    if skip {
        println!("[auto-confirmed] {}", message);
        return Ok(());
    }

    if !prompt(message)? {
        return Err(eyre!("Operation cancelled by user."));
    }

    Ok(())
}
