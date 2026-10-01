# Conformance with the JS SDK

The [JS SDK](https://github.com/apify/apify-sdk-js) is the reference for this crate's behavior.

| Path | What it is |
|---|---|
| `oracle/generate-golden.mts` | Runs the compiled JS SDK and records its outputs for a list of inputs |
| `golden/*.json` | The recorded cases, committed. `tests/golden.rs` (and unit tests in `src/charging.rs` and `src/input.rs`) replay them |

Covered: the configuration from environment variables, pay-per-event charging (budget limits,
over-charging by one, `toFixed` rounding), Apify Proxy URLs, pseudo-URLs, input value parsing and
input schema defaults. Input secrets are checked against values encrypted by
`@apify/input_secrets` (`tests/fixtures/input_secrets.json`).

Deliberate differences are listed in [`docs/allowed-differences.md`](../docs/allowed-differences.md).

## Regenerating the golden files

```sh
git clone https://github.com/apify/apify-sdk-js ../apify-sdk-js
(cd ../apify-sdk-js && corepack enable && pnpm install && pnpm compile)
APIFY_SDK_JS_DIR=../apify-sdk-js node --no-warnings conformance/oracle/generate-golden.mts
cargo test
```

CI does the same at the commit pinned in `.github/workflows/ci.yml` and fails when the golden
files change. Node 22 or later is required.
