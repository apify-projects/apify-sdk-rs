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

## Usage

```rust
#[tokio::main]
async fn main() {
    apify::main(|actor| async move {
        let input: serde_json::Value = actor.get_input().await?;
        // Crawlers built from here on store their data where the Actor does: in the platform
        // storages on Apify, in ./storage elsewhere.
        actor.push_data(&input).await?;
        Ok(())
    })
    .await;
}
```

`templates/actor` is a complete Actor (Dockerfile, `.actor/actor.json`, input schema) with a
crawlee-rs crawler. Use crawlee-rs through `apify::crawlee`, so that the SDK and the crawler
share one version of it.

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Minimum supported Rust version: 1.88.
