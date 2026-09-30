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
- **`abort` sets the status message on the aborted run.** The JS SDK sets it on the current run.
- **`get_env` falls back to the `APIFY_*` variables.** The JS SDK reads both generations of
  variables into one object, and a missing `ACTOR_*` variable overwrites its `APIFY_*` predecessor
  with `null`. Here the `ACTOR_*` variable is read first and the `APIFY_*` one is the fallback.
- **`use_state` uses the Actor's services.** The JS SDK opens the store with the global
  configuration, whatever the Actor was configured with.
- **Shared request queues can prolong locks.** `extend_request_processing_time` prolongs the
  platform lock of a request in progress; the JS SDK never prolongs locks.

## Input

- **Encrypted PKCS#8 keys are not supported for input secrets.** The platform's key is a legacy
  encrypted PKCS#1 PEM (DES-EDE3-CBC), which is supported, as are AES-CBC encrypted and
  unencrypted keys. Node.js also reads `BEGIN ENCRYPTED PRIVATE KEY` PEMs.
- **Pseudo-URLs escape characters above U+00FF correctly.** `purlToRegExp` writes them as
  `\xHHHH`, which a JavaScript regex reads as `\xHH` followed by two literal characters, so such
  pseudo-URLs never match. Here they match the character.
- **Pseudo-URL `[regex]` sections use the syntax of the `regex` crate.** Lookarounds and
  backreferences of JavaScript regexes are not supported.
