//! The 50/72 rule, enforced on whatever the model returns. The system
//! prompt asks for the format, but asking is not enforcing: `app` runs the
//! reply through `sanitize`, `parse`, the subject check, and `wrap_body`,
//! so the printed message obeys the rule even when the model does not.

pub(crate) const SUBJECT_LIMIT: usize = 50;
pub(crate) const BODY_WIDTH: usize = 72;

/// A commit message pulled apart for enforcement: the subject is checked
/// against `SUBJECT_LIMIT`, the body wrapped to `BODY_WIDTH`. An empty body
/// means a subject-only message, the common case.
pub(crate) struct Formatted {
    pub subject: String,
    pub body: String,
}

/// Strip the wrapping a model tends to add around a commit message: code
/// fences, a single pair of quotes, and CRLF line endings.
pub(crate) fn sanitize(raw: &str) -> String {
    let normalized = raw.replace("\r\n", "\n");
    let mut text = normalized.trim();
    if text.starts_with("```")
        && let Some(inner) = text.strip_suffix("```")
        && let Some((_fence_line, rest)) = inner.split_once('\n')
    {
        text = rest.trim();
    }
    for quote in ['"', '\''] {
        if text.len() >= 2 && text.starts_with(quote) && text.ends_with(quote) {
            text = text[1..text.len() - 1].trim();
        }
    }
    text.to_owned()
}

/// First non-empty line becomes the subject (trailing periods stripped);
/// everything after it becomes the body.
pub(crate) fn parse(raw: &str) -> Formatted {
    let mut lines = raw.lines();
    let mut subject = String::new();
    for line in lines.by_ref() {
        if !line.trim().is_empty() {
            subject = line.trim().trim_end_matches('.').trim_end().to_owned();
            break;
        }
    }
    let body = lines
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_owned();
    Formatted { subject, body }
}

pub(crate) fn subject_fits(subject: &str) -> bool {
    width(subject) <= SUBJECT_LIMIT
}

/// Cut at the last word boundary within the limit; hard cut if the subject
/// is a single long word. No ellipsis: git subjects conventionally just end.
pub(crate) fn truncate_subject(subject: &str) -> String {
    if subject_fits(subject) {
        return subject.to_owned();
    }
    let cut: String = subject.chars().take(SUBJECT_LIMIT).collect();
    match cut.rfind(' ') {
        Some(i) if i > 0 => cut[..i].trim_end().to_owned(),
        _ => cut,
    }
}

