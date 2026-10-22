use anyhow::{Context, Result, anyhow, bail};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::WikiPaths;
use crate::fsutil::{absolute_from_root, append_log, write_text_atomic};
use crate::index::reindex;
use crate::text::{infer_title, single_line, slugify};

pub fn ingest(paths: &WikiPaths, source: &Path, title: Option<String>) -> Result<PathBuf> {
    let source_path = absolute_from_root(&paths.root, source);
    let source_text = fs::read_to_string(&source_path).with_context(|| {
        format!(
            "failed to read source {} as UTF-8 text",
            source_path.display()
        )
    })?;
    let title = title
        .map(|title| single_line(&title))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| infer_title(&source_path, &source_text));
    let source_name = source_path
        .file_name()
        .ok_or_else(|| anyhow!("source path has no file name"))?;
    let copied_source = paths.raw_dir.join(source_name);

    let mut slug = slugify(&title);
    if slug.is_empty() {
        slug = "untitled".to_string();
    }
    let page_path = paths.sources_dir.join(format!("{slug}.md"));
    if page_path.exists() {
        bail!(
            "{} already exists; pass --title to choose a different page name",
            page_path.display()
        );
    }

    // Raw sources are immutable: never replace a file that differs from the incoming one.
    if copied_source.exists() {
        let existing = fs::read(&copied_source)
            .with_context(|| format!("failed to read {}", copied_source.display()))?;
        if existing != source_text.as_bytes() {
            bail!(
                "{} already exists with different content; raw sources are immutable",
                copied_source.display()
            );
        }
    } else {
        fs::copy(&source_path, &copied_source).with_context(|| {
            format!(
                "failed to copy source {} to {}",
                source_path.display(),
                copied_source.display()
            )
        })?;
    }

    let rel_source = copied_source.strip_prefix(&paths.root)?.to_path_buf();
    let body = source_stub_page(&title, &rel_source, &source_text);
    write_text_atomic(&page_path, &body)?;

    reindex(paths)?;
    let rel_page = page_path.strip_prefix(&paths.root)?.to_path_buf();
    append_log(
        &paths.log_path,
        "ingest",
        &title,
        &format!(
            "source={} page={}",
            rel_source.display(),
            rel_page.display()
        ),
    )?;
    Ok(rel_page)
}

