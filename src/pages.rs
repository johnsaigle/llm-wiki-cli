use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use walkdir::WalkDir;

use crate::config::WikiPaths;
use crate::fsutil::{
    absolute_from_root, display_rel, normalize_abs_link_to_rel, normalize_link_target,
};
use crate::text::{
    extract_title, first_meaningful_line, sanitize_summary, slugify, tokenize, truncate_chars,
};

const SNIPPET_MAX_CHARS: usize = 200;

static MARKDOWN_LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[[^\]]+\]\(([^)#]+\.md)(?:#[^)]*)?\)").expect("markdown link regex is valid")
});

static WIKI_LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[\[([^\]|#]+)(?:#[^\]|]*)?(?:\|[^\]]+)?\]\]").expect("wikilink regex is valid")
});

#[derive(Debug, Clone)]
pub struct Page {
    pub abs_path: PathBuf,
    pub rel_path: PathBuf,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    pub score: usize,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageView {
    pub path: String,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkRecord {
    pub source: String,
    pub target: String,
    pub exists: bool,
}

pub fn load_pages(paths: &WikiPaths) -> Result<Vec<Page>> {
    let mut pages = Vec::new();
    for entry in WalkDir::new(&paths.wiki_dir) {
        let entry =
            entry.with_context(|| format!("failed to walk {}", paths.wiki_dir.display()))?;
        let path = entry.path();
        if !entry.file_type().is_file()
            || path.extension().and_then(|ext| ext.to_str()) != Some("md")
        {
            continue;
        }
        if path.starts_with(&paths.raw_dir) || path == paths.schema_path.as_path() {
            continue;
        }
        let body = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        pages.push(Page {
            abs_path: path.to_path_buf(),
            rel_path: path.strip_prefix(&paths.root)?.to_path_buf(),
            title: extract_title(path, &body),
            body,
        });
    }
    pages.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(pages)
}

pub fn search_pages(
    pages: &[Page],
    query: &str,
    top: usize,
    excluded: &[&Path],
) -> Result<Vec<SearchHit>> {
    let tokens = tokenize(query);
    if tokens.is_empty() {
        bail!("query `{query}` has no searchable tokens (need words of at least 2 letters/digits)");
    }
    let mut hits = pages
        .iter()
        .filter(|page| !excluded.contains(&page.rel_path.as_path()))
        .filter_map(|page| {
            let haystack = format!("{}\n{}", page.title, page.body).to_lowercase();
            let score = tokens
                .iter()
                .map(|token| haystack.matches(token.as_str()).count())
                .sum::<usize>();
            if score == 0 {
                return None;
            }
            Some(SearchHit {
                path: display_rel(&page.rel_path),
                title: page.title.clone(),
                score,
                snippet: snippet_for(&page.body, &tokens),
            })
        })
        .collect::<Vec<_>>();
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    hits.truncate(top);
    Ok(hits)
}

/// The first body line mentioning a query token, falling back to the first meaningful line.
fn snippet_for(body: &str, tokens: &[String]) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("# "))
        .find(|line| {
            let lowered = line.to_lowercase();
            tokens.iter().any(|token| lowered.contains(token.as_str()))
        })
        .or_else(|| first_meaningful_line(body))
        .unwrap_or("");
    truncate_chars(&sanitize_summary(line), SNIPPET_MAX_CHARS)
}

pub fn resolve_page<'a>(paths: &WikiPaths, pages: &'a [Page], query: &str) -> Result<&'a Page> {
    let candidate = absolute_from_root(&paths.root, Path::new(query));
    if let Ok(rel) = candidate.strip_prefix(&paths.root)
        && let Some(page) = pages.iter().find(|page| page.rel_path == rel)
    {
        return Ok(page);
    }

    let query_norm = query.trim_end_matches(".md");
    let query_slug = slugify(query_norm);
    let mut matches = pages
        .iter()
        .filter(|page| {
            page.rel_path == Path::new(&format!("{query_norm}.md"))
                || page
                    .rel_path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| slugify(name) == query_slug)
                || slugify(&page.title) == query_slug
        })
        .collect::<Vec<_>>();

    matches.sort_by_key(|page| page.rel_path.clone());
    match matches.len() {
        0 => bail!("no page matched `{query}`"),
        1 => Ok(matches[0]),
        _ => {
            let names = matches
                .iter()
                .map(|page| page.rel_path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bail!("page `{query}` is ambiguous: {names}")
        }
    }
}

