#[cfg(not(target_os = "linux"))]
compile_error!("schelk is Linux-only and is not supposed to be compiled on this platform");

fn main() {}
