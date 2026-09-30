# crawlee-rs Actor template

An [Apify Actor](https://docs.apify.com/platform/actors) in Rust: it crawls the start URLs of its
input with a crawlee-rs `HtmlCrawler` and saves the URL and title of every page to the default
dataset.

## Run locally

```bash
cargo run
```

The input is read from `INPUT.json`; fields it leaves out get the defaults of
`.actor/input_schema.json`. Results go to `storage/datasets/default/`.

## Run on Apify

Copy this directory, then with the [Apify CLI](https://docs.apify.com/cli):

```bash
apify login
apify push
```

The platform builds the `Dockerfile` and runs the Actor with its storages, input and events.
