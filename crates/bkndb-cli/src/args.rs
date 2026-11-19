use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "bkndb",
    version,
    about = "Offline BknDb administration (validation candidate)"
)]
pub struct Cli {
    #[arg(long, global = true, value_enum, default_value = "table")]
    pub format: Format,
    /// Permit mutations through query, shell, import and KV commands.
    #[arg(long, global = true)]
    pub write: bool,
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq)]
pub enum Format {
    Table,
    Json,
    Jsonl,
    Csv,
}
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Redb,
    Lsm,
}
#[derive(Args, Debug)]
pub struct Source {
    pub file: PathBuf,
    /// Assert the detected backend; never reinterpret a different format.
    #[arg(long, value_enum)]
    pub backend: Option<Backend>,
}
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a new database; never overwrite an existing destination.
    Create {
        file: PathBuf,
        #[arg(long, value_enum, default_value = "redb")]
        backend: Backend,
    },
    /// Inspect backend, format and recovery state without changing source data.
    Info(Source),
    /// Show storage and logical database statistics.
    Stats(Source),
    /// Check physical integrity, graph references and relational records offline.
    Check(Source),
    /// Check a database and suggest recovery steps without repairing it.
    Doctor(Source),
    /// Compact the original database in place; close other users first.
    Compact(Source),
    /// Write and verify a consistent backup at a new destination.
    Backup {
        #[command(flatten)]
        source: Source,
        #[arg(long)]
        output: PathBuf,
    },
    /// Restore a database backup into a verified new destination.
    Restore {
        #[command(flatten)]
        source: Source,
        #[arg(long)]
        output: PathBuf,
    },
    /// Convert backend or upgrade an LSM format into a new database.
    Migrate {
        #[command(flatten)]
        source: Source,
        #[arg(long, value_enum)]
        to: Backend,
        #[arg(long)]
        output: PathBuf,
    },
    /// Salvage recoverable LSM data into a new file; report discarded bytes.
    Repair {
        #[command(flatten)]
        source: Source,
        #[arg(long)]
        output: PathBuf,
    },
    /// Describe backend capabilities and supported formats.
    Capabilities { file: Option<PathBuf> },
    /// Show candidate version and database/dump format versions.
    Version,
    /// Execute SQL or an atomic script; mutations require --write.
    Sql {
        #[command(flatten)]
        source: Source,
        #[arg(long, required_unless_present = "script", conflicts_with = "script")]
        query: Option<String>,
        #[arg(long = "file", required_unless_present = "query")]
        script: Option<PathBuf>,
        #[arg(long, default_value = "[]")]
        params: String,
        /// Commit each statement separately; earlier commits survive later errors.
        #[arg(long)]
        per_statement: bool,
    },
    /// Open an interactive SQL shell; read-only unless --write is supplied.
    Shell {
        #[command(flatten)]
        source: Source,
        /// Opt-in history file; may contain sensitive queries.
        #[arg(long)]
        history: Option<PathBuf>,
    },
    /// List registered relational tables.
    Tables(Source),
    /// Show registered table schemas and constraints.
    Schema {
        #[command(flatten)]
        source: Source,
        table: Option<String>,
    },
    /// List secondary, full-text, vector and graph property indexes.
    Indexes {
        #[command(flatten)]
        source: Source,
        table: Option<String>,
    },
    /// Query or modify graph nodes, edges and neighbors.
    Graph {
        #[command(subcommand)]
        command: Graph,
    },
    /// Run full-text or vector search.
    Search {
        #[command(subcommand)]
        command: Search,
    },
    /// Create or drop an index; requires --write.
    Index {
        #[command(subcommand)]
        command: Index,
    },
    /// Export a versioned full dump, or one table as typed CSV.
    Export {
        #[command(flatten)]
        source: Source,
        #[arg(long)]
        output: PathBuf,
        /// Export a single table as CSV instead of a lossless BknDb dump.
        #[arg(long)]
        table: Option<String>,
    },
    /// Import a dump into a new database, or CSV into an existing table.
    Import {
        file: PathBuf,
        #[arg(long)]
        input: PathBuf,
        #[arg(long, value_enum, default_value = "redb")]
        backend: Backend,
        /// Import CSV into this existing table, according to its schema.
        #[arg(long)]
        table: Option<String>,
    },
    /// Inspect validated LSM structures without exposing raw values.
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },
    /// Access isolated application KV namespaces; never internal tables.
    Kv {
        #[command(subcommand)]
        command: Kv,
    },
    /// Benchmark a new dedicated database; never use an existing database.
    Bench(Workload),
    /// Exercise a new database against a deterministic reference model.
    Stress(Workload),
    /// Generate shell completion definitions.
    Completion {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}
