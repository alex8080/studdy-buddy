use serde::Deserialize;

use crate::error::{AppError, Result};
use crate::llm::ChunkContext;

#[derive(Debug, Clone, Copy)]
pub struct ChunkConfig {
    pub target_words: usize,
    pub max_words: usize,
    pub min_words: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            target_words: 500,
            max_words: 1500,
            min_words: 50,
        }
    }
}

/// Parse markdown content directly into chunks. `source_file` is recorded
/// verbatim on each chunk; callers are responsible for passing a meaningful
/// path. This is the entry point unit tests should use — no filesystem.
pub fn ingest_text(
    content: &str,
    source_file: &str,
    config: &ChunkConfig,
) -> Result<Vec<ChunkContext>> {
    let (fm_yaml, body) = split_frontmatter(content);
    let (excluded, fm_tags) = parse_frontmatter(fm_yaml)?;
    if excluded {
        return Ok(vec![]);
    }

    let raw = collect_sections(body);
    let sections: Vec<Section> = raw
        .into_iter()
        .map(|r| Section {
            text: obsidian_transform(&strip_callout_markers(&r.body)),
            tags: extract_inline_tags(&r.body),
            heading_path: r.heading_path,
        })
        .filter(|s| !s.text.trim().is_empty())
        .collect();

    let merged = merge_small_siblings(sections, config);
    let final_sections: Vec<Section> = merged
        .into_iter()
        .flat_map(|s| split_oversize(s, config))
        .collect();

    Ok(final_sections
        .into_iter()
        .map(|s| {
            let mut tags = fm_tags.clone();
            for t in &s.tags {
                if !tags.contains(t) {
                    tags.push(t.clone());
                }
            }
            ChunkContext {
                source_file: source_file.to_string(),
                source_heading: if s.heading_path.is_empty() {
                    None
                } else {
                    Some(s.heading_path.join(" > "))
                },
                tags,
                text: s.text.trim().to_string(),
            }
        })
        .collect())
}

// ---------- internal types ----------

#[derive(Debug, Clone)]
struct RawSection {
    heading_path: Vec<String>,
    body: String,
}

#[derive(Debug, Clone)]
struct Section {
    heading_path: Vec<String>,
    text: String,
    tags: Vec<String>,
}

#[derive(Default, Deserialize)]
struct Frontmatter {
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    studybuddy: StudyBuddyFm,
}

#[derive(Default, Deserialize)]
struct StudyBuddyFm {
    #[serde(default)]
    exclude: bool,
}

// ---------- frontmatter ----------

pub(crate) fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let Some(after_open) = content.strip_prefix("---\n") else {
        return (None, content);
    };
    let Some(end) = after_open.find("\n---") else {
        return (None, content);
    };
    let yaml = &after_open[..end];
    let after_close = &after_open[end + 4..];
    let body = after_close.trim_start_matches(['\n', '\r']);
    (Some(yaml), body)
}

pub(crate) fn parse_frontmatter(yaml: Option<&str>) -> Result<(bool, Vec<String>)> {
    let Some(yaml) = yaml else {
        return Ok((false, vec![]));
    };
    let fm: Frontmatter = serde_yaml::from_str(yaml).map_err(|e| AppError::Parse(e.to_string()))?;
    Ok((fm.studybuddy.exclude, fm.tags))
}

// ---------- heading-based section collection ----------

fn collect_sections(body: &str) -> Vec<RawSection> {
    let mut sections = Vec::new();
    let mut current_path: Vec<String> = vec![];
    let mut current_body = String::new();
    let mut heading_stack: Vec<(usize, String)> = vec![];
    let mut in_code_fence = false;

    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_code_fence = !in_code_fence;
            current_body.push_str(line);
            current_body.push('\n');
            continue;
        }
        if in_code_fence {
            current_body.push_str(line);
            current_body.push('\n');
            continue;
        }

        if let Some((level, text)) = parse_atx_heading(line) {
            flush_section(&current_path, &mut current_body, &mut sections);
            while heading_stack.last().is_some_and(|(l, _)| *l >= level) {
                heading_stack.pop();
            }
            heading_stack.push((level, text));
            current_path = heading_stack.iter().map(|(_, t)| t.clone()).collect();
            continue;
        }

        current_body.push_str(line);
        current_body.push('\n');
    }
    flush_section(&current_path, &mut current_body, &mut sections);
    sections
}

