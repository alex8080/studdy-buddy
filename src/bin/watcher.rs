//! Watcher feeder skeleton.
//!
//! The watcher owns the vault filesystem: it discovers `.md` files and pushes
//! their content to the server's `POST /ingest`. This is a skeleton — it walks
//! a directory and reports what it found. Still to build: the HTTP push of
//! `{ source_file, content }` per file (letting the server chunk), change
//! detection via content hash, and `notify`-based live watching.
//!
//! Usage: `watcher <vault-dir>`

use std::path::PathBuf;
use std::process::ExitCode;

use studybuddy::ingest::ChunkConfig;
use studybuddy::watcher::ingest_directory;

fn main() -> ExitCode {
    let Some(root) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: watcher <vault-dir>");
        return ExitCode::FAILURE;
    };

    match ingest_directory(&root, &ChunkConfig::default()) {
        Ok(out) => {
            // TODO: rather than walk+chunk here, the real feeder reads each
            // `.md` file and POSTs `{ source_file, content }` to `/ingest`,
            // letting the server chunk — plus content-hash change detection and
            // `notify`-based live watching.
            println!(
                "scanned {} file(s), {} excluded, {} chunk(s) — would push to /ingest",
                out.files_scanned,
                out.excluded_files,
                out.chunks.len(),
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("walk failed: {e}");
            ExitCode::FAILURE
        }
    }
}
