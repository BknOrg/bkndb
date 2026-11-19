#![cfg(feature = "test-failpoints")]
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bkndb"))
}
fn ok(args: &[&str]) {
    let r = cli().args(args).output().unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
}
fn kill_at(args: &[&str], name: &str, marker: &Path) {
    let mut child = cli()
        .args(args)
        .env("BKNDB_CLI_FAILPOINT", name)
        .env("BKNDB_CLI_MARKER", marker)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !marker.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("child exited before failpoint: {status}");
        }
        if start.elapsed() > Duration::from_secs(30) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("failpoint timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    std::fs::remove_file(marker).unwrap();
}
#[test]
fn killed_publication_never_overwrites_or_exposes_half_file() {
    let d = tempfile::tempdir().unwrap();
    for backend in ["redb", "lsm"] {
        for point in ["before_publish", "after_publish"] {
            let source = d.path().join(format!("{backend}-{point}"));
            let output = source.with_extension("backup");
            let marker = source.with_extension("marker");
            ok(&["create", source.to_str().unwrap(), "--backend", backend]);
            let bytes = std::fs::read(&source).unwrap();
            kill_at(
                &[
                    "backup",
                    source.to_str().unwrap(),
                    "--output",
                    output.to_str().unwrap(),
                ],
                point,
                &marker,
            );
            assert_eq!(std::fs::read(&source).unwrap(), bytes);
            if point == "before_publish" {
                assert!(!output.exists())
            } else {
                assert!(output.exists());
                ok(&["check", output.to_str().unwrap()]);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    assert_eq!(std::fs::metadata(&output).unwrap().nlink(), 1);
                }
            }
        }
    }
}
#[test]
fn killed_commit_recovers_whole_transaction() {
    let d = tempfile::tempdir().unwrap();
    for backend in ["redb", "lsm"] {
        for point in ["before_commit", "after_commit"] {
            let p = d.path().join(format!("{backend}-{point}"));
            let marker = p.with_extension("marker");
            let s = p.to_str().unwrap();
            ok(&["create", s, "--backend", backend]);
            ok(&[
                "sql",
                s,
                "--write",
                "--query",
                "CREATE TABLE t (id INT PRIMARY KEY)",
            ]);
            kill_at(
                &[
                    "sql",
                    s,
                    "--write",
                    "--query",
                    "INSERT INTO t VALUES (1); INSERT INTO t VALUES (2);",
                ],
                point,
                &marker,
            );
            // A killed Redb writer may set its recovery marker. Explicit writable open
            // is authorized here; read-only CLI inspection never repairs the source.
            if backend == "redb" {
                drop(bkndb::RedbStorageBackend::open_existing(&p).unwrap());
            }
            let r = cli()
                .args([
                    "sql",
                    s,
                    "--query",
                    "SELECT COUNT(*) FROM t",
                    "--format",
                    "json",
                ])
                .output()
                .unwrap();
            assert!(r.status.success());
            let v: serde_json::Value = serde_json::from_slice(&r.stdout).unwrap();
            assert_eq!(
                v["data"]["rows"][0][0]["Int"],
                if point == "before_commit" { 0 } else { 2 }
            );
        }
    }
}
