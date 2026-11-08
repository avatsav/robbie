//! Shared unit-test fixtures; compiled only through #[cfg(test)].
mod http;

pub(crate) use http::{Reply, SUCCESS, Server};

#[cfg(unix)]
mod agent;
#[cfg(unix)]
mod journal;

#[cfg(unix)]
pub(crate) use agent::agent;
#[cfg(unix)]
pub(crate) use journal::{events, log};
