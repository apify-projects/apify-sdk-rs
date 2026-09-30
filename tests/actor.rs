//! The Actor lifecycle, end to end. An Actor owns the process (global services, `exit`), so every
//! scenario runs in a child process: this test binary again, running only [`child`].

mod common;

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use common::{DEFAULT_DATASET, DEFAULT_QUEUE, DEFAULT_STORE, FakeApi, TOKEN};

const SCENARIO: &str = "APIFY_SDK_TEST_SCENARIO";

/// Runs `scenario` of [`child`] in a new process, with `vars` as its whole environment. The wait
/// is off the runtime, which serves the fake API meanwhile.
async fn run_child(scenario: &str, vars: &[(&str, String)], cwd: &Path) -> Output {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["child", "--exact", "--nocapture", "--test-threads=1"]).env_clear().current_dir(cwd);
    command.env(SCENARIO, scenario).env("RUST_BACKTRACE", "1");
    for (name, value) in vars {
        command.env(name, value);
    }
    tokio::task::spawn_blocking(move || command.output().unwrap()).await.unwrap()
}

fn platform_env(api: &FakeApi, events_url: Option<String>) -> Vec<(&'static str, String)> {
    let mut vars = vec![
        ("APIFY_IS_AT_HOME", "1".to_owned()),
        ("APIFY_TOKEN", TOKEN.to_owned()),
        ("APIFY_API_BASE_URL", api.url.clone()),
        ("APIFY_API_PUBLIC_BASE_URL", api.url.clone()),
        ("ACTOR_DEFAULT_DATASET_ID", DEFAULT_DATASET.to_owned()),
        ("ACTOR_DEFAULT_KEY_VALUE_STORE_ID", DEFAULT_STORE.to_owned()),
        ("ACTOR_DEFAULT_REQUEST_QUEUE_ID", DEFAULT_QUEUE.to_owned()),
        ("ACTOR_RUN_ID", "run-1".to_owned()),
        ("ACTOR_MEMORY_MBYTES", "1024".to_owned()),
        ("APIFY_METAMORPH_AFTER_SLEEP_MILLIS", "100".to_owned()),
    ];
    if let Some(url) = events_url {
        vars.push(("ACTOR_EVENTS_WEBSOCKET_URL", url));
    }
    vars
}

