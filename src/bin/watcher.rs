//! Watcher feeder.
//!
//! The watcher owns the vault filesystem: it discovers `.md` files and pushes
//! their raw content to the server's `POST /ingest`, letting the server chunk
//! and propose. The push goes through the shared [`studybuddy::client::Client`]
//! — the same code path the `sb push` command uses. Still to build: content-hash
//! change detection (push only what changed) and `notify`-based live watching;
//! today it's a one-shot full sweep.
//!
//! Usage: `watcher <vault-dir>` (server from `$STUDYBUDDY_SERVER`, default
//! `http://127.0.0.1:8080`).

use std::path::PathBuf;
use std::process::ExitCode;

use studybuddy::client::Client;
use studybuddy::watcher::discover_notes;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let Some(root) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: watcher <vault-dir>");
        return ExitCode::FAILURE;
    };
    let server =
        std::env::var("STUDYBUDDY_SERVER").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let client = Client::new(server);

    let notes = match discover_notes(&root) {
        Ok(notes) => notes,
        Err(e) => {
            eprintln!("walk failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut pushed = 0usize;
    let mut failed = 0usize;
    for (source_file, content) in notes {
        match client.ingest(&source_file, &content).await {
            Ok(counts) => {
                pushed += 1;
                println!(
                    "{source_file}: {} chunk(s), {} proposed",
                    counts.chunks, counts.proposed_cards
                );
            }
            Err(e) => {
                failed += 1;
                eprintln!("{source_file}: push failed: {e}");
            }
        }
    }

    println!("pushed {pushed} file(s), {failed} failed");
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
