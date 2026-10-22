use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

static SUMMARY_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").expect("summary link regex is valid"));

static SUMMARY_WIKI_LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[\[([^\]|]+)(?:\|([^\]]+))?\]\]").expect("summary wikilink regex is valid")
});

pub fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((idx, _)) => format!("{}…", text[..idx].trim_end()),
        None => text.to_string(),
    }
}

pub fn extract_title(path: &Path, body: &str) -> String {
    body.lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|title| !title.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "untitled".to_string())
}

pub fn infer_title(source_path: &Path, source_text: &str) -> String {
    source_text
        .lines()
        .find_map(|line| {
            let trimmed = line.trim().trim_start_matches('#').trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .or_else(|| {
            source_path
                .file_stem()
                .and_then(|name| name.to_str())
                .map(|name| name.replace(['-', '_'], " "))
        })
        .unwrap_or_else(|| "untitled source".to_string())
}

pub fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in input.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

pub fn tokenize(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| token.chars().count() > 1)
        .map(str::to_lowercase)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn sanitize_summary(summary: &str) -> String {
    single_line(summary)
}

/// One-line page summary for index.md. Links are reduced to their text: a link copied into the
/// index would resolve relative to the wrong directory and be mistaken for an index entry.
pub fn index_summary(line: &str) -> String {
    let text = SUMMARY_LINK_RE.replace_all(line, "$1");
    let text = SUMMARY_WIKI_LINK_RE.replace_all(&text, |caps: &regex::Captures<'_>| {
        caps.get(2)
            .unwrap_or_else(|| caps.get(1).expect("group 1 always matches"))
            .as_str()
            .to_string()
    });
    single_line(&text)
}

/// Collapse any whitespace run (including newlines) so text is safe in a heading or list item.
pub fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn escape_markdown_link_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

pub fn first_meaningful_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("# "))
}

pub fn ensure_trailing_newline(text: &str) -> String {
    if text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_respects_char_boundaries() {
        assert_eq!(truncate_chars("héllo", 5), "héllo");
        assert_eq!(truncate_chars("héllo wörld", 5), "héllo…");
        assert_eq!(truncate_chars("", 0), "");
    }

    #[test]
    fn slugify_collapses_separators_and_keeps_unicode_letters() {
        assert_eq!(slugify("Hello,  World!"), "hello-world");
        assert_eq!(slugify("--A__B--"), "a-b");
        assert_eq!(slugify("Über Café 2"), "über-café-2");
        assert_eq!(slugify("???"), "");
    }

    #[test]
    fn tokenize_drops_short_tokens_and_dedupes() {
        assert_eq!(tokenize("a Bc bc, d-ef 7"), vec!["bc", "ef"]);
    }

    #[test]
    fn extract_title_uses_first_h1_or_file_stem() {
        assert_eq!(
            extract_title(Path::new("x/y.md"), "text\n# Real\n## Sub\n"),
            "Real"
        );
        assert_eq!(extract_title(Path::new("x/y.md"), "#  \nno heading"), "y");
        assert_eq!(extract_title(Path::new("x/y.md"), "#Nospace\n"), "y");
    }

    #[test]
    fn infer_title_skips_blank_lines_and_heading_markers() {
        assert_eq!(
            infer_title(Path::new("f.txt"), "\n\n## Heading\nmore"),
            "Heading"
        );
        assert_eq!(
            infer_title(Path::new("my-file_name.txt"), "  \n"),
            "my file name"
        );
        assert_eq!(infer_title(Path::new(""), ""), "untitled source");
    }

    #[test]
    fn markdown_helpers_escape_and_flatten() {
        assert_eq!(escape_markdown_link_text(r"a[b]\c"), r"a\[b\]\\c");
        assert_eq!(single_line("  a \n\t b  "), "a b");
        assert_eq!(first_meaningful_line("# T\n\n  hi  \nlater"), Some("hi"));
        assert_eq!(first_meaningful_line("# T\n\n"), None);
        assert_eq!(ensure_trailing_newline("a"), "a\n");
        assert_eq!(ensure_trailing_newline("a\n"), "a\n");
    }
}
