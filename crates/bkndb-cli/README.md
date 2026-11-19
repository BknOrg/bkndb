# BknDb CLI (validation candidate)

`bkndb-cli` is a separate workspace crate producing the `bkndb` executable. It
uses the native Rust libraries directly; there is no daemon or runtime BknDb
shared library. It is **not published** and **not declared stable**. Redb remains
the creation default. LSM stability gates still apply to this CLI.

Build from the complete workspace with the pinned CI Rust toolchain (1.99.1):

```sh
cargo build --release --locked -p bkndb-cli
./target/release/bkndb --help
```

Or install from the checkout with `cargo install --locked --path crates/bkndb-cli`.
Do not use `cargo install bkndb-cli` yet: registry publication, dependency release
ordering and a new unused release version remain separate release work.

```sh
bkndb create app.bkndb                         # Redb default
bkndb create experiment.bkndb --backend lsm
bkndb sql app.bkndb --write --query 'CREATE TABLE notes (id INT PRIMARY KEY, text TEXT)'
bkndb sql app.bkndb --write --query "INSERT INTO notes VALUES (1, 'hello')"
bkndb sql app.bkndb --query 'SELECT * FROM notes' --format json
bkndb check app.bkndb --format json
bkndb backup app.bkndb --output app.backup.bkndb
bkndb shell app.bkndb                          # read-only
bkndb shell app.bkndb --write                  # explicit transactions available
```

Close applications using the database before administration. Inspection never
repairs the source. Output destinations must not exist, and their parent must
already exist. Do not delete a database lock sidecar to bypass a lock error.

See [the complete guide](../../docs/CLI.md) for all commands, encoding, exit
codes, safety contracts and acceptance gates.
