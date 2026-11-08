//! Opt-in fixtures for real sessions, loopback HTTP, and recording faults.
//! Normal builds exclude this module. Private core inspection stays test-only.
mod http;
mod tools;

pub use http::{Reply, SUCCESS, Server};
pub use tools::read_file;

#[cfg(unix)]
pub use crate::session::RecordingControl;

#[cfg(all(test, unix))]
mod agent;
#[cfg(unix)]
mod journal;

#[cfg(all(test, unix))]
pub(crate) use agent::agent;
#[cfg(unix)]
pub use journal::{events, log};

#[cfg(all(test, unix))]
mod tests;
