// Command: status
// Reports the current status of schelk
//
// Displays:
//   - Whether schelk is initialized
//   - Configured volume paths (virgin, scratch, ramdisk)
//   - Granularity setting
//   - Whether the volume is currently mounted
//   - Current dm-era epoch (if mounted)
//   - State file location

use eyre::Result;

use crate::dmera;
use crate::mount;
use crate::state;

/// Run the status command
/// Reports the current status of schelk configuration and runtime state
pub async fn run() -> Result<()> {
    let state_path = state::state_path()?;

    match state::load()? {
        None => {
            println!("schelk is not initialized.");
            println!();
            println!("Run 'schelk init' to configure schelk with your volumes.");
        }
        Some(state) => {
            println!("schelk status");
            println!("=============");
            println!();
            println!("State file: {}", state_path.display());
            println!();
            println!("Configuration:");
            println!("  Virgin volume:  {}", state.virgin.display());
            println!("  Scratch volume: {}", state.scratch.display());
            println!("  RAM disk:       {}", state.ramdisk.display());
            println!("  Mount point:    {}", state.mount_point.display());
            if let Some(ref opts) = state.mount_options {
                println!("  Mount options:  {}", opts);
            }
            println!("  Granularity:    {} bytes", state.granularity);
            println!();
            println!("Runtime status:");
            let device_exists = dmera::exists(dmera::DM_ERA_NAME).await.unwrap_or(false);
            let actually_mounted = mount::is_mounted(&state.mount_point).unwrap_or(false);

            if state.is_mounted {
                println!("  Mounted: yes (state)");
                if actually_mounted {
                    println!("  Mounted: yes (actual)");
                } else {
                    println!("  Mounted: NO (actual) - state inconsistent!");
                }
                if device_exists {
                    println!("  dm-era device: active");
                } else {
                    println!("  dm-era device: MISSING (state inconsistent, possible crash)");
                }
                if let Some(era) = state.current_era {
                    println!("  Current era: {}", era);
                }
            } else {
                println!("  Mounted: no (state)");
                if actually_mounted {
                    println!("  Mounted: YES (actual) - state inconsistent!");
                } else {
                    println!("  Mounted: no (actual)");
                }
                if device_exists {
                    println!("  dm-era device: EXISTS (state inconsistent, stale device?)");
                } else {
                    println!("  dm-era device: none");
                }
            }
            println!();
            println!("Volume checksum:");
            println!(
                "  Superblock hash: {}",
                hex_encode(&state.virgin_superblock_hash)
            );
        }
    }

    Ok(())
}

/// Encode bytes as lowercase hex string
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}
