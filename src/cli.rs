//! Command logic for the `sb` CLI, with injected I/O.
//!
//! Lives in the lib (not `bin/sb.rs`) so the integration-test crate can drive
//! it: each `run_*` takes a [`Client`] plus reader/writer handles, so tests pass
//! a `Cursor`/`Vec<u8>` and the real binary passes `stdin`/`stdout`. No domain
//! logic here either — these orchestrate the client and the terminal.

use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

use crate::client::Client;
use crate::model::{CardContent, CardId, ClozeSpan, Rating, Verdict};

/// `sb push` — push one note's markdown to the server for card generation.
///
/// `file` must live under `vault`; its path relative to the vault becomes the
/// card anchor the server records.
pub async fn run_push(
    client: &Client,
    vault: &Path,
    file: &Path,
    out: &mut impl Write,
) -> Result<()> {
    let source_file = vault_relative(vault, file)?;
    let content =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let counts = client.ingest(&source_file, &content).await?;
    writeln!(
        out,
        "{source_file}: {} chunk(s), {} proposed, {} failed, {} skipped",
        counts.chunks, counts.proposed_cards, counts.failed_chunks, counts.skipped_chunks
    )?;
    Ok(())
}

/// Compute `file`'s path relative to the `vault` root, as a clean
/// forward-slashed string. Both are canonicalized first (so `.`, `..`, and
/// relative inputs resolve), then the file must sit under the vault.
fn vault_relative(vault: &Path, file: &Path) -> Result<String> {
    let vault_abs = vault
        .canonicalize()
        .with_context(|| format!("resolving vault {}", vault.display()))?;
    let file_abs = file
        .canonicalize()
        .with_context(|| format!("resolving file {}", file.display()))?;
    relativize(&vault_abs, &file_abs)
}

/// Strip `vault_abs` from `file_abs` and render the remainder forward-slashed.
/// Pure (no filesystem) given two already-absolute paths — the testable core of
/// [`vault_relative`]. Errors if the file isn't under the vault.
fn relativize(vault_abs: &Path, file_abs: &Path) -> Result<String> {
    let rel = file_abs.strip_prefix(vault_abs).map_err(|_| {
        anyhow!(
            "{} is outside the vault {}",
            file_abs.display(),
            vault_abs.display()
        )
    })?;
    if rel.as_os_str().is_empty() {
        bail!("file resolves to the vault root itself, not a note");
    }
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Ok(parts.join("/"))
}

