//! Core of the `carla-pack` CLI. All network, authentication, credential storage,
//! download and pack installation logic lives here; the Python package only parses arguments and prints.

pub mod auth;
pub mod client;
pub mod error;
pub mod pack;
pub mod store;

#[cfg(feature = "python")]
mod python;

pub use client::Client;
pub use error::{Error, Result};