/// Lookup tables over the loaded pages, used to resolve link targets.
pub struct PageIndex {
    rel_paths: HashSet<PathBuf>,
    by_slug: HashMap<String, PathBuf>,
}

impl PageIndex {
    pub fn new(pages: &[Page]) -> Self {
        let mut by_slug = HashMap::new();
        for page in pages {
            if let Some(stem) = page.rel_path.file_stem().and_then(|stem| stem.to_str()) {
                // Pages are sorted by path, so the first page with a given slug wins.
                by_slug
                    .entry(slugify(stem))
                    .or_insert_with(|| page.rel_path.clone());
            }
        }
        Self {
            rel_paths: pages.iter().map(|page| page.rel_path.clone()).collect(),
            by_slug,
        }
    }
}

fn is_external_target(target: &str) -> bool {
    target.contains("://") || target.starts_with("mailto:")
}

pub fn extract_links(paths: &WikiPaths, page: &Page, index: &PageIndex) -> Vec<LinkRecord> {
    let base = page.abs_path.parent().unwrap_or_else(|| Path::new(""));
    let source = display_rel(&page.rel_path);
    let mut links = Vec::new();

    for caps in MARKDOWN_LINK_RE.captures_iter(&page.body) {
        let target = caps[1].trim();
        if is_external_target(target) {
            continue;
        }
        let rel = normalize_abs_link_to_rel(&paths.root, &normalize_link_target(base, target));
        links.push(LinkRecord {
            source: source.clone(),
            exists: index.rel_paths.contains(&rel),
            target: display_rel(&rel),
        });
    }

    for caps in WIKI_LINK_RE.captures_iter(&page.body) {
        let slug = slugify(&caps[1]);
        // Prefer the conventional sources/ location, then any page with a matching name.
        let in_sources = paths.sources_dir.join(format!("{slug}.md"));
        let in_sources = normalize_abs_link_to_rel(&paths.root, &in_sources);
        let (rel, exists) = if index.rel_paths.contains(&in_sources) {
            (in_sources, true)
        } else if let Some(found) = index.by_slug.get(&slug) {
            (found.clone(), true)
        } else {
            (in_sources, false)
        };
        links.push(LinkRecord {
            source: source.clone(),
            target: display_rel(&rel),
            exists,
        });
    }

    links
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    fn page(rel: &str, body: &str) -> Page {
        Page {
            abs_path: Path::new("/root").join(rel),
            rel_path: PathBuf::from(rel),
            title: extract_title(Path::new(rel), body),
            body: body.to_string(),
        }
    }

    fn links_of(paths: &WikiPaths, rel: &str) -> Result<Vec<LinkRecord>> {
        let pages = load_pages(paths)?;
        let index = PageIndex::new(&pages);
        let page = pages
            .iter()
            .find(|page| page.rel_path == Path::new(rel))
            .expect("page exists");
        Ok(extract_links(paths, page, &index))
    }

    #[test]
    fn links_ignore_external_urls_and_strip_anchors() -> Result<()> {
        let (_tmp, paths) = workspace("links-external")?;
        fs::write(paths.analyses_dir.join("b.md"), "# B\n")?;
        fs::write(
            paths.analyses_dir.join("a.md"),
            "# A\n\n[web](https://example.com/page.md) [mail](mailto:x@y.md) \
             [anchor](b.md#section) [dot](./../analyses/b.md)\n",
        )?;

        let links = links_of(&paths, "wiki/analyses/a.md")?;
        let targets: Vec<_> = links
            .iter()
            .map(|link| (link.target.as_str(), link.exists))
            .collect();
        assert_eq!(
            targets,
            vec![("wiki/analyses/b.md", true), ("wiki/analyses/b.md", true)]
        );
        Ok(())
    }

    #[test]
    fn wikilinks_resolve_to_sources_first_then_any_page_by_name() -> Result<()> {
        let (_tmp, paths) = workspace("wikilinks")?;
        fs::write(paths.sources_dir.join("topic.md"), "# Topic\n")?;
        fs::write(paths.analyses_dir.join("topic.md"), "# Topic Analysis\n")?;
        fs::write(paths.analyses_dir.join("deep-dive.md"), "# Deep Dive\n")?;
        fs::write(
            paths.wiki_dir.join("hub.md"),
            "# Hub\n\n[[Topic]] [[Deep Dive|the dive]] [[deep-dive#part]] [[Nope]]\n",
        )?;

        let links = links_of(&paths, "wiki/hub.md")?;
        let got: Vec<_> = links
            .iter()
            .map(|link| (link.target.as_str(), link.exists))
            .collect();
        assert_eq!(
            got,
            vec![
                ("wiki/sources/topic.md", true),
                ("wiki/analyses/deep-dive.md", true),
                ("wiki/analyses/deep-dive.md", true),
                ("wiki/sources/nope.md", false),
            ]
        );
        Ok(())
    }

    #[test]
    fn backlinks_are_found_via_resolved_targets() -> Result<()> {
        let (_tmp, paths) = workspace("backlinks")?;
        fs::write(paths.analyses_dir.join("a.md"), "# A\n\n[b](b.md)\n")?;
        fs::write(paths.analyses_dir.join("b.md"), "# B\n")?;
        let pages = load_pages(&paths)?;
        let index = PageIndex::new(&pages);

        let hits: Vec<_> = pages
            .iter()
            .flat_map(|page| extract_links(&paths, page, &index))
            .filter(|link| link.target == "wiki/analyses/b.md")
            .map(|link| link.source)
            .collect();
        assert_eq!(hits, vec!["wiki/analyses/a.md"]);
        Ok(())
    }

    #[test]
    fn search_ranks_by_score_then_path_and_excludes_index_and_log() -> Result<()> {
        let pages = vec![
            page("wiki/b.md", "# B\n\nraft raft raft\n"),
            page("wiki/a.md", "# A\n\nraft once\n"),
            page("wiki/c.md", "# C\n\nraft once\n"),
            page("wiki/log.md", "# Log\n\nraft raft raft raft raft\n"),
            page(
                "wiki/index.md",
                "# Index\n\nraft raft raft raft raft raft\n",
            ),
            page("wiki/d.md", "# D\n\nunrelated\n"),
        ];

        let hits = search_pages(
            &pages,
            "Raft",
            10,
            &[Path::new("wiki/index.md"), Path::new("wiki/log.md")],
        )?;

        let order: Vec<_> = hits.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(order, vec!["wiki/b.md", "wiki/a.md", "wiki/c.md"]);
        assert_eq!(hits[0].score, 3);
        assert_eq!(
            search_pages(
                &pages,
                "raft",
                2,
                &[Path::new("wiki/index.md"), Path::new("wiki/log.md")]
            )?
            .len(),
            2
        );
        Ok(())
    }

    #[test]
    fn search_counts_title_and_ignores_duplicate_query_tokens() -> Result<()> {
        let pages = vec![page("wiki/a.md", "# Paxos\n\nbody\n")];
        let once = search_pages(
            &pages,
            "paxos",
            5,
            &[Path::new("wiki/index.md"), Path::new("wiki/log.md")],
        )?;
        let twice = search_pages(
            &pages,
            "paxos paxos",
            5,
            &[Path::new("wiki/index.md"), Path::new("wiki/log.md")],
        )?;
        // Title and the `# ` heading in the body both count.
        assert_eq!(once[0].score, 2);
        assert_eq!(twice[0].score, once[0].score);
        Ok(())
    }

    #[test]
    fn search_snippet_prefers_a_line_that_matches_the_query() -> Result<()> {
        let pages = vec![page(
            "wiki/a.md",
            "# A\n\nIntro line.\n\nThe quorum is a majority.\n",
        )];
        let hits = search_pages(
            &pages,
            "quorum",
            5,
            &[Path::new("wiki/index.md"), Path::new("wiki/log.md")],
        )?;
        assert_eq!(hits[0].snippet, "The quorum is a majority.");
        Ok(())
    }

    #[test]
    fn search_rejects_queries_without_tokens() {
        let pages = vec![page("wiki/a.md", "# A\n")];
        assert!(
            search_pages(
                &pages,
                "a ! ?",
                5,
                &[Path::new("wiki/index.md"), Path::new("wiki/log.md")]
            )
            .is_err()
        );
    }

    #[test]
    fn search_handles_non_ascii_queries() -> Result<()> {
        let pages = vec![page("wiki/a.md", "# Заметка\n\nПривет мир\n")];
        let hits = search_pages(
            &pages,
            "привет",
            5,
            &[Path::new("wiki/index.md"), Path::new("wiki/log.md")],
        )?;
        assert_eq!(hits.len(), 1);
        Ok(())
    }

    #[test]
    fn resolve_page_matches_path_stem_title_and_reports_ambiguity() -> Result<()> {
        let tmp = TempDir::new("resolve");
        let paths = test_paths(&tmp.0);
        let pages = vec![
            page("wiki/analyses/consensus.md", "# Consensus Mechanisms\n"),
            page("wiki/sources/consensus.md", "# Consensus Source\n"),
            page("wiki/sources/unique.md", "# Unique Thing\n"),
        ];

        assert_eq!(
            resolve_page(&paths, &pages, "wiki/analyses/consensus.md")?.rel_path,
            Path::new("wiki/analyses/consensus.md")
        );
        assert_eq!(
            resolve_page(&paths, &pages, "unique")?.title,
            "Unique Thing"
        );
        assert_eq!(
            resolve_page(&paths, &pages, "unique.md")?.title,
            "Unique Thing"
        );
        assert_eq!(
            resolve_page(&paths, &pages, "Unique Thing")?.title,
            "Unique Thing"
        );
        let ambiguous = resolve_page(&paths, &pages, "consensus").unwrap_err();
        assert!(ambiguous.to_string().contains("ambiguous"));
        let missing = resolve_page(&paths, &pages, "nope").unwrap_err();
        assert!(missing.to_string().contains("no page matched"));
        Ok(())
    }

    #[test]
    fn resolve_page_accepts_extensionless_paths_and_unslugged_stems() -> Result<()> {
        let tmp = TempDir::new("resolve-more");
        let paths = test_paths(&tmp.0);
        let pages = vec![
            page("wiki/sources/foo.md", "# Something Else\n"),
            page("wiki/sources/my_file.md", "# Other\n"),
        ];

        assert_eq!(
            resolve_page(&paths, &pages, "wiki/sources/foo")?.rel_path,
            Path::new("wiki/sources/foo.md")
        );
        assert_eq!(
            resolve_page(&paths, &pages, "my_file")?.rel_path,
            Path::new("wiki/sources/my_file.md")
        );
        assert_eq!(
            resolve_page(&paths, &pages, "My File")?.rel_path,
            Path::new("wiki/sources/my_file.md"),
            "title/slug lookups also work"
        );
        Ok(())
    }

    #[test]
    fn load_pages_only_reads_markdown_files() -> Result<()> {
        let (_tmp, paths) = workspace("load-pages-md")?;
        fs::write(paths.wiki_dir.join("notes.txt"), [0xff, 0xfe, 0x00])?;
        fs::create_dir_all(paths.wiki_dir.join("dir.md"))?;
        fs::write(paths.wiki_dir.join("page.md"), "# Page\n")?;

        let rels: Vec<_> = load_pages(&paths)?
            .into_iter()
            .map(|page| page.rel_path)
            .collect();

        assert_eq!(
            rels,
            vec![
                PathBuf::from("wiki/index.md"),
                PathBuf::from("wiki/log.md"),
                PathBuf::from("wiki/page.md"),
            ]
        );
        Ok(())
    }

    #[test]
    fn snippet_skips_headings_even_when_they_match_the_query() {
        let tokens = vec!["quorum".to_string()];
        assert_eq!(
            snippet_for("# Quorum\n\nfirst line\nthe Quorum rules\n", &tokens),
            "the Quorum rules"
        );
        assert_eq!(
            snippet_for("# T\n\nno match here\n", &tokens),
            "no match here"
        );
        assert_eq!(snippet_for("# T\n", &tokens), "");
    }
}
