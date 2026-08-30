// SPDX-License-Identifier: MIT

use clap::{Parser, Subcommand};
use vento_file_parser::{ConvertOptions, DocumentInput, FileParser};

#[derive(Debug, Parser)]
#[command(name = "vento-runtime")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Convert { input: std::path::PathBuf },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Convert { input } => {
            let root = std::env::current_dir()?.canonicalize()?;
            let result = FileParser::new()
                .with_allowed_roots(vec![root])
                .convert(
                    DocumentInput::Path { path: input },
                    ConvertOptions::default(),
                )
                .await?;
            println!("{}", result.markdown);
        }
    }
    Ok(())
}
