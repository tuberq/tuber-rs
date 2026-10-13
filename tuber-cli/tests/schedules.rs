//! `tuber-cli schedule-sync`, `list-schedules` and `stats-schedule`.

mod common;

use common::{scripted_server, FakeServer};
use std::io::Write;
use std::process::{Command, Output, Stdio};

const SPLAT: &str = r#"[
  {"name": "rollup", "tube": "splat.maintenance", "cron": "5 * * * *",
   "idp": "rollup", "con": "maintenance", "pri": 1024,
   "body": {"class": "HistogramRollupJob", "args": []}},
  {"name": "retention", "tube": "splat.maintenance",
   "cron": "0 2 * * *", "zone": "Australia/Melbourne", "con": "maintenance", "pri": 1024,
   "body": "{\"class\":\"DataRetentionJob\",\"args\":[]}"}
]"#;

/// Run tuber-cli against `server` with `stdin` piped in.
fn run(server: &FakeServer, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tuber-cli"))
        .args(["--addr", &server.addr])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn sync_sends_the_file_as_one_sync() {
    let server = scripted_server(&[b"SYNCING splat\r\n", b"STAGED\r\n", b"STAGED\r\n", b"SYNCED 2 0 0 0\r\n"]);
    let out = run(&server, &["schedule-sync", "splat", "-"], SPLAT);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        server.transcript(),
        [
            "schedule-begin splat",
            "schedule-put rollup splat.maintenance 1024 0 120 40 idp:rollup con:maintenance 5 * * * *",
            r#"{"class":"HistogramRollupJob","args":[]}"#,
            "schedule-put retention splat.maintenance 1024 0 120 38 con:maintenance 0 2 * * * Australia/Melbourne",
            r#"{"class":"DataRetentionJob","args":[]}"#,
            "schedule-commit",
        ]
    );
    assert_eq!(
        json(&out),
        serde_json::json!({ "owner": "splat", "added": 2, "changed": 0, "removed": 0, "unchanged": 0 })
    );
}

/// A typo in one zone must not delete that entry: tuber aborts the sync, the
/// previous schedule keeps running, and the run fails naming the entry.
#[test]
fn a_refused_entry_fails_the_run_and_names_it() {
    let server = scripted_server(&[b"SYNCING splat\r\n", b"STAGED\r\n", b"BAD_SCHEDULE\r\n", b"ABORTED\r\n"]);
    let out = run(&server, &["schedule-sync", "splat", "-"], &SPLAT.replace("Melbourne", "Melborne"));
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("retention") && stderr.contains("BAD_SCHEDULE"), "{stderr}");
    assert!(stderr.contains("nothing changed"), "{stderr}");
    assert_eq!(server.transcript().last().map(String::as_str), Some("schedule-abort"));
}

/// An empty file deletes everything the owner has, so it needs saying twice.
#[test]
fn an_empty_file_needs_allow_empty() {
    let server = scripted_server(&[b"SYNCING splat\r\n", b"SYNCED 0 0 9 0\r\n"]);
    let out = run(&server, &["schedule-sync", "splat", "-"], "[]");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--allow-empty"), "{stderr}");
    // `-` is how stdin is named on the command line, not in a sentence.
    assert!(stderr.contains("stdin has no entries"), "{stderr}");

    let server = scripted_server(&[b"SYNCING splat\r\n", b"SYNCED 0 0 9 0\r\n"]);
    let out = run(&server, &["schedule-sync", "splat", "-", "--allow-empty"], "[]");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(json(&out)["removed"], 9);
    assert_eq!(server.transcript(), ["schedule-begin splat", "schedule-commit"]);
}

/// An unreadable file is not an empty one: a JSON error sends nothing at all.
#[test]
fn a_file_that_does_not_parse_sends_nothing() {
    let server = scripted_server(&[b"SYNCING splat\r\n", b"SYNCED 0 0 9 0\r\n"]);
    let out = run(&server, &["schedule-sync", "splat", "-", "--allow-empty"], "");
    assert_eq!(out.status.code(), Some(1));
    assert!(server.transcript().is_empty(), "sent {:?}", server.transcript());
}

#[test]
fn list_schedules_gives_owner_and_name() {
    let server = scripted_server(&[b"OK 37\r\n---\n- splat rollup\n- splat retention\n\r\n"]);
    let out = run(&server, &["list-schedules", "--owner", "splat"], "");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(server.transcript(), ["list-schedules splat"]);
    assert_eq!(
        json(&out),
        serde_json::json!([
            { "owner": "splat", "name": "rollup" },
            { "owner": "splat", "name": "retention" },
        ])
    );
}

#[test]
fn stats_schedule_parses_values_with_colons() {
    const YAML: &str = "---\nowner: \"splat\"\nname: \"rollup\"\nspec: \"5 * * * *\"\n\
        tags: \"idp:rollup con:maintenance\"\nnext-fire: 2026-10-11T21:05:00+00:00\n\
        last-fire: \"\"\nlast-result: \"INSERTED 47101 BURIED\"\nburied-streak: 3\n";
    let reply: &'static [u8] = format!("OK {}\r\n{YAML}\r\n", YAML.len()).leak().as_bytes();
    let server = scripted_server(&[reply]);
    let out = run(&server, &["stats-schedule", "splat", "rollup"], "");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(server.transcript(), ["stats-schedule splat rollup"]);
    let v = json(&out);
    assert_eq!(v["tags"], "idp:rollup con:maintenance");
    assert_eq!(v["next_fire"], "2026-10-11T21:05:00+00:00");
    assert_eq!(v["last_fire"], "");
    assert_eq!(v["buried_streak"], 3);
}
