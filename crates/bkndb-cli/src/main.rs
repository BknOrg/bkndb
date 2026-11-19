use clap::Parser;
fn main() {
    let arguments: Vec<_> = std::env::args_os().collect();
    let json = arguments
        .iter()
        .any(|a| a == "--format=json" || a == "--format=jsonl")
        || arguments
            .windows(2)
            .any(|a| a[0] == "--format" && (a[1] == "json" || a[1] == "jsonl"));
    let cli = match bkndb_cli::args::Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(e)
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            e.exit()
        }
        Err(e) => {
            bkndb_cli::report_error(
                if json {
                    bkndb_cli::args::Format::Json
                } else {
                    bkndb_cli::args::Format::Table
                },
                &bkndb_cli::Error::new(2, e.to_string()),
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = ctrlc::set_handler(bkndb_cli::cancel) {
        eprintln!("cannot install interrupt handler: {e}");
        std::process::exit(1);
    }
    let format = cli.format;
    match bkndb_cli::run(cli) {
        Ok(()) => (),
        Err(e) => {
            bkndb_cli::report_error(format, &e);
            std::process::exit(e.code);
        }
    }
}
