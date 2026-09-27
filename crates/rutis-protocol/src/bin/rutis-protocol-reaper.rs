//! Dedicated, single-threaded Linux subreaper. Business output is diagnostic;
//! launch/reaping evidence uses the separate inherited private fd 4.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    rutis_protocol::process::run_reaper().map_err(Into::into)
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("rutis-protocol-reaper requires Linux");
    std::process::exit(1);
}
