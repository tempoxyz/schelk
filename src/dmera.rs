// dm-era device mapper operations
// Manages creation, snapshot, and teardown of dm-era targets for write tracking

use std::path::Path;
use std::process::Command;

use eyre::{Result, WrapErr, eyre};
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::cmd;
use crate::volume;

/// Default device name for the dm-era target
pub const DM_ERA_NAME: &str = "bench_era";

/// Path to the dm-era device in /dev/mapper
pub fn device_path() -> std::path::PathBuf {
    std::path::PathBuf::from("/dev/mapper").join(DM_ERA_NAME)
}

/// Check that dmsetup is available in PATH
pub async fn check_dmsetup() -> Result<()> {
    cmd::require("dmsetup", "device-mapper tools (e.g., apt install dmsetup)").await
}

/// Check that era_invalidate is available in PATH and warn if version is < 1.0
pub async fn check_era_invalidate() -> Result<()> {
    cmd::require(
        "era_invalidate",
        "thin-provisioning-tools (e.g., apt install thin-provisioning-tools)",
    )
    .await?;

    // Check version and if pre-1.0 then warn because it's not written in Rust and as such is not
    // blazingly fast.
    if let Some(version) = get_era_invalidate_version() {
        if is_version_below_1_0(&version) {
            eprintln!(
                "Warning: era_invalidate version {} is slow. \
                 For better performance, compile version 1.0+ from \
                 https://github.com/device-mapper-utils/thin-provisioning-tools",
                version
            );
        }
    }

    Ok(())
}

/// Get the version string from era_invalidate -V
fn get_era_invalidate_version() -> Option<String> {
    let output = Command::new("era_invalidate").arg("-V").output().ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    // Output is just the version number, e.g., "0.9.0\n"
    Some(stdout.trim().to_string())
}

/// Check if version string represents a version below 1.0
fn is_version_below_1_0(version: &str) -> bool {
    // Parse first component of version (e.g., "0" from "0.9.0")
    version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major < 1)
}

/// Check if dm-era device with given name exists
pub async fn exists(name: &str) -> Result<bool> {
    let output = cmd::run_unchecked("dmsetup", ["info", name]).await?;
    Ok(output.success)
}

/// Create a dm-era device
///
/// Runs: dmsetup create <name> --table "0 <sectors> era <metadata_dev> <origin_dev> <block_size>"
///
/// - metadata_dev: RAM disk for storing era metadata
/// - origin_dev: The actual block device (scratch volume)
/// - block_size: Granularity in 512-byte sectors
pub async fn create(
    name: &str,
    metadata_dev: &Path,
    origin_dev: &Path,
    origin_size_bytes: u64,
    granularity: u64,
) -> Result<()> {
    // Convert bytes to 512-byte sectors
    let sectors = origin_size_bytes / 512;

    // Granularity in sectors (dm-era expects sectors, not bytes)
    let block_size_sectors = granularity / 512;

    // Table format: "0 <length> era <metadata dev> <origin dev> <block size>"
    let table = format!(
        "0 {} era {} {} {}",
        sectors,
        metadata_dev.display(),
        origin_dev.display(),
        block_size_sectors
    );

    cmd::run("dmsetup", ["create", name, "--table", &table])
        .await
        .wrap_err("Failed to create dm-era device")?;

    Ok(())
}

/// Send checkpoint message to dm-era device
/// This marks the current era and starts tracking writes from this point
pub async fn checkpoint(name: &str) -> Result<()> {
    cmd::run("dmsetup", ["message", name, "0", "checkpoint"])
        .await
        .wrap_err("Failed to checkpoint dm-era device")?;
    Ok(())
}

/// Remove a dm-era device
pub async fn remove(name: &str) -> Result<()> {
    cmd::run("dmsetup", ["remove", name])
        .await
        .wrap_err("Failed to remove dm-era device")?;
    Ok(())
}

/// Take a metadata snapshot for userspace reading
pub async fn take_metadata_snapshot(name: &str) -> Result<()> {
    cmd::run("dmsetup", ["message", name, "0", "take_metadata_snap"])
        .await
        .wrap_err("Failed to take dm-era metadata snapshot")?;
    Ok(())
}

/// Drop the metadata snapshot
pub async fn drop_metadata_snapshot(name: &str) -> Result<()> {
    cmd::run("dmsetup", ["message", name, "0", "drop_metadata_snap"])
        .await
        .wrap_err("Failed to drop dm-era metadata snapshot")?;
    Ok(())
}