fn flush_section(path: &[String], body: &mut String, sections: &mut Vec<RawSection>) {
    if body.trim().is_empty() {
        body.clear();
        return;
    }
    sections.push(RawSection {
        heading_path: path.to_vec(),
        body: std::mem::take(body),
    });
}

fn parse_atx_heading(line: &str) -> Option<(usize, String)> {
    let stripped = line.trim_start();
    let level = stripped.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let after = &stripped[level..];
    if !after.is_empty() && !after.starts_with(' ') {
        return None;
    }
    let text = after.trim().trim_end_matches('#').trim().to_string();
    Some((level, text))
}

// ---------- Obsidian transforms ----------

fn obsidian_transform(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(idx) = rest.find("[[") else {
            result.push_str(rest);
            return result;
        };
        let is_embed = idx > 0 && rest.as_bytes()[idx - 1] == b'!';
        let prefix_end = if is_embed { idx - 1 } else { idx };
        result.push_str(&rest[..prefix_end]);

        let after = &rest[idx + 2..];
        let Some(close) = after.find("]]") else {
            result.push_str(&rest[prefix_end..]);
            return result;
        };
        let inner = &after[..close];
        if !is_embed {
            let display = match inner.rfind('|') {
                Some(p) => &inner[p + 1..],
                None => inner,
            };
            result.push_str(display);
        }
        rest = &after[close + 2..];
    }
}

