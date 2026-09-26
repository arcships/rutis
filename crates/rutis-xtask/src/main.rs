use std::process::{self, Command};

fn main() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tools/pack-dylib-plugin.py"
    );
    let status = Command::new("python3")
        .arg(script)
        .args(std::env::args_os().skip(1))
        .status()
        .unwrap_or_else(|error| {
            eprintln!("failed to run pack-plugin: {error}");
            process::exit(1);
        });
    process::exit(status.code().unwrap_or(1));
}
