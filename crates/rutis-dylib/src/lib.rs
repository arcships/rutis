//! Optional in-process loader for trusted, first-party Rust dylib plugins.
//! The dynamic loader is available on Linux; other targets retain the static
//! host build without compiling platform-specific loader code.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;
