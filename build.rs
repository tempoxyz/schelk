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
}
