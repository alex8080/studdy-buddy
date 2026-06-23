//! File-backed [`Repository`] — one store per server, rooted at a data dir.
//!
//! Layout directly under `root` (the configured data dir):
//! - `cards/<sha256(source_file)>.json` — one sidecar per note, a JSON array of
//!   [`Card`]. The filename is a hash of the note's vault-relative path, which
//!   is flat and traversal-proof (the path arrives as untrusted HTTP input); the
//!   readable path lives inside each card as `source_file`.
//! - `state.json` — `card_id → { state, next_due }`; the due-index.
//! - `reviews.jsonl` — append-only review log, one JSON [`Review`] per line.
//!
//! A coarse process-level lock serializes the read-modify-write sequences;
//! `state.json` and sidecars are written via temp-file + atomic rename, so a
//! crash mid-write can't corrupt them. I/O is synchronous `std::fs` (fine for a
//! single-user local server); moving to `spawn_blocking`/`tokio::fs` is the
//! upgrade path if it ever blocks the runtime under load.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppError, Result};
use crate::model::{Card, CardContent, CardId, CardStatus, Review};
use crate::scheduler::SchedulerState;

use super::Repository;

/// Extension for the per-note card sidecars; the writer and the directory walk
/// share it so they stay aligned.
const SIDECAR_EXT: &str = "json";

/// File-backed store rooted at `root`, the configured server data dir.
pub struct FileRepository {
    root: PathBuf,
    /// Serializes read-modify-write sequences across concurrent requests.
    lock: Mutex<()>,
}

/// One `state.json` entry: SRS state plus the due date that makes it the
/// due-index.
#[derive(Serialize, Deserialize)]
struct StateEntry {
    state: SchedulerState,
    next_due: DateTime<Utc>,
}

impl FileRepository {
    /// `root` is the configured data dir; the store's files live directly under it.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            lock: Mutex::new(()),
        }
    }

    fn cards_dir(&self) -> PathBuf {
        self.root.join("cards")
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    fn reviews_path(&self) -> PathBuf {
        self.root.join("reviews.jsonl")
    }

    /// Note path → `<cards_dir>/<sha256(source_file)>.json`.
    fn sidecar_path(&self, source_file: &str) -> PathBuf {
        self.cards_dir()
            .join(format!("{}.{SIDECAR_EXT}", sidecar_name(source_file)))
    }

    fn read_state(&self) -> Result<HashMap<CardId, StateEntry>> {
        match fs::read(self.state_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(parse_err),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(AppError::Io(e)),
        }
    }

    fn write_state(&self, map: &HashMap<CardId, StateEntry>) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(map).map_err(parse_err)?;
        write_atomic(&self.state_path(), &bytes)
    }

    fn load_all_cards(&self) -> Result<Vec<Card>> {
        let mut cards = Vec::new();
        for path in list_sidecar_paths(&self.cards_dir())? {
            cards.extend(read_cards(&path)?);
        }
        Ok(cards)
    }

    /// Find the card by id across the sidecars, apply `f` to it, and persist the
    /// owning sidecar atomically. Returns `NotFound` if no sidecar holds the card,
    /// or whatever error `f` returns (without writing).
    fn modify_card(&self, card: CardId, f: impl FnOnce(&mut Card) -> Result<()>) -> Result<()> {
        let _guard = self.lock.lock().unwrap();
        for path in list_sidecar_paths(&self.cards_dir())? {
            let mut cards = read_cards(&path)?;
            if let Some(c) = cards.iter_mut().find(|c| c.id == card) {
                f(c)?;
                let bytes = serde_json::to_vec_pretty(&cards).map_err(parse_err)?;
                write_atomic(&path, &bytes)?;
                return Ok(());
            }
        }
        Err(AppError::NotFound)
    }
}

#[async_trait]
impl Repository for FileRepository {
    async fn save_pending(&self, cards: &[Card]) -> Result<()> {
        let _guard = self.lock.lock().unwrap();
        // Group by note so each sidecar is read-modified-written once.
        let mut by_file: HashMap<&str, Vec<&Card>> = HashMap::new();
        for card in cards {
            by_file
                .entry(card.source_file.as_str())
                .or_default()
                .push(card);
        }
        for (source_file, incoming) in by_file {
            let path = self.sidecar_path(source_file);
            let mut existing = read_cards(&path)?;
            for card in incoming {
                // Upsert by id to match the in-memory backend's insert-by-id. In
                // practice proposals carry fresh UUIDs, so re-ingesting a note
                // *appends* new pending cards rather than replacing — deduping
                // stale re-ingests is the watcher's reconciliation job, not here.
                match existing.iter_mut().find(|c| c.id == card.id) {
                    Some(slot) => *slot = card.clone(),
                    None => existing.push(card.clone()),
                }
            }
            let bytes = serde_json::to_vec_pretty(&existing).map_err(parse_err)?;
            write_atomic(&path, &bytes)?;
        }
        Ok(())
    }