/// Wrap body lines at 72 columns. Lines are never joined, so bullet lists
/// and intentional short lines survive; runs of blank lines collapse to one.
pub(crate) fn wrap_body(body: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut prev_blank = false;
    for line in body.lines() {
        if line.trim().is_empty() {
            if !prev_blank && !out.is_empty() {
                out.push(String::new());
                prev_blank = true;
            }
            continue;
        }
        prev_blank = false;
        if width(line.trim_end()) <= BODY_WIDTH {
            out.push(line.trim_end().to_owned());
        } else {
            out.extend(wrap_line(line, BODY_WIDTH));
        }
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out.join("\n")
}

/// The final output shape: the subject, then one blank line and the body
/// when there is one. `app` prints this with a single trailing newline,
/// which `$(lede generate)` strips, so `git commit -m` receives exactly the message.
pub(crate) fn render(message: &Formatted) -> String {
    if message.body.is_empty() {
        message.subject.clone()
    } else {
        format!("{}\n\n{}", message.subject, message.body)
    }
}

/// Columns as chars. Wide glyphs count as one; unicode-width is not worth a
/// dependency for commit messages.
fn width(s: &str) -> usize {
    s.chars().count()
}

/// Greedy word wrap of one over-long line, preserving its indentation.
fn wrap_line(line: &str, limit: usize) -> Vec<String> {
    let content = line.trim_start();
    let indent = &line[..line.len() - content.len()];
    // A wrapped bullet keeps its text aligned under itself, not under the
    // marker.
    let bullet = content.starts_with("- ") || content.starts_with("* ");
    let cont_indent = if bullet {
        format!("{indent}  ")
    } else {
        indent.to_owned()
    };

    let mut lines = Vec::new();
    let mut current = indent.to_owned();
    let mut has_word = false;
    for word in content.split_whitespace() {
        if has_word && width(&current) + 1 + width(word) > limit {
            lines.push(current);
            current = cont_indent.clone();
            has_word = false;
        }
        if has_word {
            current.push(' ');
        }
        // A single word over the limit (a URL, a hash) lands here with an
        // empty line and is emitted unbroken; a broken URL is worse than a
        // long line.
        current.push_str(word);
        has_word = true;
    }
    if has_word {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_a_code_fence() {
        let raw = "```\nAdd feature\n\nBody text.\n```";
        assert_eq!(sanitize(raw), "Add feature\n\nBody text.");
    }

    #[test]
    fn sanitize_strips_a_fence_with_a_language_tag() {
        assert_eq!(sanitize("```text\nAdd feature\n```"), "Add feature");
    }

    #[test]
    fn sanitize_strips_wrapping_quotes() {
        assert_eq!(sanitize("\"Add feature\""), "Add feature");
        assert_eq!(sanitize("'Add feature'"), "Add feature");
    }

    #[test]
    fn sanitize_normalizes_crlf() {
        assert_eq!(sanitize("Add feature\r\n\r\nBody."), "Add feature\n\nBody.");
    }

    #[test]
    fn sanitize_keeps_interior_quotes() {
        assert_eq!(sanitize("Rename \"foo\" to bar"), "Rename \"foo\" to bar");
    }

    #[test]
    fn parse_splits_subject_and_body() {
        let message = parse("Add feature\n\nSome body.\nMore body.");
        assert_eq!(message.subject, "Add feature");
        assert_eq!(message.body, "Some body.\nMore body.");
    }

    #[test]
    fn parse_strips_a_trailing_period_from_the_subject() {
        assert_eq!(parse("Add feature.").subject, "Add feature");
    }

    #[test]
    fn parse_skips_leading_blank_lines() {
        let message = parse("\n\nAdd feature\n\nBody.");
        assert_eq!(message.subject, "Add feature");
        assert_eq!(message.body, "Body.");
    }

    #[test]
    fn a_subject_alone_has_an_empty_body() {
        let message = parse("Add feature");
        assert_eq!(message.subject, "Add feature");
        assert_eq!(message.body, "");
    }

    #[test]
    fn truncation_cuts_at_a_word_boundary() {
        let long = "Add a very long subject line that keeps going well past fifty";
        let cut = truncate_subject(long);
        assert!(subject_fits(&cut));
        assert!(!cut.ends_with(' '));
        assert!(long.starts_with(&cut));
        assert_eq!(cut, "Add a very long subject line that keeps going");
    }

    #[test]
    fn truncation_hard_cuts_a_single_long_word() {
        let long = "a".repeat(80);
        assert_eq!(truncate_subject(&long).chars().count(), SUBJECT_LIMIT);
    }

    #[test]
    fn truncation_counts_chars_not_bytes() {
        let long = "é".repeat(60);
        assert_eq!(truncate_subject(&long).chars().count(), SUBJECT_LIMIT);
    }

    #[test]
    fn truncation_leaves_a_short_subject_alone() {
        assert_eq!(truncate_subject("Add feature"), "Add feature");
    }

    #[test]
    fn wrapping_leaves_short_lines_alone() {
        let body = "Short line.\n\nAnother short line.";
        assert_eq!(wrap_body(body), body);
    }

    #[test]
    fn wrapping_holds_every_line_to_72() {
        let body = "word ".repeat(30);
        for line in wrap_body(&body).lines() {
            assert!(line.chars().count() <= BODY_WIDTH, "too long: {line}");
        }
    }

    #[test]
    fn a_wrapped_bullet_gets_a_hanging_indent() {
        let body = format!("- {}", "word ".repeat(30).trim_end());
        let wrapped = wrap_body(&body);
        let mut lines = wrapped.lines();
        assert!(lines.next().unwrap().starts_with("- "));
        for continuation in lines {
            assert!(
                continuation.starts_with("  word"),
                "bad continuation: {continuation:?}"
            );
        }
    }

    #[test]
    fn a_long_token_stays_unbroken() {
        let url = format!("https://example.com/{}", "x".repeat(80));
        let body = format!("See {url} for details");
        assert!(wrap_body(&body).lines().any(|line| line == url));
    }

    #[test]
    fn blank_runs_collapse_to_one() {
        assert_eq!(wrap_body("a\n\n\n\nb"), "a\n\nb");
    }

    #[test]
    fn leading_and_trailing_blanks_drop() {
        assert_eq!(wrap_body("\n\na\n\n"), "a");
    }

    #[test]
    fn a_subject_renders_alone() {
        let message = Formatted {
            subject: "Add feature".into(),
            body: String::new(),
        };
        assert_eq!(render(&message), "Add feature");
    }

    #[test]
    fn a_body_renders_after_one_blank_line() {
        let message = Formatted {
            subject: "Add feature".into(),
            body: "Body.".into(),
        };
        assert_eq!(render(&message), "Add feature\n\nBody.");
    }
}