pub fn source_stub_page(title: &str, source_path: &Path, source_text: &str) -> String {
    let excerpt = source_text.lines().take(20).collect::<Vec<_>>().join("\n");
    let excerpt = excerpt.trim();
    // Use a fence longer than any backtick run in the excerpt so it cannot be closed early.
    let longest_run = excerpt.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest_run.max(2) + 1);
    format!(
        "# {}\n\nSource: `{}`\n\n## Summary\n\nSource registered. This page is ready for the LLM to expand into a real summary and to link into the rest of the wiki.\n\n## Excerpt\n\n{fence}text\n{}\n{fence}\n",
        title,
        source_path.display(),
        excerpt
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::lint_issues;
    use crate::test_support::*;

    #[test]
    fn ingest_creates_stub_copies_source_and_logs_relative_paths() -> Result<()> {
        let (tmp, paths) = workspace("ingest-ok")?;
        let source = tmp.0.join("paper.txt");
        fs::write(&source, "Great Paper\nbody text\n")?;

        let rel_page = ingest(&paths, &source, None)?;

        assert_eq!(rel_page, Path::new("wiki/sources/great-paper.md"));
        assert_eq!(
            fs::read_to_string(paths.raw_dir.join("paper.txt"))?,
            "Great Paper\nbody text\n"
        );
        let stub = fs::read_to_string(tmp.0.join(&rel_page))?;
        assert!(stub.starts_with("# Great Paper\n"));
        assert!(stub.contains("Source: `raw/paper.txt`"));
        let log = fs::read_to_string(&paths.log_path)?;
        assert!(log.contains("] ingest | Great Paper\n"));
        assert!(log.contains("source=raw/paper.txt page=wiki/sources/great-paper.md"));
        assert!(fs::read_to_string(&paths.index_path)?.contains("sources/great-paper.md"));
        assert_eq!(lint_issues(&paths)?, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn ingest_never_overwrites_existing_stub_page() -> Result<()> {
        let (tmp, paths) = workspace("ingest-no-clobber-page")?;
        let source = tmp.0.join("a.txt");
        fs::write(&source, "Topic\nfirst\n")?;
        ingest(&paths, &source, None)?;
        let stub = paths.sources_dir.join("topic.md");
        fs::write(&stub, "# Topic\n\nHand-written summary.\n")?;

        let other = tmp.0.join("b.txt");
        fs::write(&other, "Topic\nsecond\n")?;
        let err = ingest(&paths, &other, None).unwrap_err();

        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(
            fs::read_to_string(&stub)?,
            "# Topic\n\nHand-written summary.\n"
        );
        assert!(
            !paths.raw_dir.join("b.txt").exists(),
            "no partial side effects"
        );
        Ok(())
    }

    #[test]
    fn ingest_never_overwrites_different_raw_file() -> Result<()> {
        let (tmp, paths) = workspace("ingest-no-clobber-raw")?;
        fs::write(paths.raw_dir.join("doc.txt"), "original")?;
        let incoming_dir = tmp.0.join("incoming");
        fs::create_dir_all(&incoming_dir)?;
        let incoming = incoming_dir.join("doc.txt");
        fs::write(&incoming, "changed")?;

        let err = ingest(&paths, &incoming, Some("Doc".into())).unwrap_err();

        assert!(err.to_string().contains("immutable"), "{err}");
        assert_eq!(
            fs::read_to_string(paths.raw_dir.join("doc.txt"))?,
            "original"
        );
        Ok(())
    }

    #[test]
    fn ingest_accepts_source_already_in_raw_dir() -> Result<()> {
        let (_tmp, paths) = workspace("ingest-in-raw")?;
        let source = paths.raw_dir.join("note.md");
        fs::write(&source, "# Note\n\nalready here\n")?;

        let rel_page = ingest(&paths, &source, None)?;

        assert_eq!(rel_page, Path::new("wiki/sources/note.md"));
        assert_eq!(fs::read_to_string(&source)?, "# Note\n\nalready here\n");
        Ok(())
    }

    #[test]
    fn ingest_non_ascii_title_gets_a_visible_page_name() -> Result<()> {
        let (tmp, paths) = workspace("ingest-unicode")?;
        let source = tmp.0.join("ru.txt");
        fs::write(&source, "Привет мир\n")?;
        let symbols = tmp.0.join("sym.txt");
        fs::write(&symbols, "???\n")?;

        let first = ingest(&paths, &source, None)?;
        let second = ingest(&paths, &symbols, None)?;

        assert_eq!(first, Path::new("wiki/sources/привет-мир.md"));
        assert_eq!(second, Path::new("wiki/sources/untitled.md"));
        Ok(())
    }

    #[test]
    fn ingest_title_override_is_flattened_to_one_line() -> Result<()> {
        let (tmp, paths) = workspace("ingest-title")?;
        let source = tmp.0.join("a.txt");
        fs::write(&source, "body\n")?;

        let rel_page = ingest(&paths, &source, Some("Two\nLines  here".into()))?;

        assert_eq!(rel_page, Path::new("wiki/sources/two-lines-here.md"));
        let stub = fs::read_to_string(tmp.0.join(rel_page))?;
        assert!(stub.starts_with("# Two Lines here\n"));
        Ok(())
    }

    #[test]
    fn stub_excerpt_fence_cannot_be_closed_by_source_content() {
        let stub = source_stub_page(
            "T",
            Path::new("raw/t.md"),
            "before\n```\ninside\n```\nafter",
        );
        assert!(stub.contains("````text\nbefore\n```\ninside\n```\nafter\n````\n"));
    }

    #[test]
    fn stub_excerpt_is_limited_to_twenty_lines() {
        let text = (1..=30)
            .map(|n| format!("line{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let stub = source_stub_page("T", Path::new("raw/t.md"), &text);
        assert!(stub.contains("line20"));
        assert!(!stub.contains("line21"));
    }
}