fn assert_exit(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn terminal_messages(api: &FakeApi) -> Vec<String> {
    api.run_updates()
        .iter()
        .filter(|update| update["isStatusMessageTerminal"] == true)
        .map(|update| update["statusMessage"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn input_and_output_on_the_platform() {
    let api = FakeApi::start().await;
    api.put_record(DEFAULT_STORE, "INPUT", br#"{"greeting":"hi"}"#, "application/json; charset=utf-8");
    let dir = tempfile::tempdir().unwrap();

    let output = run_child("echo", &platform_env(&api, None), dir.path()).await;
    assert_exit(&output, 0);
    assert_eq!(api.dataset_items(DEFAULT_DATASET), [json!({ "echo": { "greeting": "hi" } })]);
    assert_eq!(terminal_messages(&api), ["Echoed"]);
    assert!(!dir.path().join("storage").exists(), "nothing is stored locally");
}

#[tokio::test]
async fn a_failing_user_function_exits_with_91() {
    let api = FakeApi::start().await;
    let dir = tempfile::tempdir().unwrap();
    let output = run_child("fail", &platform_env(&api, None), dir.path()).await;
    assert_exit(&output, 91);
    assert!(String::from_utf8_lossy(&output.stdout).contains("boom"));
    // The exit options of `main_with` are kept (the JS SDK drops them).
    assert_eq!(terminal_messages(&api), ["Done"]);
}

#[tokio::test]
async fn an_aborted_run_exits_gracefully_after_saving_its_state() {
    let api = FakeApi::start().await;
    let events =
        common::serve_events(Duration::from_millis(500), vec![json!({ "name": "aborting", "data": null })]).await;
    let dir = tempfile::tempdir().unwrap();

    let started = Instant::now();
    let output = run_child("wait", &platform_env(&api, Some(events)), dir.path()).await;
    assert_exit(&output, 0);
    assert!(started.elapsed() < Duration::from_secs(20), "did not wait for the user function");
    let (state, _) = api.record(DEFAULT_STORE, "STATE").expect("state saved on exit");
    assert_eq!(serde_json::from_slice::<Value>(&state).unwrap(), json!({ "count": 1 }));
}

#[tokio::test]
async fn a_migrating_run_saves_its_state_and_reboots() {
    let api = FakeApi::start().await;
    // Once the user function has registered its state.
    let events = common::serve_events(Duration::from_millis(500), vec![
        json!({ "name": "systemInfo", "data": { "cpuCurrentUsage": 10.0, "isCpuOverloaded": false, "memCurrentBytes": 1 } }),
        json!({ "name": "migrating", "data": null }),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();

    let output = run_child("migrate", &platform_env(&api, Some(events)), dir.path()).await;
    assert_exit(&output, 0);
    let calls = api.calls();
    let reboot = calls.iter().position(|call| call == "POST /v2/actor-runs/run-1/reboot").expect("rebooted");
    let saved = calls.iter().position(|call| call == "PUT /v2/key-value-stores/default-store/records/STATE").unwrap();
    assert!(saved < reboot, "the state is saved before the reboot: {calls:?}");
}

#[tokio::test]
async fn a_local_run_reads_its_input_from_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("INPUT.json"), r#"{"greeting":"hello"}"#).unwrap();
    std::fs::create_dir(dir.path().join(".actor")).unwrap();
    let schema = json!({ "input": { "properties": { "maxPages": { "type": "integer", "default": 5 } } } });
    std::fs::write(dir.path().join(".actor/actor.json"), schema.to_string()).unwrap();

    let output = run_child("echo", &[], dir.path()).await;
    assert_exit(&output, 0);
    let item: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("storage/datasets/default/000000001.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(item, json!({ "echo": { "maxPages": 5, "greeting": "hello" } }));

    // A second run finds the input in the store, which purging on start keeps.
    std::fs::remove_file(dir.path().join("INPUT.json")).unwrap();
    std::fs::write(dir.path().join("storage/key_value_stores/default/INPUT.json"), r#"{"greeting":"again"}"#).unwrap();
    assert_exit(&run_child("echo", &[], dir.path()).await, 0);
    let item: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("storage/datasets/default/000000001.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(item["echo"]["greeting"], "again");
    assert!(!dir.path().join("storage/datasets/default/000000002.json").exists(), "the dataset was purged");
}

#[tokio::test]
async fn missing_input_and_late_init_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let output = run_child("echo", &[], dir.path()).await;
    assert_exit(&output, 91);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(
            r#"Input does not exist. Expected the "INPUT" record of the default key-value store or a "INPUT" or "INPUT.json" file in the working directory."#
        ),
        "{stdout}"
    );

    assert_exit(&run_child("late-init", &[], dir.path()).await, 0);
}

/// The scenarios, run by [`run_child`]. Does nothing in a normal test run.
#[test]
fn child() {
    let Ok(scenario) = std::env::var(SCENARIO) else { return };
    tracing_subscriber::fmt().with_writer(std::io::stdout).init();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        match scenario.as_str() {
            "echo" => {
                let exit = apify::ExitOptions::message("Echoed");
                apify::main_with(apify::InitOptions::default(), exit, |actor| async move {
                    let input: Value = actor.get_input().await?;
                    actor.push_data(&json!({ "echo": input })).await?;
                    Ok(())
                })
                .await
            }
            "fail" => {
                let exit = apify::ExitOptions::message("Done");
                apify::main_with(apify::InitOptions::default(), exit, |_| async { anyhow::bail!("boom") }).await
            }
            "wait" | "migrate" => {
                apify::main(|actor| async move {
                    let state = actor.services().auto_saved_value("STATE", || json!({ "count": 0 })).await?;
                    state.lock()["count"] = json!(1);
                    let wait = if scenario == "wait" { 60 } else { 2 };
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    Ok(())
                })
                .await
            }
            "late-init" => {
                crawlee::Services::global();
                let result = apify::Actor::init(apify::InitOptions::default()).await;
                assert!(matches!(result, Err(apify::InitError::ServicesAlreadySet)));
                std::process::exit(0);
            }
            other => panic!("unknown scenario {other}"),
        }
    });
    unreachable!("the Actor exits the process");
}
