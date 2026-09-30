# Allowed differences from the JS SDK

Behavior that intentionally differs from the JS SDK (`apify` on npm). Everything else should match.

## Configuration

- **"At home" has one source.** The JS SDK reads `APIFY_IS_AT_HOME` both through `Configuration`
  (coerced like a boolean, so `0` and `false` are false) and directly with `!!process.env[...]` (so
  any non-empty value is true). Here it is always `Configuration::is_at_home`.
- **Invalid numbers are ignored.** A value such as `APIFY_PROXY_PORT=abc` falls back to the default
  with a warning. The JS SDK throws when the configuration is constructed.

## Storages

- **Listed keys have an empty content type.** The API does not report content types when it lists
  the keys of a key-value store. The JS SDK leaves the field undefined; crawlee-rs requires a
  string, so it is empty.

## Actor lifecycle

- **A failing user function keeps the exit options.** When the function given to `main` fails,
  the JS SDK exits with code 91 and drops the other exit options (such as the status message).
  `main_with` keeps them and only sets the exit code.
- **The final status message is awaited.** The JS SDK gives the request that sets the terminal
  status message 1 ms before exiting. Here it gets up to 1 s (the status message timeout), since
  a process exit in Rust cuts off requests in flight.
- **`Actor::current()` returns `Option`.** Before `Actor::init()` there is no Actor; the JS SDK
  warns and continues with an uninitialized one.