/// Get changed blocks since a given era by calling the era_invalidate binary
///
/// Runs: era_invalidate --metadata-snapshot --written-since <era> <metadata_dev>
/// and parses the XML output to extract all blocks that have been written.
pub fn get_changed_blocks(metadata_dev: &Path, since_era: u32) -> Result<Vec<volume::BlockRange>> {
    let output = Command::new("era_invalidate")
        .arg("--metadata-snapshot")
        .arg("--written-since")
        .arg(since_era.to_string())
        .arg(metadata_dev)
        .output()
        .wrap_err("Failed to execute era_invalidate")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("era_invalidate failed: {}", stderr.trim()));
    }

    let xml_content =
        String::from_utf8(output.stdout).wrap_err("era_invalidate output is not valid UTF-8")?;

    parse_era_invalidate_xml(&xml_content)
}

/// Parse era_invalidate XML output to extract block ranges using streaming XML parser
///
/// Example XML:
/// ```xml
/// <blocks>
///   <range begin="0" end = "10"/>
///   <block block="50"/>
///   <range begin="100" end = "150"/>
/// </blocks>
/// ```
///
/// Note: era_invalidate outputs spaces around `=` in attributes (e.g., `end = "10"`)
/// and uses both `<range>` for consecutive blocks and `<block>` for single blocks.
fn parse_era_invalidate_xml(xml: &str) -> Result<Vec<volume::BlockRange>> {
    let mut ranges = Vec::new();
    let mut reader = Reader::from_str(xml);

    loop {
        match reader.read_event() {
            Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"range" => {
                    let mut begin: Option<u64> = None;
                    let mut end: Option<u64> = None;

                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"begin" => {
                                let val = attr
                                    .unescape_value()
                                    .wrap_err("Invalid UTF-8 in 'begin' attribute")?;
                                begin = Some(val.parse().wrap_err("Invalid 'begin' value")?);
                            }
                            b"end" => {
                                let val = attr
                                    .unescape_value()
                                    .wrap_err("Invalid UTF-8 in 'end' attribute")?;
                                end = Some(val.parse().wrap_err("Invalid 'end' value")?);
                            }
                            _ => {}
                        }
                    }

                    let begin = begin.ok_or_else(|| eyre!("Missing 'begin' attribute"))?;
                    let end = end.ok_or_else(|| eyre!("Missing 'end' attribute"))?;

                    if end > begin {
                        ranges.push(volume::BlockRange {
                            start: begin,
                            len: end - begin,
                        });
                    }
                }
                b"block" => {
                    let mut block: Option<u64> = None;

                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"block" {
                            let val = attr
                                .unescape_value()
                                .wrap_err("Invalid UTF-8 in 'block' attribute")?;
                            block = Some(val.parse().wrap_err("Invalid 'block' value")?);
                        }
                    }

                    let block = block.ok_or_else(|| eyre!("Missing 'block' attribute"))?;
                    ranges.push(volume::BlockRange {
                        start: block,
                        len: 1,
                    });
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(eyre!("XML parse error: {}", e)),
            _ => {}
        }
    }

    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_era_invalidate_xml() {
        let xml = r#"
<blocks>
  <range begin="0" end="10"/>
  <range begin="100" end="150"/>
</blocks>
"#;
        let ranges = parse_era_invalidate_xml(xml).unwrap();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].len, 10);
        assert_eq!(ranges[1].start, 100);
        assert_eq!(ranges[1].len, 50);
    }

    #[test]
    fn test_version_below_1_0() {
        assert!(is_version_below_1_0("0.9.0"));
        assert!(is_version_below_1_0("0.1.0"));
        assert!(is_version_below_1_0("0.9.0-rc1"));
        assert!(!is_version_below_1_0("1.0.0"));
        assert!(!is_version_below_1_0("1.0.12"));
        assert!(!is_version_below_1_0("2.0.0"));
    }

    #[test]
    fn test_parse_era_invalidate_xml_real_format() {
        // Real format from era_invalidate: spaces around = and <block> elements
        let xml = r#"
<blocks>
  <range begin="0" end = "2"/>
  <block block="1028"/>
  <block block="1043"/>
  <range begin="1081363" end = "1081374"/>
</blocks>
"#;
        let ranges = parse_era_invalidate_xml(xml).unwrap();
        assert_eq!(ranges.len(), 4);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].len, 2);
        assert_eq!(ranges[1].start, 1028);
        assert_eq!(ranges[1].len, 1);
        assert_eq!(ranges[2].start, 1043);
        assert_eq!(ranges[2].len, 1);
        assert_eq!(ranges[3].start, 1081363);
        assert_eq!(ranges[3].len, 11);
    }

    /// Integration test for dm-era setup
    ///
    /// This test requires:
    /// - Root privileges
    /// - dmsetup installed
    /// - Loop devices available
    ///
    /// Run with: sudo -E $(which cargo) test dmera_integration -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn test_dmera_integration() {
        use std::process::Command;

        const TEST_SIZE_MB: u64 = 10;
        const TEST_GRANULARITY: u64 = 4096;
        const TEST_NAME: &str = "schelk_test_era";

        let test_dir = std::path::PathBuf::from("/tmp/schelk-dmera-test");
        let origin_img = test_dir.join("origin.img");
        let metadata_img = test_dir.join("metadata.img");

        // Setup test environment
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).expect("Failed to create test dir");

        // Create test images
        let status = Command::new("dd")
            .args([
                "if=/dev/zero",
                &format!("of={}", origin_img.display()),
                "bs=1M",
                &format!("count={}", TEST_SIZE_MB),
            ])
            .output()
            .expect("Failed to create origin image");
        assert!(status.status.success(), "Failed to create origin image");

        let status = Command::new("dd")
            .args([
                "if=/dev/zero",
                &format!("of={}", metadata_img.display()),
                "bs=1M",
                "count=2",
            ])
            .output()
            .expect("Failed to create metadata image");
        assert!(status.status.success(), "Failed to create metadata image");

        // Set up loop devices using --find to get available ones
        let output = Command::new("losetup")
            .args(["--find", "--show", &origin_img.to_string_lossy()])
            .output()
            .expect("Failed to setup origin loop");
        assert!(
            output.status.success(),
            "Failed to setup origin loop: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        let origin_loop = std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        println!("Origin loop device: {}", origin_loop.display());

        let output = Command::new("losetup")
            .args(["--find", "--show", &metadata_img.to_string_lossy()])
            .output()
            .expect("Failed to setup metadata loop");
        assert!(
            output.status.success(),
            "Failed to setup metadata loop: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata_loop =
            std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        println!("Metadata loop device: {}", metadata_loop.display());

        // Cleanup closure
        let origin_loop_c = origin_loop.clone();
        let metadata_loop_c = metadata_loop.clone();
        let test_dir_c = test_dir.clone();
        let cleanup = move || {
            let _ = Command::new("dmsetup").args(["remove", TEST_NAME]).output();
            let _ = Command::new("losetup")
                .args(["-d", &origin_loop_c.to_string_lossy()])
                .output();
            let _ = Command::new("losetup")
                .args(["-d", &metadata_loop_c.to_string_lossy()])
                .output();
            let _ = std::fs::remove_dir_all(&test_dir_c);
        };

        // Run tests
        let result = async {
            println!("Test 1: Checking dmsetup availability...");
            check_dmsetup().await?;
            println!("  OK");

            println!("Test 2: Verifying device doesn't exist...");
            assert!(!exists(TEST_NAME).await?, "Device should not exist yet");
            println!("  OK");

            println!("Test 3: Creating dm-era device...");
            let origin_size = TEST_SIZE_MB * 1024 * 1024;
            create(
                TEST_NAME,
                &metadata_loop,
                &origin_loop,
                origin_size,
                TEST_GRANULARITY,
            )
            .await?;
            println!("  OK");

            println!("Test 4: Verifying device exists...");
            assert!(
                exists(TEST_NAME).await?,
                "Device should exist after creation"
            );
            println!("  OK");

            println!("Test 5: Sending checkpoint...");
            checkpoint(TEST_NAME).await?;
            println!("  OK");

            println!("Test 6: Verifying /dev/mapper entry...");
            let dm_path = std::path::PathBuf::from("/dev/mapper").join(TEST_NAME);
            assert!(dm_path.exists(), "Device mapper path should exist");
            println!("  OK");

            println!("Test 7: Removing dm-era device...");
            remove(TEST_NAME).await?;
            println!("  OK");

            println!("Test 8: Verifying device is gone...");
            assert!(
                !exists(TEST_NAME).await?,
                "Device should not exist after removal"
            );
            println!("  OK");

            Ok::<(), eyre::Report>(())
        }
        .await;

        cleanup();

        if let Err(e) = result {
            panic!("Test failed: {}", e);
        }

        println!("\nAll dm-era integration tests passed!");
    }
}
