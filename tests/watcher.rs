//! End-to-end acceptance tests for the watcher's directory walking.
//!
//! These run against realistic markdown fixtures and exercise the full
//! discovery + ingest pipeline through `watcher::ingest_directory`. Parser
//! edge cases (tag rules, Obsidian syntax, size-aware chunking) live as unit
//! tests in `src/ingest.rs`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use studybuddy::ingest::ChunkConfig;
use studybuddy::llm::ChunkContext;
use studybuddy::watcher;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ingest")
}

fn source_files(chunks: &[ChunkContext]) -> HashSet<String> {
    chunks
        .iter()
        .map(|c| c.source_file.replace(std::path::MAIN_SEPARATOR, "/"))
        .collect()
}

fn ends_with(set: &HashSet<String>, suffix: &str) -> bool {
    set.iter().any(|s| s.ends_with(suffix))
}

fn contains_substring(set: &HashSet<String>, needle: &str) -> bool {
    set.iter().any(|s| s.contains(needle))
}

// ---- discovery ----

#[test]
fn discovers_markdown_recursively() {
    let chunks = watcher::ingest_directory(&fixtures_dir().join("nested"), &ChunkConfig::default())
        .expect("ingest_directory")
        .chunks;
    let files = source_files(&chunks);
    assert!(ends_with(&files, "top.md"), "top.md not found in {files:?}");
    assert!(
        ends_with(&files, "sub/inner.md"),
        "sub/inner.md not found in {files:?}"
    );
}

#[test]
fn ignores_non_markdown_files() {
    let chunks = watcher::ingest_directory(
        &fixtures_dir().join("mixed_extensions"),
        &ChunkConfig::default(),
    )
    .expect("ingest_directory")
    .chunks;
    let files = source_files(&chunks);
    for f in &files {
        assert!(f.ends_with(".md"), "non-md file ingested: {f}");
    }
    assert!(ends_with(&files, "note.md"));
}

#[test]
fn ignores_hidden_directories() {
    let chunks =
        watcher::ingest_directory(&fixtures_dir().join("hidden_dirs"), &ChunkConfig::default())
            .expect("ingest_directory")
            .chunks;
    let files = source_files(&chunks);
    assert!(
        !contains_substring(&files, ".git/"),
        ".git contents ingested: {files:?}"
    );
    assert!(
        !contains_substring(&files, ".obsidian/"),
        ".obsidian contents ingested: {files:?}"
    );
    assert!(
        ends_with(&files, "normal.md"),
        "expected normal.md, got {files:?}"
    );
}

#[test]
fn empty_directory_yields_no_chunks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let chunks = watcher::ingest_directory(dir.path(), &ChunkConfig::default())
        .expect("ingest_directory")
        .chunks;
    assert!(chunks.is_empty(), "expected no chunks, got {chunks:?}");
}

#[test]
fn directory_chunks_use_paths_relative_to_root() {
    let chunks = watcher::ingest_directory(&fixtures_dir().join("nested"), &ChunkConfig::default())
        .expect("ingest_directory")
        .chunks;
    for c in &chunks {
        let p = Path::new(&c.source_file);
        assert!(
            p.is_relative(),
            "expected relative source_file, got {:?}",
            c.source_file
        );
        assert!(
            !c.source_file.starts_with('/') && !c.source_file.contains(":\\"),
            "source_file looks absolute: {}",
            c.source_file
        );
    }
}

// ---- realistic vault end-to-end ----
//
// The `sample_vault/` fixture mimics a small Obsidian folder:
//   - linear_algebra.md  — frontmatter tags, inline tag, headings, wikilinks,
//                          alias, callout, fenced code with a would-be tag
//   - dot_product.md     — short note referenced via wikilink
//   - ignored.md         — frontmatter `studybuddy.exclude: true`
//
// These tests verify the pipeline composes correctly on a real-shaped input;
// they intentionally don't drill into every edge case (those are unit tests).

fn ingest_sample_vault() -> Vec<ChunkContext> {
    watcher::ingest_directory(
        &fixtures_dir().join("sample_vault"),
        &ChunkConfig::default(),
    )
    .expect("ingest_directory")
    .chunks
}

#[test]
fn sample_vault_ingests_non_excluded_notes_only() {
    let files = source_files(&ingest_sample_vault());
    assert!(
        ends_with(&files, "linear_algebra.md"),
        "linear_algebra.md missing: {files:?}"
    );
    assert!(
        ends_with(&files, "dot_product.md"),
        "dot_product.md missing: {files:?}"
    );
    assert!(
        !ends_with(&files, "ignored.md"),
        "excluded file was ingested: {files:?}"
    );
}

#[test]
fn sample_vault_propagates_frontmatter_and_inline_tags() {
    let chunks = ingest_sample_vault();
    let la: Vec<&ChunkContext> = chunks
        .iter()
        .filter(|c| c.source_file.ends_with("linear_algebra.md"))
        .collect();
    assert!(!la.is_empty(), "no chunks from linear_algebra.md");
    let tags: HashSet<String> = la.iter().flat_map(|c| c.tags.iter().cloned()).collect();
    assert!(tags.contains("math"), "frontmatter tag missing: {tags:?}");
    assert!(
        tags.contains("linear-algebra"),
        "frontmatter tag missing: {tags:?}"
    );
    assert!(tags.contains("algebra"), "inline tag missing: {tags:?}");
    assert!(
        !tags.iter().any(|t| t == "not_a_tag"),
        "code-block content leaked into tags: {tags:?}"
    );
}

#[test]
fn sample_vault_resolves_wikilinks_and_strips_callouts() {
    let chunks = ingest_sample_vault();
    let la_text: String = chunks
        .iter()
        .filter(|c| c.source_file.ends_with("linear_algebra.md"))
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        la_text.contains("vectors"),
        "wikilink target not preserved: {la_text}"
    );
    assert!(
        !la_text.contains("[[vectors]]"),
        "raw wikilink remained: {la_text}"
    );
    assert!(
        la_text.contains("matrix operations"),
        "wikilink alias not used: {la_text}"
    );
    assert!(
        la_text.contains("Tip") && la_text.contains("same dimension"),
        "callout content lost: {la_text}"
    );
    assert!(
        !la_text.contains("[!note]"),
        "callout marker remained: {la_text}"
    );
}

#[test]
fn sample_vault_heading_paths_carry_hierarchy() {
    let chunks = ingest_sample_vault();
    let la_headings: HashSet<String> = chunks
        .iter()
        .filter(|c| c.source_file.ends_with("linear_algebra.md"))
        .filter_map(|c| c.source_heading.clone())
        .collect();
    assert!(
        la_headings
            .iter()
            .any(|h| h.contains("Linear Algebra") && h.contains("Vectors")),
        "expected an ancestor-carrying heading path, got {la_headings:?}"
    );
}
