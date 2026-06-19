use crate::llm::ChunkContext;

pub const SYSTEM_PROMPT: &str = "\
You author concise flashcards from study material. Given a chunk of source text, \
produce 1-5 self-contained Q&A cards.

Rules:
- Each card must be self-contained: a learner should be able to answer it without \
seeing the source material.
- Front: one concise question.
- Back: a complete, minimal answer (1-3 sentences).
- Prefer atomic facts: one concept per card.
- Don't invent content not present in the source.
- If the chunk is unsuitable (table of contents, navigation, code-only with no \
explanation), return an empty cards array.

Respond with JSON matching this schema:
{
  \"cards\": [
    { \"front\": \"<question>\", \"back\": \"<answer>\", \"rationale\": \"<optional brief reason>\" }
  ]
}";

pub fn render_user(chunk: &ChunkContext) -> String {
    let mut out = String::new();
    if let Some(heading) = &chunk.source_heading {
        out.push_str("Heading: ");
        out.push_str(heading);
        out.push('\n');
    }
    if !chunk.tags.is_empty() {
        out.push_str("Tags: ");
        out.push_str(&chunk.tags.join(", "));
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&chunk.text);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(heading: Option<&str>, tags: &[&str], text: &str) -> ChunkContext {
        ChunkContext {
            source_file: "n.md".to_string(),
            source_heading: heading.map(str::to_string),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            text: text.to_string(),
        }
    }

    #[test]
    fn renders_heading_tags_and_text() {
        let c = chunk(
            Some("Linear Algebra > Vectors"),
            &["math", "linear-algebra"],
            "A vector has direction and magnitude.",
        );
        assert_eq!(
            render_user(&c),
            "Heading: Linear Algebra > Vectors\n\
             Tags: math, linear-algebra\n\
             \n\
             A vector has direction and magnitude."
        );
    }

    #[test]
    fn omits_heading_line_when_absent() {
        let out = render_user(&chunk(None, &["math"], "body"));
        assert!(!out.contains("Heading:"), "got: {out:?}");
        assert!(out.contains("Tags: math\n"), "got: {out:?}");
        assert!(out.ends_with("body"), "got: {out:?}");
    }

    #[test]
    fn omits_tags_line_when_empty() {
        let out = render_user(&chunk(Some("H"), &[], "body"));
        assert!(out.contains("Heading: H\n"), "got: {out:?}");
        assert!(!out.contains("Tags:"), "got: {out:?}");
    }

    #[test]
    fn drops_blank_separator_when_no_metadata() {
        let out = render_user(&chunk(None, &[], "just the body"));
        assert_eq!(out, "just the body");
    }

    #[test]
    fn joins_multiple_tags_with_comma_space() {
        let out = render_user(&chunk(None, &["a", "b", "c"], "x"));
        assert!(out.contains("Tags: a, b, c\n"), "got: {out:?}");
    }
}
