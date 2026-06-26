//! `sb` — the StudyBuddy command-line client.
//!
//! A thin clap shell over [`studybuddy::cli`]: it parses arguments, builds a
//! [`Client`], wires real stdin/stdout (and `$EDITOR` for curation edits), and
//! delegates. All command logic and its tests live in the lib.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use studybuddy::cli;
use studybuddy::client::Client;
use studybuddy::model::CardContent;

#[derive(Parser)]
#[command(
    name = "sb",
    about = "StudyBuddy CLI: push notes, curate cards, run reviews"
)]
struct Cli {
    /// Base URL of the StudyBuddy server.
    #[arg(
        long,
        global = true,
        env = "STUDYBUDDY_SERVER",
        default_value = "http://127.0.0.1:8080"
    )]
    server: String,

    /// Bearer token for server authentication.
    #[arg(long, global = true, env = "STUDYBUDDY_API_TOKEN")]
    api_token: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Push one note's markdown to the server for card generation.
    Push {
        /// Vault root the note's anchor is computed relative to.
        #[arg(long, default_value = ".")]
        vault: PathBuf,
        /// The note file to push (must live under the vault).
        file: PathBuf,
    },
    /// Run a spaced-repetition session over the currently-due cards.
    Review,
    /// Walk the pending cards: accept, reject, or edit each.
    Curate,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let client = match &cli.api_token {
        Some(t) => {
            studybuddy::client::validate_api_token(t)?;
            Client::authenticated(&cli.server, t)
        }
        None => Client::new(&cli.server),
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    match cli.command {
        Command::Push { vault, file } => cli::run_push(&client, &vault, &file, &mut out).await,
        Command::Review => {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            cli::run_review(&client, &mut input, &mut out).await
        }
        Command::Curate => {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            cli::run_curate(&client, &mut input, &mut out, edit_in_editor).await
        }
    }
}

/// Open a card's content as pretty JSON in `$EDITOR` (falling back to `vi`),
/// then parse the saved result back into a [`CardContent`].
fn edit_in_editor(content: &CardContent) -> Result<CardContent> {
    let json = serde_json::to_string_pretty(content)?;
    let path = std::env::temp_dir().join(format!("sb-card-{}.json", std::process::id()));
    std::fs::write(&path, &json).with_context(|| format!("writing {}", path.display()))?;

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let status = std::process::Command::new(program)
        .args(parts)
        .arg(&path)
        .status()
        .with_context(|| format!("launching editor '{editor}'"))?;
    if !status.success() {
        bail!("editor '{editor}' exited with failure");
    }

    let edited =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let _ = std::fs::remove_file(&path);
    serde_json::from_str(&edited).context("parsing edited card JSON")
}