    async fn list_pending(&self) -> Result<Vec<Card>> {
        let _guard = self.lock.lock().unwrap();
        Ok(self
            .load_all_cards()?
            .into_iter()
            .filter(|c| c.status == CardStatus::Pending)
            .collect())
    }

    async fn update_content(&self, card: CardId, content: CardContent) -> Result<()> {
        self.modify_card(card, |c| {
            if c.status != CardStatus::Pending {
                return Err(super::not_pending_err(card));
            }
            c.content = content;
            Ok(())
        })
    }

    async fn set_status(&self, card: CardId, status: CardStatus) -> Result<()> {
        self.modify_card(card, |c| {
            c.status = status;
            Ok(())
        })
    }

    async fn list_due(&self, now: DateTime<Utc>) -> Result<Vec<Card>> {
        let _guard = self.lock.lock().unwrap();
        let due: HashSet<CardId> = self
            .read_state()?
            .into_iter()
            .filter(|(_, e)| e.next_due <= now)
            .map(|(id, _)| id)
            .collect();
        Ok(self
            .load_all_cards()?
            .into_iter()
            // Only accepted cards are due: a card rejected after acceptance may
            // still have a lingering state entry, but it isn't in the SRS pool.
            .filter(|c| due.contains(&c.id) && c.status == CardStatus::Accepted)
            .collect())
    }

    async fn load_state(&self, card: CardId) -> Result<SchedulerState> {
        let _guard = self.lock.lock().unwrap();
        self.read_state()?
            .get(&card)
            .map(|e| e.state)
            .ok_or(AppError::NotFound)
    }

    async fn save_state(
        &self,
        card: CardId,
        state: SchedulerState,
        next_due: DateTime<Utc>,
    ) -> Result<()> {
        let _guard = self.lock.lock().unwrap();
        let mut map = self.read_state()?;
        map.insert(card, StateEntry { state, next_due });
        self.write_state(&map)
    }

    async fn save_review(&self, review: &Review) -> Result<()> {
        let _guard = self.lock.lock().unwrap();
        fs::create_dir_all(&self.root)?;
        let line = serde_json::to_string(review).map_err(parse_err)?;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.reviews_path())?;
        writeln!(f, "{line}")?;
        Ok(())
    }
}

fn parse_err(e: serde_json::Error) -> AppError {
    AppError::Parse(e.to_string())
}

/// Hash a note's vault-relative path into a flat, traversal-proof sidecar stem.
fn sidecar_name(source_file: &str) -> String {
    Sha256::digest(source_file.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Read a sidecar's cards; a missing file is an empty list.
fn read_cards(path: &Path) -> Result<Vec<Card>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(parse_err),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(AppError::Io(e)),
    }
}

/// Write `bytes` to `path` via a temp file + atomic rename, creating parents.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp: OsString = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Recursively collect every `*.json` sidecar under `dir`; missing dir → empty.
fn list_sidecar_paths(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(dir, &mut out)?;
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(AppError::Io(e)),
    };
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some(SIDECAR_EXT) {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_path_is_flat_hashed_and_deterministic() {
        let repo = FileRepository::new("/data");
        let p = repo.sidecar_path("topic/sub/note.md");

        // Flat under cards/, a 64-char hex stem, .json extension.
        assert_eq!(p.parent().unwrap(), PathBuf::from("/data/cards"));
        assert_eq!(p.extension().unwrap(), "json");
        let stem = p.file_stem().unwrap().to_str().unwrap();
        assert_eq!(stem.len(), 64);
        assert!(stem.chars().all(|c| c.is_ascii_hexdigit()));

        // Same path → same name; different path → different name.
        assert_eq!(repo.sidecar_path("topic/sub/note.md"), p);
        assert_ne!(repo.sidecar_path("other/note.md"), p);
    }
}
