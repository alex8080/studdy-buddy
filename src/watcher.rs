//! Filesystem walking for the watcher feeder.
//!
//! In the push model the server no longer reads the filesystem: a separate
//! watcher app discovers `.md` files under a vault and pushes their content to
//! `POST /ingest`. This module owns that filesystem-facing walking. The
//! chunking it currently performs (via [`crate::ingest::ingest_text`]) is a
//! carryover from the pull model — the real watcher will push raw content and
//! let the server chunk. See `src/bin/watcher.rs` for the feeder skeleton.

use std::path::Path;

use crate::error::Result;
use crate::ingest::{self, ChunkConfig};
use crate::llm::ChunkContext;

pub struct IngestOutput {
    pub chunks: Vec<ChunkContext>,
    pub files_scanned: usize,
    pub excluded_files: usize,
}

/// Walk `root` recursively, ingest every `.md` file, return chunks with
/// `source_file` paths relative to `root`. Hidden directories (anything
/// whose name starts with `.`) are skipped. Files whose frontmatter sets
/// `studybuddy.exclude: true` are counted in `excluded_files` and contribute
/// no chunks.
pub fn ingest_directory(root: &Path, config: &ChunkConfig) -> Result<IngestOutput> {
    let mut chunks_out = vec![];
    let mut files_scanned = 0usize;
    let mut excluded_files = 0usize;
    let mut queue = vec![root.to_path_buf()];
    while let Some(dir) = queue.pop() {
        for entry in dir.read_dir()? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let ft = entry.file_type()?;
            if ft.is_dir() {
                queue.push(entry.path());
                continue;
            }
            if !ft.is_file() || !name.ends_with(".md") {
                continue;
            }
            files_scanned += 1;
            let abs = entry.path();
            let Ok(content) = std::fs::read_to_string(&abs) else {
                continue;
            };
            let (fm_yaml, _body) = ingest::split_frontmatter(&content);
            let Ok((excluded, _fm_tags)) = ingest::parse_frontmatter(fm_yaml) else {
                continue;
            };
            if excluded {
                excluded_files += 1;
                continue;
            }
            let rel = abs
                .strip_prefix(root)
                .unwrap_or(&abs)
                .to_string_lossy()
                .into_owned();
            let Ok(chunks) = ingest::ingest_text(&content, &rel, config) else {
                continue;
            };
            chunks_out.extend(chunks);
        }
    }
    Ok(IngestOutput {
        chunks: chunks_out,
        files_scanned,
        excluded_files,
    })
}

/// Read and parse a single markdown file. Owns only the I/O; parsing is
/// delegated to [`crate::ingest::ingest_text`].
pub fn ingest_file(path: &Path, config: &ChunkConfig) -> Result<Vec<ChunkContext>> {
    let content = std::fs::read_to_string(path)?;
    ingest::ingest_text(&content, &path.to_string_lossy(), config)
}