fn strip_callout_markers(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for line in text.lines() {
        if line == ">" {
            continue;
        }
        if let Some(after_arrow) = line.strip_prefix("> ") {
            if let Some(after_open) = after_arrow.strip_prefix("[!")
                && let Some(close) = after_open.find(']')
            {
                let title = after_open[close + 1..].trim_start();
                if !title.is_empty() {
                    result.push_str(title);
                    result.push('\n');
                }
                continue;
            }
            result.push_str(after_arrow);
            result.push('\n');
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

// ---------- tag extraction (prose only) ----------

fn extract_inline_tags(body: &str) -> Vec<String> {
    let cleaned = strip_code_regions(body);
    let mut tags = Vec::new();
    for word in cleaned.split_whitespace() {
        let Some(tag_raw) = word.strip_prefix('#') else {
            continue;
        };
        if !tag_raw
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let tag: String = tag_raw
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '/')
            .collect();
        if !tag.is_empty() {
            tags.push(tag);
        }
    }
    tags
}

fn strip_code_regions(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut in_inline = false;
        for c in line.chars() {
            if c == '`' {
                in_inline = !in_inline;
                continue;
            }
            if !in_inline {
                result.push(c);
            }
        }
        result.push('\n');
    }
    result
}

// ---------- size-aware merge / split ----------

fn merge_small_siblings(sections: Vec<Section>, config: &ChunkConfig) -> Vec<Section> {
    let mut result: Vec<Section> = Vec::new();
    let mut iter = sections.into_iter().peekable();

    while let Some(mut current) = iter.next() {
        let mut count = word_count(&current.text);
        while count < config.min_words {
            let Some(next) = iter.peek() else { break };
            if !are_siblings(&current.heading_path, &next.heading_path) {
                break;
            }
            let next_count = word_count(&next.text);
            if count + next_count > config.target_words {
                break;
            }
            let next = iter.next().unwrap();
            current.heading_path = common_prefix(&current.heading_path, &next.heading_path);
            current.text.push_str("\n\n");
            current.text.push_str(&next.text);
            for t in next.tags {
                if !current.tags.contains(&t) {
                    current.tags.push(t);
                }
            }
            count += next_count;
        }
        result.push(current);
    }
    result
}

fn split_oversize(section: Section, config: &ChunkConfig) -> Vec<Section> {
    if word_count(&section.text) <= config.max_words {
        return vec![section];
    }

    let paragraphs: Vec<&str> = section
        .text
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();

    let mut chunks: Vec<Section> = Vec::new();
    let mut current = String::new();
    let mut count = 0usize;

    for para in paragraphs {
        let para_count = word_count(para);
        if !current.is_empty() && count + para_count > config.max_words {
            chunks.push(Section {
                heading_path: section.heading_path.clone(),
                text: std::mem::take(&mut current),
                tags: section.tags.clone(),
            });
            count = 0;
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(para);
        count += para_count;
    }
    if !current.is_empty() {
        chunks.push(Section {
            heading_path: section.heading_path,
            text: current,
            tags: section.tags,
        });
    }
    chunks
}

fn are_siblings(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a[..a.len() - 1] == b[..b.len() - 1]
}

fn common_prefix(a: &[String], b: &[String]) -> Vec<String> {
    let n = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    a[..n].to_vec()
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

#[cfg(test)]
mod tests {
    //! Unit tests for the ingest parser/chunker. These exercise edge cases of
    //! the algorithm in isolation, using synthetic input via `ingest_text`.
    //! End-to-end behavior on realistic input is covered in `tests/ingest.rs`.

    use super::*;
    use std::collections::HashSet;

    fn parse(content: &str) -> Vec<ChunkContext> {
        ingest_text(content, "note.md", &ChunkConfig::default()).unwrap()
    }

    // ---- frontmatter & opt-out ----

    #[test]
    fn frontmatter_exclude_skips_file() {
        let content = "---\nstudybuddy:\n  exclude: true\n---\n\n# Heading\n\nbody text here.\n";
        assert!(
            parse(content).is_empty(),
            "expected empty result, got {:#?}",
            parse(content)
        );
    }

    #[test]
    fn frontmatter_tags_extracted_onto_chunks() {
        let content = "---\ntags: [math, calculus]\n---\n\n# Heading\n\nbody text.\n";
        let chunks = parse(content);
        assert_eq!(chunks.len(), 1, "expected one chunk, got {chunks:?}");
        let tags: HashSet<_> = chunks[0].tags.iter().cloned().collect();
        assert!(tags.contains("math"), "tags = {tags:?}");
        assert!(tags.contains("calculus"), "tags = {tags:?}");
    }

    #[test]
    fn frontmatter_block_excluded_from_chunk_text() {
        let content = "---\ntags: [x]\n---\n\n# Heading\n\nbody text.\n";
        let chunks = parse(content);
        assert!(!chunks[0].text.contains("tags:"));
        assert!(!chunks[0].text.contains("---"));
    }

    #[test]
    fn file_with_only_frontmatter_yields_no_chunks() {
        assert!(parse("---\ntags: [x]\n---\n").is_empty());
    }

    // ---- tag extraction ----

    #[test]
    fn inline_hashtag_extracted() {
        let chunks = parse("# H\n\nthis note covers #algebra topics.\n");
        assert!(
            chunks[0].tags.iter().any(|t| t == "algebra"),
            "tags = {:?}",
            chunks[0].tags
        );
    }

    #[test]
    fn hierarchical_tags_preserved_verbatim() {
        let chunks = parse("# H\n\ntagged #math/calculus here.\n");
        assert!(
            chunks[0].tags.iter().any(|t| t == "math/calculus"),
            "tags = {:?}",
            chunks[0].tags
        );
    }

    #[test]
    fn hashtags_in_fenced_code_block_are_not_tags() {
        let content = "# H\n\ntext\n\n```\n#not_a_tag\n```\n\nmore text.\n";
        let chunks = parse(content);
        assert!(
            !chunks[0].tags.iter().any(|t| t == "not_a_tag"),
            "code-block content should not be a tag: tags = {:?}",
            chunks[0].tags
        );
    }

    #[test]
    fn hashtags_in_inline_code_are_not_tags() {
        let content = "# H\n\nuse `#not_a_tag` for the literal value.\n";
        let chunks = parse(content);
        assert!(
            !chunks[0].tags.iter().any(|t| t == "not_a_tag"),
            "inline-code content should not be a tag: tags = {:?}",
            chunks[0].tags
        );
    }

    #[test]
    fn markdown_heading_does_not_become_a_tag() {
        let chunks = parse("## Section A\n\nbody.\n");
        assert!(
            !chunks[0].tags.iter().any(|t| t.contains("Section")),
            "heading text leaked as tag: {:?}",
            chunks[0].tags
        );
    }

    // ---- chunking by heading ----

    #[test]
    fn content_before_first_heading_has_none_heading() {
        let intro: String = "intro paragraph word ".repeat(60);
        let content = format!("{intro}\n\n# Heading\n\nbody text here.\n");
        let chunks = parse(&content);
        assert!(
            chunks.iter().any(|c| c.source_heading.is_none()),
            "no None-heading chunk in {chunks:?}"
        );
    }

    #[test]
    fn distinct_large_leaf_headings_yield_separate_chunks() {
        let big: String = "word ".repeat(600);
        let content = format!("# Top\n\n## A\n\n{big}\n\n## B\n\n{big}\n");
        let chunks = parse(&content);
        let headings: HashSet<_> = chunks
            .iter()
            .filter_map(|c| c.source_heading.clone())
            .collect();
        assert!(
            headings.iter().any(|h| h.contains("A")),
            "headings = {headings:?}"
        );
        assert!(
            headings.iter().any(|h| h.contains("B")),
            "headings = {headings:?}"
        );
    }

    #[test]
    fn empty_section_under_heading_does_not_emit_chunk() {
        let content =
            "# Top\n\n## Empty\n\n## NonEmpty\n\nactual content goes here in the second section.\n";
        let chunks = parse(content);
        let headings: HashSet<_> = chunks
            .iter()
            .filter_map(|c| c.source_heading.clone())
            .collect();
        assert!(
            !headings
                .iter()
                .any(|h| h.contains("Empty") && !h.contains("NonEmpty")),
            "empty section emitted chunk: {headings:?}"
        );
    }

    #[test]
    fn heading_path_includes_ancestors() {
        let big: String = "word ".repeat(600);
        let content = format!("# Linear Algebra\n\n## Vectors\n\n### Dot Product\n\n{big}\n");
        let chunks = parse(&content);
        let h = chunks
            .iter()
            .find_map(|c| c.source_heading.clone())
            .expect("heading present");
        assert!(h.contains("Linear Algebra"), "h = {h}");
        assert!(h.contains("Vectors"), "h = {h}");
        assert!(h.contains("Dot Product"), "h = {h}");
    }

    // ---- Obsidian syntax ----

    #[test]
    fn wikilink_basic_keeps_target_text() {
        let chunks = parse("# H\n\nSee [[foo]] for details.\n");
        assert!(
            chunks[0].text.contains("foo"),
            "text = {:?}",
            chunks[0].text
        );
        assert!(
            !chunks[0].text.contains("[[foo]]"),
            "raw wikilink remained: {:?}",
            chunks[0].text
        );
    }

    #[test]
    fn wikilink_with_alias_keeps_alias_only() {
        let chunks = parse("# H\n\nSee [[target|the alias]] for details.\n");
        assert!(
            chunks[0].text.contains("the alias"),
            "text = {:?}",
            chunks[0].text
        );
        assert!(
            !chunks[0].text.contains("[["),
            "raw wikilink remained: {:?}",
            chunks[0].text
        );
        assert!(
            !chunks[0].text.contains("target"),
            "non-aliased target leaked: {:?}",
            chunks[0].text
        );
    }

    #[test]
    fn embed_is_dropped() {
        let chunks = parse("# H\n\nBefore ![[embedded-note]] After text.\n");
        assert!(
            !chunks[0].text.contains("![["),
            "embed syntax remained: {:?}",
            chunks[0].text
        );
        assert!(
            !chunks[0].text.contains("embedded-note"),
            "embed target leaked: {:?}",
            chunks[0].text
        );
        assert!(chunks[0].text.contains("Before"));
        assert!(chunks[0].text.contains("After"));
    }

    #[test]
    fn callout_marker_stripped_content_preserved() {
        let content = "# H\n\n> [!note] Callout Title\n> callout body line.\n";
        let chunks = parse(content);
        assert!(chunks[0].text.contains("Callout Title"));
        assert!(chunks[0].text.contains("callout body line"));
        assert!(
            !chunks[0].text.contains("[!note]"),
            "callout marker remained: {:?}",
            chunks[0].text
        );
    }

    // ---- robustness ----

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(parse("").is_empty());
    }

    // ---- size-aware chunking ----

    #[test]
    fn small_siblings_merge_under_common_parent() {
        let small_a: String = "alpha alpha alpha alpha alpha ".repeat(4);
        let small_b: String = "beta beta beta beta beta ".repeat(4);
        let content = format!("# Top\n\n## Parent\n\n### A\n\n{small_a}\n\n### B\n\n{small_b}\n");
        let chunks = parse(&content);
        assert_eq!(
            chunks.len(),
            1,
            "expected siblings to merge into one chunk, got {chunks:?}"
        );
        let h = chunks[0].source_heading.as_ref().expect("heading present");
        assert!(
            h.contains("Top") && h.contains("Parent"),
            "common-ancestor path expected, got {h}"
        );
        assert!(
            chunks[0].text.contains("alpha") && chunks[0].text.contains("beta"),
            "merged chunk missing content"
        );
    }

    #[test]
    fn small_section_does_not_merge_with_large_sibling() {
        let big: String = "big ".repeat(400);
        let small: String = "small body word ".repeat(5);
        let content = format!("# Top\n\n## Parent\n\n### Big\n\n{big}\n\n### Small\n\n{small}\n");
        let chunks = parse(&content);
        assert!(
            chunks.len() >= 2,
            "expected at least 2 chunks (no merge), got {chunks:?}"
        );
    }

    #[test]
    fn oversize_section_with_subheadings_splits_at_subheadings() {
        let big: String = "word ".repeat(2000);
        let content = format!("## Section\n\n### SubA\n\n{big}\n\n### SubB\n\n{big}\n");
        let chunks = parse(&content);
        let headings: HashSet<_> = chunks
            .iter()
            .filter_map(|c| c.source_heading.clone())
            .collect();
        assert!(
            headings.iter().any(|h| h.contains("SubA")),
            "SubA not split out: {headings:?}"
        );
        assert!(
            headings.iter().any(|h| h.contains("SubB")),
            "SubB not split out: {headings:?}"
        );
    }

    #[test]
    fn oversize_section_without_subheadings_splits_at_paragraphs_and_respects_max() {
        let para1: String = "alpha ".repeat(900);
        let para2: String = "beta ".repeat(900);
        let content = format!("## Section\n\n{para1}\n\n{para2}\n");
        let chunks = parse(&content);
        assert!(
            chunks.len() >= 2,
            "expected paragraph split, got {} chunk(s)",
            chunks.len()
        );
        let cfg = ChunkConfig::default();
        for c in &chunks {
            let wc = word_count(&c.text);
            assert!(
                wc <= cfg.max_words,
                "chunk exceeded max_words ({} > {}): heading={:?}",
                wc,
                cfg.max_words,
                c.source_heading
            );
        }
    }

    #[test]
    fn tiny_lone_section_kept_as_own_chunk() {
        let chunks = parse("# Solo\n\nshort body of about ten words, kept whole.\n");
        assert_eq!(chunks.len(), 1, "expected single chunk, got {chunks:?}");
        let h = chunks[0].source_heading.as_ref().expect("heading present");
        assert!(h.contains("Solo"), "h = {h}");
    }
}