/// `sb review` — run a spaced-repetition session over the currently-due cards.
///
/// Shows each card's question, reveals the answer on Enter, reads a rating key,
/// and records the review. The due list is snapshotted once at the start.
pub async fn run_review(
    client: &Client,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<()> {
    let due = client.due().await?;
    if due.is_empty() {
        writeln!(out, "nothing due")?;
        return Ok(());
    }

    let total = due.len();
    for (i, card) in due.iter().enumerate() {
        writeln!(
            out,
            "\n[{}/{total}] {}",
            i + 1,
            render_question(&card.content)
        )?;

        let rating = match &card.content {
            CardContent::Qa { back, .. } => {
                evaluate_and_rate_qa(client, card.id, back, input, out).await?
            }
            CardContent::Cloze { text, .. } => reveal_and_rate_cloze(text, input, out)?,
        };

        match rating {
            Some(r) => {
                let outcome = client.review(card.id, r).await?;
                writeln!(out, "  next due in {} day(s)", outcome.interval_days)?;
            }
            None => {
                writeln!(out, "\ninput ended; stopping review")?;
                return Ok(());
            }
        }
    }
    writeln!(out, "\nreviewed {total} card(s)")?;
    Ok(())
}

/// Prompt for a free-text answer on a Q&A card, optionally evaluate it against
/// the server, reveal the expected answer, then collect a rating.
///
/// Returns `None` on EOF, signalling the caller to stop the session.
async fn evaluate_and_rate_qa(
    client: &Client,
    card_id: CardId,
    back: &str,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<Option<Rating>> {
    write!(out, "your answer (or Enter to reveal): ")?;
    out.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;

    let suggested = if !answer.trim().is_empty() {
        match client.evaluate(card_id, answer.trim()).await {
            Ok(eval) => {
                writeln!(
                    out,
                    "  {} — {}",
                    verdict_label(eval.verdict),
                    eval.explanation
                )?;
                writeln!(out, "  Expected: {back}")?;
                Some(eval.suggested_rating)
            }
            Err(e) => {
                writeln!(out, "  evaluation unavailable: {e}")?;
                writeln!(out, "{back}")?;
                None
            }
        }
    } else {
        writeln!(out, "{back}")?;
        None
    };

    read_rating(input, out, suggested)
}

/// Reveal a cloze card's filled text on Enter, then collect a rating.
///
/// Returns `None` on EOF, signalling the caller to stop the session.
fn reveal_and_rate_cloze(
    text: &str,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<Option<Rating>> {
    write!(out, "(press Enter to reveal) ")?;
    out.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    writeln!(out, "{text}")?;
    read_rating(input, out, None)
}

/// Read a rating keystroke in a loop, returning `None` on EOF.
fn read_rating(
    input: &mut impl BufRead,
    out: &mut impl Write,
    suggested: Option<Rating>,
) -> Result<Option<Rating>> {
    loop {
        write!(out, "{}", rating_prompt(suggested))?;
        out.flush()?;
        let mut key = String::new();
        if input.read_line(&mut key)? == 0 {
            return Ok(None);
        }
        if let Some(rating) = rating_from_key(&key) {
            return Ok(Some(rating));
        }
        writeln!(out, "  unrecognized — enter 1, 2, 3, or 4")?;
    }
}

/// Rating prompt string, with the suggested rating wrapped in `*...*`.
fn rating_prompt(suggested: Option<Rating>) -> String {
    let labels: &[(Rating, &str)] = &[
        (Rating::Again, "1=again"),
        (Rating::Hard, "2=hard"),
        (Rating::Good, "3=good"),
        (Rating::Easy, "4=easy"),
    ];
    let parts: Vec<String> = labels
        .iter()
        .map(|(r, label)| {
            if suggested == Some(*r) {
                format!("*{label}*")
            } else {
                (*label).to_string()
            }
        })
        .collect();
    format!("rate [{}]: ", parts.join(" "))
}

/// Human-readable label for an evaluation verdict.
fn verdict_label(v: Verdict) -> &'static str {
    match v {
        Verdict::Correct => "Correct",
        Verdict::Partial => "Partial",
        Verdict::Incorrect => "Incorrect",
    }
}

/// `sb curate` — walk the pending cards, accepting/rejecting/editing each.
///
/// `edit` turns a card's current content into edited content (the real binary
/// opens `$EDITOR`; tests pass a closure). An edit leaves the card pending, so
/// the prompt repeats — letting the user edit then accept in one pass.
pub async fn run_curate(
    client: &Client,
    input: &mut impl BufRead,
    out: &mut impl Write,
    edit: impl Fn(&CardContent) -> Result<CardContent>,
) -> Result<()> {
    let pending = client.pending().await?;
    if pending.is_empty() {
        writeln!(out, "nothing pending")?;
        return Ok(());
    }

    let total = pending.len();
    for (i, card) in pending.iter().enumerate() {
        let mut content = card.content.clone();
        writeln!(out, "\n[{}/{total}] {}", i + 1, card.source_file)?;
        write_card(out, &content)?;

        loop {
            write!(out, "[a]ccept [r]eject [e]dit [s]kip [q]uit: ")?;
            out.flush()?;
            let mut cmd = String::new();
            if input.read_line(&mut cmd)? == 0 {
                writeln!(out, "\ninput ended; stopping")?;
                return Ok(());
            }
            match cmd.trim() {
                "a" => {
                    client.accept(card.id).await?;
                    writeln!(out, "  accepted")?;
                    break;
                }
                "r" => {
                    client.reject(card.id).await?;
                    writeln!(out, "  rejected")?;
                    break;
                }
                "s" => {
                    writeln!(out, "  skipped")?;
                    break;
                }
                "q" => {
                    writeln!(out, "stopping")?;
                    return Ok(());
                }
                "e" => match edit(&content) {
                    Ok(edited) => {
                        client.patch(card.id, edited.clone()).await?;
                        content = edited;
                        writeln!(out, "  edited")?;
                        write_card(out, &content)?;
                    }
                    Err(e) => writeln!(out, "  edit aborted: {e}")?,
                },
                other => writeln!(out, "  unrecognized '{other}' — use a/r/e/s/q")?,
            }
        }
    }
    writeln!(out, "\ncurated {total} card(s)")?;
    Ok(())
}

/// Print a card's content in full (both sides) for curation review.
fn write_card(out: &mut impl Write, content: &CardContent) -> Result<()> {
    match content {
        CardContent::Qa { front, back } => {
            writeln!(out, "  Q: {front}")?;
            writeln!(out, "  A: {back}")?;
        }
        CardContent::Cloze { text, spans } => {
            writeln!(out, "  Cloze: {text}")?;
            writeln!(out, "  Blanked: {}", render_cloze_blanked(text, spans))?;
        }
    }
    Ok(())
}

/// Map a review rating keystroke to a [`Rating`]; `None` if unrecognized.
fn rating_from_key(key: &str) -> Option<Rating> {
    match key.trim() {
        "1" => Some(Rating::Again),
        "2" => Some(Rating::Hard),
        "3" => Some(Rating::Good),
        "4" => Some(Rating::Easy),
        _ => None,
    }
}

/// The question side of a card: the Q&A front, or a cloze with its spans blanked.
fn render_question(content: &CardContent) -> String {
    match content {
        CardContent::Qa { front, .. } => front.clone(),
        CardContent::Cloze { text, spans } => render_cloze_blanked(text, spans),
    }
}

/// Render cloze `text` with each span replaced by a `{{...}}` blank (carrying
/// its hint when present). Spans are taken in `start` order; malformed or
/// overlapping spans are skipped rather than panicking.
fn render_cloze_blanked(text: &str, spans: &[ClozeSpan]) -> String {
    let mut ordered: Vec<&ClozeSpan> = spans.iter().collect();
    ordered.sort_by_key(|s| s.start);

    let mut out = String::new();
    let mut cursor = 0usize;
    for s in ordered {
        // `start`/`end` are byte offsets into `text`; slicing assumes they land
        // on char boundaries (true for ASCII v1). A mid-char offset would panic —
        // revisit if the LLM ever emits multibyte-spanning offsets.
        if s.start < cursor || s.end > text.len() || s.start > s.end {
            continue;
        }
        out.push_str(&text[cursor..s.start]);
        match &s.hint {
            Some(hint) => out.push_str(&format!("{{{{...: {hint}}}}}")),
            None => out.push_str("{{...}}"),
        }
        cursor = s.end;
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relativize_strips_vault_prefix() {
        let rel = relativize(Path::new("/vault"), Path::new("/vault/linear/vectors.md")).unwrap();
        assert_eq!(rel, "linear/vectors.md");
    }

    #[test]
    fn relativize_rejects_file_outside_vault() {
        let err = relativize(Path::new("/vault"), Path::new("/elsewhere/a.md")).unwrap_err();
        assert!(err.to_string().contains("outside the vault"), "{err}");
    }

    #[test]
    fn relativize_rejects_vault_root_itself() {
        let err = relativize(Path::new("/vault"), Path::new("/vault")).unwrap_err();
        assert!(err.to_string().contains("vault root"), "{err}");
    }

    #[test]
    fn rating_keys_map_to_ratings() {
        assert_eq!(rating_from_key("1\n"), Some(Rating::Again));
        assert_eq!(rating_from_key(" 2 "), Some(Rating::Hard));
        assert_eq!(rating_from_key("3"), Some(Rating::Good));
        assert_eq!(rating_from_key("4"), Some(Rating::Easy));
        assert_eq!(rating_from_key("x"), None);
        assert_eq!(rating_from_key(""), None);
    }

    #[test]
    fn cloze_blanks_spans_on_the_question_side() {
        let text = "The capital of France is Paris.";
        let start = text.find("Paris").unwrap();
        let spans = vec![ClozeSpan {
            start,
            end: start + "Paris".len(),
            hint: None,
        }];
        assert_eq!(
            render_cloze_blanked(text, &spans),
            "The capital of France is {{...}}."
        );
    }

    #[test]
    fn cloze_blank_carries_hint_when_present() {
        let text = "Mitochondria are the powerhouse of the cell.";
        let start = text.find("powerhouse").unwrap();
        let spans = vec![ClozeSpan {
            start,
            end: start + "powerhouse".len(),
            hint: Some("energy".to_string()),
        }];
        assert!(
            render_cloze_blanked(text, &spans).contains("{{...: energy}}"),
            "hint should appear in the blank"
        );
    }

    #[test]
    fn verdict_label_maps_all_variants() {
        assert_eq!(verdict_label(Verdict::Correct), "Correct");
        assert_eq!(verdict_label(Verdict::Partial), "Partial");
        assert_eq!(verdict_label(Verdict::Incorrect), "Incorrect");
    }

    #[test]
    fn rating_prompt_marks_suggested_rating() {
        assert!(rating_prompt(Some(Rating::Good)).contains("*3=good*"));
        assert!(!rating_prompt(Some(Rating::Good)).contains("*1=again*"));
    }

    #[test]
    fn rating_prompt_unmodified_when_no_suggestion() {
        let prompt = rating_prompt(None);
        assert!(prompt.contains("1=again"));
        assert!(!prompt.contains('*'));
    }
}
