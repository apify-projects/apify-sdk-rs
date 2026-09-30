//! Apify SDK for Rust: run [crawlee-rs](https://github.com/apify-projects/crawlee-rs) crawlers as
//! Actors on the [Apify platform](https://apify.com), a port of the JS SDK (`apify` on npm).
//!
//! See `docs/plan.md` for the scope and the milestones.

pub mod client;
pub mod configuration;
pub mod storage;

pub use crate::client::{RateLimitCounter, new_client};
pub use crate::configuration::Configuration;
pub use crate::storage::{ApifyStorageBackend, RequestQueueAccess, SmartStorageBackend};

pub use apify_client::ApifyClient;
