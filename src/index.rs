use anyhow::{Context, Result, bail};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::config::WikiPaths;
use crate::fsutil::{
    index_link_path, normalize_abs_link_to_rel, normalize_link_target, write_text_atomic,
};
use crate::pages::{PageIndex, extract_links, load_pages};
use crate::text::{escape_markdown_link_text, first_meaningful_line, index_summary};

static INDEX_LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[(?:\\.|[^\\\]])+\]\(([^)]+\.md)\)").expect("index link regex is valid")
});

pub fn reindex(paths: &WikiPaths) -> Result<()> {
    let pages = load_pages(paths)?;
    let index_rel = paths.index_path.strip_prefix(&paths.root)?;
    let log_rel = paths.log_path.strip_prefix(&paths.root)?;
    let sources_rel = paths.sources_dir.strip_prefix(&paths.root)?;
    let analyses_rel = paths.analyses_dir.strip_prefix(&paths.root)?;
    let mut sources = Vec::new();
    let mut analyses = Vec::new();
    let mut other = Vec::new();

    for page in &pages {
        if page.rel_path == index_rel || page.rel_path == log_rel {
            continue;
        }
        let entry = format!(
            "- [{}]({}) - {}",
            escape_markdown_link_text(&page.title),
            index_link_path(&paths.index_path, &page.abs_path).display(),
            index_summary(first_meaningful_line(&page.body).unwrap_or("wiki page"))
        );
        if page.rel_path.starts_with(sources_rel) {
            sources.push(entry);
        } else if page.rel_path.starts_with(analyses_rel) {
            analyses.push(entry);
        } else {
            other.push(entry);
        }
    }

    sources.sort();
    analyses.sort();
    other.sort();

    let mut index = String::from("# Index\n");
    push_section(&mut index, "Sources", &sources);
    push_section(&mut index, "Analyses", &analyses);
    if !other.is_empty() {
        push_section(&mut index, "Other", &other);
    }

    let indexed = indexed_pages_from_text(paths, &index);
    let missing = pages
        .iter()
        .filter(|page| page.rel_path != index_rel && page.rel_path != log_rel)
        .filter(|page| !indexed.contains(&page.rel_path))
        .map(|page| page.rel_path.display().to_string())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "generated index is incomplete; missing entries for {}",
            missing.join(", ")
        );
    }

    write_text_atomic(&paths.index_path, &index)?;
    Ok(())
}

pub fn lint_issues(paths: &WikiPaths) -> Result<Vec<String>> {
    let pages = load_pages(paths)?;
    let page_index = PageIndex::new(&pages);
    let indexed = indexed_pages(paths)?;
    let analyses_rel = paths.analyses_dir.strip_prefix(&paths.root)?;
    let index_rel = paths.index_path.strip_prefix(&paths.root)?;
    let log_rel = paths.log_path.strip_prefix(&paths.root)?;
    let mut inbound: HashMap<PathBuf, usize> = HashMap::new();
    let mut issues = Vec::new();

    for page in &pages {
        if page.rel_path != index_rel
            && page.rel_path != log_rel
            && !indexed.contains(&page.rel_path)
        {
            issues.push(format!("missing index entry: {}", page.rel_path.display()));
        }

        let from_index = page.rel_path == index_rel;
        let from_log = page.rel_path == log_rel;
        for link in extract_links(paths, page, &page_index) {
            let target = PathBuf::from(&link.target);
            if !link.exists {
                if from_index {
                    issues.push(format!("stale index entry: {}", link.target));
                } else {
                    issues.push(format!("broken link: {} -> {}", link.source, link.target));
                }
            } else if !from_index && !from_log && target != page.rel_path {
                // The index lists every page and the log mentions many; neither (nor a
                // self-link) makes a page reachable, so they must not hide orphans.
                *inbound.entry(target).or_insert(0) += 1;
            }
        }
    }

    for page in &pages {
        if page.rel_path.starts_with(analyses_rel)
            && inbound.get(&page.rel_path).copied().unwrap_or(0) == 0
        {
            issues.push(format!("orphan page: {}", page.rel_path.display()));
        }
    }

    Ok(issues)
}

pub fn indexed_pages(paths: &WikiPaths) -> Result<HashSet<PathBuf>> {
    let index = fs::read_to_string(&paths.index_path)
        .with_context(|| format!("failed to read {}", paths.index_path.display()))?;
    Ok(indexed_pages_from_text(paths, &index))
}

fn indexed_pages_from_text(paths: &WikiPaths, index: &str) -> HashSet<PathBuf> {
    let base = paths.index_path.parent().unwrap_or_else(|| Path::new(""));
    INDEX_LINK_RE
        .captures_iter(index)
        .filter_map(|caps| caps.get(1).map(|m| normalize_link_target(base, m.as_str())))
        .map(|path| normalize_abs_link_to_rel(&paths.root, &path))
        .collect()
}

