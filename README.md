# Apify SDK for Rust

Run [crawlee-rs](https://github.com/apify-projects/crawlee-rs) crawlers as Actors on the
[Apify platform](https://apify.com). A port of the [JS SDK](https://github.com/apify/apify-sdk-js),
using the Rust [Apify API client](https://github.com/apify/apify-client-rust).

**Status: in development, not published.** See [docs/plan.md](docs/plan.md) for the scope and
milestones, and [docs/allowed-differences.md](docs/allowed-differences.md) for deliberate
differences from the JS SDK.

## Install

The crate is not on crates.io yet. Depend on it by git:

```toml
[dependencies]
apify = { git = "https://github.com/apify-projects/apify-sdk-rs" }
```

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Minimum supported Rust version: 1.88.
