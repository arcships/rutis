//! The library behind the `rutis-host` binary: reading `rutis.json`, making
//! a plugin project into a configuration, and starting what it names.
//!
//! It is public for the binary and its tests, not as an API: it changes with
//! the binary, without a semver promise.

pub mod config;
pub mod host;
pub mod new;
pub mod project;
pub mod status;