fn push_section(index: &mut String, name: &str, entries: &[String]) {
    let _ = write!(index, "\n## {name}\n");
    if entries.is_empty() {
        index.push('\n');
        return;
    }
    for entry in entries {
        index.push_str(entry);
        index.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SCHEMA_FILE;
    use crate::test_support::*;

    #[test]
    fn reindex_includes_root_pages_with_special_characters_in_titles() -> Result<()> {
        let (_tmp, paths) = workspace("reindex-special-title")?;
        fs::write(
            paths.wiki_dir.join("source-security-context.md"),
            "# Source Security Context [Draft]\n\nA newly added page.\n",
        )?;

        reindex(&paths)?;

        let index = fs::read_to_string(&paths.index_path)?;
        assert!(index.contains(r"[Source Security Context \[Draft\]](source-security-context.md)"));
        let indexed = indexed_pages(&paths)?;
        assert!(indexed.contains(Path::new("wiki/source-security-context.md")));
        Ok(())
    }

    #[test]
    fn lint_sees_new_page_immediately_after_reindex() -> Result<()> {
        let (_tmp, paths) = workspace("lint-after-reindex")?;
        fs::write(
            paths.wiki_dir.join("source-foo.md"),
            "# Source Foo\n\nA newly added page.\n",
        )?;

        reindex(&paths)?;

        assert_eq!(lint_issues(&paths)?, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn raw_and_schema_are_not_indexed_when_nested_in_wiki_dir() -> Result<()> {
        let (_tmp, mut paths) = workspace("raw-nested-in-wiki")?;
        paths.raw_dir = paths.wiki_dir.join("raw");
        paths.schema_path = paths.wiki_dir.join(SCHEMA_FILE);
        fs::create_dir_all(&paths.raw_dir)?;
        fs::write(&paths.schema_path, "# Wiki Schema\n")?;
        fs::write(
            paths.raw_dir.join("doc.md"),
            "# Raw Doc\n\nImmutable source.\n",
        )?;
        fs::write(
            paths.sources_dir.join("doc-stub.md"),
            "# Doc Stub\n\nSummary of the raw doc. Back to [a](../page-a.md).\n",
        )?;
        fs::write(
            paths.wiki_dir.join("page-a.md"),
            "# Page A\n\nLinks to [stub](sources/doc-stub.md).\n",
        )?;

        reindex(&paths)?;

        let index = fs::read_to_string(&paths.index_path)?;
        assert!(!index.contains("raw/doc.md"));
        assert!(!index.contains("WIKI.md"));
        assert!(index.contains("sources/doc-stub.md"));
        assert_eq!(lint_issues(&paths)?, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn lint_reports_orphan_analysis_even_though_index_lists_it() -> Result<()> {
        let (_tmp, paths) = workspace("lint-orphan")?;
        fs::write(
            paths.analyses_dir.join("lonely.md"),
            "# Lonely\n\nNobody links here.\n",
        )?;
        reindex(&paths)?;

        assert_eq!(
            lint_issues(&paths)?,
            vec!["orphan page: wiki/analyses/lonely.md".to_string()]
        );
        Ok(())
    }

    #[test]
    fn lint_orphan_cleared_by_link_from_another_page_but_not_self_link() -> Result<()> {
        let (_tmp, paths) = workspace("lint-orphan-linked")?;
        fs::write(
            paths.analyses_dir.join("a.md"),
            "# A\n\nSee [self](a.md) and [b](b.md).\n",
        )?;
        fs::write(paths.analyses_dir.join("b.md"), "# B\n\nBody.\n")?;
        reindex(&paths)?;

        assert_eq!(
            lint_issues(&paths)?,
            vec!["orphan page: wiki/analyses/a.md".to_string()]
        );
        Ok(())
    }

    #[test]
    fn lint_reports_broken_links_and_stale_index_entries() -> Result<()> {
        let (_tmp, paths) = workspace("lint-stale")?;
        fs::write(paths.sources_dir.join("gone.md"), "# Gone\n\nTemporary.\n")?;
        fs::write(
            paths.sources_dir.join("kept.md"),
            "# Kept\n\nPoints at [nothing](missing.md).\n",
        )?;
        reindex(&paths)?;
        fs::remove_file(paths.sources_dir.join("gone.md"))?;

        let issues = lint_issues(&paths)?;
        assert!(issues.contains(&"stale index entry: wiki/sources/gone.md".to_string()));
        assert!(
            issues.contains(
                &"broken link: wiki/sources/kept.md -> wiki/sources/missing.md".to_string()
            )
        );
        assert_eq!(issues.len(), 2, "{issues:?}");
        Ok(())
    }

    #[test]
    fn lint_reports_pages_missing_from_index() -> Result<()> {
        let (_tmp, paths) = workspace("lint-missing-index")?;
        reindex(&paths)?;
        fs::write(paths.sources_dir.join("new.md"), "# New\n\nBody.\n")?;

        assert_eq!(
            lint_issues(&paths)?,
            vec!["missing index entry: wiki/sources/new.md".to_string()]
        );
        Ok(())
    }

    #[test]
    fn index_summary_strips_links_so_they_are_not_mistaken_for_entries() -> Result<()> {
        assert_eq!(
            index_summary("See [self](a.md), ![img](x.png) and [[Topic|the topic]] or [[Other]]."),
            "See self, img and the topic or Other."
        );

        let (_tmp, paths) = workspace("summary-links")?;
        fs::write(
            paths.analyses_dir.join("a.md"),
            "# A\n\nSee [b](b.md) for more.\n",
        )?;
        reindex(&paths)?;
        fs::write(paths.analyses_dir.join("b.md"), "# B\n\nBody.\n")?;

        // b.md is only *mentioned* in a's summary; it must still be reported as unindexed.
        let issues = lint_issues(&paths)?;
        assert!(issues.contains(&"missing index entry: wiki/analyses/b.md".to_string()));
        assert!(
            !issues.iter().any(|issue| issue.starts_with("stale")),
            "{issues:?}"
        );
        Ok(())
    }

    #[test]
    fn reindex_does_not_list_index_or_log() -> Result<()> {
        let (_tmp, paths) = workspace("reindex-self")?;
        fs::write(paths.sources_dir.join("a.md"), "# A\n\nBody.\n")?;

        reindex(&paths)?;

        let index = fs::read_to_string(&paths.index_path)?;
        assert!(!index.contains("index.md"));
        assert!(!index.contains("log.md"));
        assert!(index.contains("- [A](sources/a.md) - Body."));
        Ok(())
    }
}
