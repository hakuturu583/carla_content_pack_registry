//! Core of `carla-pack`: CARLA content packs shared through GitHub and S3-compatible storage
//! (or, for public packs, the Hugging Face Hub).
//!
//! There is no server. A pack repository on GitHub holds `carla-packs.toml` (where the
//! packs are stored, and the SHA-256 of each) and, as a release asset, read-only storage
//! credentials that a scheduled workflow rotates. Reading both through the GitHub API with
//! the user's own token is the access check; the pack itself is downloaded straight from
//! the bucket, verified, and installed. The Python package only parses arguments and
//! adds the CARLA side (mounting).

pub mod auth;
pub mod client;
pub mod error;
pub mod github;
pub mod hf;
pub mod manifest;
pub mod pack;
pub mod s3;
pub mod sigv4;
pub mod store;
pub mod time;

#[cfg(feature = "python")]
mod python;

pub use client::{Client, ClientOptions, PublishOptions, Release, publish};
pub use error::{Error, Result};
pub use manifest::{Filter, PackRef};
