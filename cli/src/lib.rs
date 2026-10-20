//! Core of the `carla-pack` CLI. All network, authentication, credential storage and
//! download logic lives here; the Python package only parses arguments and prints.

pub mod auth;
pub mod client;
pub mod error;
pub mod store;

#[cfg(feature = "python")]
mod python;

pub use client::Client;
pub use error::{Error, Result};
