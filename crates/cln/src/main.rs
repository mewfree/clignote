use clap::Parser;

/// clignote — a terminal org-mode editor
#[derive(Parser)]
#[command(name = "cln", version, about)]
struct Cli {
    /// File to open
    file: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    clignote_tui::run(cli.file.as_deref())
}