#[derive(Subcommand, Debug)]
pub enum Graph {
    Query {
        #[command(flatten)]
        source: Source,
        query: String,
        #[arg(long, default_value = "[]")]
        params: String,
    },
    Node {
        #[command(subcommand)]
        command: Node,
    },
    Edge {
        #[command(subcommand)]
        command: Edge,
    },
    Neighbors {
        #[command(flatten)]
        source: Source,
        id: u64,
        #[arg(long,value_parser=["out","in"],default_value="out")]
        direction: String,
    },
}
#[derive(Subcommand, Debug)]
pub enum Node {
    Add {
        #[command(flatten)]
        source: Source,
        label: String,
        #[arg(long, default_value = "{}")]
        properties: String,
    },
    Get {
        #[command(flatten)]
        source: Source,
        id: u64,
    },
    Update {
        #[command(flatten)]
        source: Source,
        id: u64,
        #[arg(long)]
        properties: String,
    },
    Delete {
        #[command(flatten)]
        source: Source,
        id: u64,
    },
    List {
        #[command(flatten)]
        source: Source,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
}
#[derive(Subcommand, Debug)]
pub enum Edge {
    Add {
        #[command(flatten)]
        source: Source,
        from: u64,
        edge_type: String,
        to: u64,
        #[arg(long, default_value = "{}")]
        properties: String,
    },
    Get {
        #[command(flatten)]
        source: Source,
        id: u64,
    },
    Update {
        #[command(flatten)]
        source: Source,
        id: u64,
        #[arg(long)]
        properties: String,
    },
    Delete {
        #[command(flatten)]
        source: Source,
        id: u64,
    },
    List {
        #[command(flatten)]
        source: Source,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
}
#[derive(Subcommand, Debug)]
pub enum Search {
    Text {
        #[command(flatten)]
        source: Source,
        table: String,
        column: String,
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    Vector {
        #[command(flatten)]
        source: Source,
        table: String,
        column: String,
        vector: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long, value_enum, default_value = "cosine")]
        metric: Metric,
        #[arg(long)]
        exact: bool,
    },
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Metric {
    Cosine,
    Dot,
    Euclidean,
}
impl Metric {
    pub fn engine(self) -> bkndb::relational::VectorMetric {
        use bkndb::relational::VectorMetric as V;
        match self {
            Self::Cosine => V::Cosine,
            Self::Dot => V::Dot,
            Self::Euclidean => V::Euclidean,
        }
    }
}
#[derive(Args, Debug)]
pub struct IndexSpec {
    #[command(flatten)]
    pub source: Source,
    #[arg(value_enum)]
    pub kind: IndexKind,
    /// Table name, or graph label for a property index.
    pub table: String,
    pub column: String,
    #[arg(long, value_enum, default_value = "cosine")]
    pub metric: Metric,
    #[arg(long,default_value_t=16,value_parser=clap::value_parser!(u32).range(2..=16384))]
    pub m: u32,
    #[arg(long,default_value_t=200,value_parser=clap::value_parser!(u32).range(1..=16384))]
    pub ef_construction: u32,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum IndexKind {
    Secondary,
    Text,
    Vector,
    Property,
}
#[derive(Subcommand, Debug)]
pub enum Index {
    Create(IndexSpec),
    Drop(IndexSpec),
}
#[derive(Subcommand, Debug)]
pub enum DebugCommand {
    Header(Source),
    Manifest(Source),
    Wal(Source),
    Sstable(Source),
}
#[derive(Args, Debug)]
pub struct KvSource {
    #[command(flatten)]
    pub source: Source,
    /// Application namespace only: [a-zA-Z0-9_-], isolated from database internals.
    pub namespace: String,
}
#[derive(Subcommand, Debug)]
pub enum Kv {
    Get {
        #[command(flatten)]
        source: KvSource,
        key: String,
    },
    Put {
        #[command(flatten)]
        source: KvSource,
        key: String,
        value: String,
    },
    Delete {
        #[command(flatten)]
        source: KvSource,
        key: String,
    },
    Scan {
        #[command(flatten)]
        source: KvSource,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
}
#[derive(Args, Debug)]
pub struct Workload {
    /// Must not exist; synthetic workload only.
    pub file: PathBuf,
    #[arg(long, value_enum, default_value = "lsm")]
    pub backend: Backend,
    #[arg(long,default_value_t=10000,value_parser=clap::value_parser!(u64).range(1..=1000000000))]
    pub operations: u64,
    #[arg(long, default_value_t = 29)]
    pub seed: u64,
}
