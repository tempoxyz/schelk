fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    println!("cargo:rerun-if-env-changed=TARGET");

    let target_os =
        std::env::var("CARGO_CFG_TARGET_OS").expect("Cargo did not set CARGO_CFG_TARGET_OS");

    if target_os != "linux" {
        let target = std::env::var("TARGET").unwrap_or_else(|_| "<unknown>".to_string());
        panic!(
            "schelk only supports Linux targets; requested target `{}` (target_os=`{}`)",
            target, target_os
        );
    }

    // Embed git SHA for --version output.
    // Works with `cargo install --path .` (has .git dir) and
    // `cargo install --git ...` (cargo sets GIT_SHA env var).
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
    println!("cargo:rerun-if-env-changed=GIT_SHA");

    let sha = std::env::var("GIT_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        });

    if let Some(sha) = sha {
        println!("cargo:rustc-env=SCHELK_GIT_SHA={}", sha);
    }
}
