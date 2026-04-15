use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use clap::{Args, Parser, Subcommand};
use regex::Regex;
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const CONFIG_DIR: &str = ".llm-wiki";
const CONFIG_FILE: &str = ".llm-wiki/config.toml";
const SCHEMA_FILE: &str = "WIKI.md";

#[derive(Parser)]
#[command(name = "llm-wiki")]
#[command(about = "Deterministic CLI helpers for an LLM-maintained markdown wiki")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init(InitArgs),
    Ingest(IngestArgs),
    Search(SearchArgs),
    Show(PageArgs),
    Links(PageArgs),
    Backlinks(PageArgs),
    Reindex,
    Lint,
}

#[derive(Args)]
struct InitArgs {
    #[arg(long, default_value = "raw")]
    raw_dir: PathBuf,
    #[arg(long, default_value = "wiki")]
    wiki_dir: PathBuf,
}

#[derive(Args)]
struct IngestArgs {
    source: PathBuf,
    #[arg(long)]
    title: Option<String>,
}

#[derive(Args)]
struct SearchArgs {
    query: String,
    #[arg(long, default_value_t = 5)]
    top: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct PageArgs {
    page: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct Config {
    raw_dir: PathBuf,
    wiki_dir: PathBuf,
}

#[derive(Debug, Clone)]
struct WikiPaths {
    root: PathBuf,
    raw_dir: PathBuf,
    wiki_dir: PathBuf,
    sources_dir: PathBuf,
    analyses_dir: PathBuf,
    index_path: PathBuf,
    log_path: PathBuf,
    schema_path: PathBuf,
}

#[derive(Debug, Clone)]
struct Page {
    abs_path: PathBuf,
    rel_path: PathBuf,
    title: String,
    body: String,
}

#[derive(Debug, Clone, Serialize)]
struct SearchHit {
    path: String,
    title: String,
    score: usize,
    snippet: String,
}

#[derive(Debug, Clone, Serialize)]
struct PageView {
    path: String,
    title: String,
    body: String,
}

#[derive(Debug, Clone, Serialize)]
struct LinkRecord {
    source: String,
    target: String,
    exists: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init(args) => cmd_init(args),
        Commands::Ingest(args) => cmd_ingest(args),
        Commands::Search(args) => cmd_search(&args),
        Commands::Show(args) => cmd_show(&args),
        Commands::Links(args) => cmd_links(&args),
        Commands::Backlinks(args) => cmd_backlinks(&args),
        Commands::Reindex => cmd_reindex(),
        Commands::Lint => cmd_lint(),
    }
}

fn cmd_init(args: InitArgs) -> Result<()> {
    let root = std::env::current_dir().context("failed to get current directory")?;
    let config = Config {
        raw_dir: args.raw_dir,
        wiki_dir: args.wiki_dir,
    };
    let paths = wiki_paths(&root, &config);

    fs::create_dir_all(&paths.raw_dir)
        .with_context(|| format!("failed to create {}", paths.raw_dir.display()))?;
    fs::create_dir_all(&paths.sources_dir)
        .with_context(|| format!("failed to create {}", paths.sources_dir.display()))?;
    fs::create_dir_all(&paths.analyses_dir)
        .with_context(|| format!("failed to create {}", paths.analyses_dir.display()))?;
    fs::create_dir_all(root.join(CONFIG_DIR))
        .with_context(|| format!("failed to create {}", root.join(CONFIG_DIR).display()))?;

    write_if_missing(&paths.index_path, "# Index\n")?;
    write_if_missing(&paths.log_path, "# Log\n")?;
    write_if_missing(&paths.schema_path, &default_schema(&config))?;
    fs::write(root.join(CONFIG_FILE), toml::to_string_pretty(&config)?)
        .context("failed to write config")?;
    reindex(&paths)?;

    println!("Initialized llm-wiki workspace");
    println!("raw: {}", paths.raw_dir.display());
    println!("wiki: {}", paths.wiki_dir.display());
    Ok(())
}

fn cmd_ingest(args: IngestArgs) -> Result<()> {
    let (_, paths) = load_workspace()?;
    let source_path = absolute_from_root(&paths.root, &args.source);
    let source_text = fs::read_to_string(&source_path)
        .with_context(|| format!("failed to read source {}", source_path.display()))?;
    let title = args
        .title
        .unwrap_or_else(|| infer_title(&source_path, &source_text));
    let source_name = source_path
        .file_name()
        .ok_or_else(|| anyhow!("source path has no file name"))?;
    let copied_source = paths.raw_dir.join(source_name);

    if source_path != copied_source {
        fs::copy(&source_path, &copied_source).with_context(|| {
            format!(
                "failed to copy source {} to {}",
                source_path.display(),
                copied_source.display()
            )
        })?;
    }

    let slug = slugify(&title);
    let page_path = paths.sources_dir.join(format!("{slug}.md"));
    let body = source_stub_page(&title, &copied_source, &source_text);
    fs::write(&page_path, ensure_trailing_newline(&body))
        .with_context(|| format!("failed to write {}", page_path.display()))?;

    reindex(&paths)?;
    let rel_page = page_path.strip_prefix(&paths.root)?.to_path_buf();
    append_log(
        &paths.log_path,
        "ingest",
        &title,
        &format!(
            "source={} page={}",
            copied_source.display(),
            rel_page.display()
        ),
    )?;

    println!("Registered source at {}", rel_page.display());
    Ok(())
}

fn cmd_search(args: &SearchArgs) -> Result<()> {
    let (_, paths) = load_workspace()?;
    let pages = load_pages(&paths)?;
    let hits = search_pages(&pages, &args.query, args.top);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&hits)?);
        return Ok(());
    }

    for (idx, hit) in hits.iter().enumerate() {
        println!("{}. {} ({})", idx + 1, hit.title, hit.path);
        println!("   score={} {}", hit.score, hit.snippet);
    }
    Ok(())
}

fn cmd_show(args: &PageArgs) -> Result<()> {
    let (_, paths) = load_workspace()?;
    let pages = load_pages(&paths)?;
    let page = resolve_page(&paths, &pages, &args.page)?;

    if args.json {
        let view = PageView {
            path: display_rel(&page.rel_path),
            title: page.title.clone(),
            body: page.body.clone(),
        };
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        println!("{}", page.body);
    }
    Ok(())
}

fn cmd_links(args: &PageArgs) -> Result<()> {
    let (_, paths) = load_workspace()?;
    let pages = load_pages(&paths)?;
    let page = resolve_page(&paths, &pages, &args.page)?;
    let page_set: HashSet<PathBuf> = pages.iter().map(|page| page.rel_path.clone()).collect();
    let links = extract_links(&paths, page, &page_set)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&links)?);
    } else {
        for link in links {
            println!(
                "{} -> {} [{}]",
                link.source,
                link.target,
                if link.exists { "ok" } else { "missing" }
            );
        }
    }
    Ok(())
}

fn cmd_backlinks(args: &PageArgs) -> Result<()> {
    let (_, paths) = load_workspace()?;
    let pages = load_pages(&paths)?;
    let target = resolve_page(&paths, &pages, &args.page)?;
    let page_set: HashSet<PathBuf> = pages.iter().map(|page| page.rel_path.clone()).collect();
    let mut backlinks = Vec::new();

    for page in &pages {
        for link in extract_links(&paths, page, &page_set)? {
            if link.target == display_rel(&target.rel_path) {
                backlinks.push(link);
            }
        }
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&backlinks)?);
    } else {
        for link in backlinks {
            println!("{} -> {}", link.source, link.target);
        }
    }
    Ok(())
}

fn cmd_reindex() -> Result<()> {
    let (_, paths) = load_workspace()?;
    reindex(&paths)?;
    println!(
        "Rebuilt {}",
        display_rel(paths.index_path.strip_prefix(&paths.root)?)
    );
    Ok(())
}

fn cmd_lint() -> Result<()> {
    let (_, paths) = load_workspace()?;
    let issues = lint_issues(&paths)?;

    if issues.is_empty() {
        println!("Wiki lint passed.");
        Ok(())
    } else {
        for issue in &issues {
            println!("- {issue}");
        }
        bail!("wiki lint found {} issue(s)", issues.len())
    }
}

fn lint_issues(paths: &WikiPaths) -> Result<Vec<String>> {
    let pages = load_pages(paths)?;
    let page_set: HashSet<PathBuf> = pages.iter().map(|page| page.rel_path.clone()).collect();
    let indexed = indexed_pages(paths)?;
    let sources_rel = paths.sources_dir.strip_prefix(&paths.root)?.to_path_buf();
    let analyses_rel = paths.analyses_dir.strip_prefix(&paths.root)?.to_path_buf();
    let index_rel = paths.index_path.strip_prefix(&paths.root)?.to_path_buf();
    let log_rel = paths.log_path.strip_prefix(&paths.root)?.to_path_buf();
    let mut inbound: HashMap<PathBuf, usize> = HashMap::new();
    let mut issues = Vec::new();

    for page in &pages {
        if page.rel_path != index_rel
            && page.rel_path != log_rel
            && !indexed.contains(&page.rel_path)
        {
            issues.push(format!("missing index entry: {}", page.rel_path.display()));
        }

        for link in extract_links(paths, page, &page_set)? {
            let target = PathBuf::from(&link.target);
            if link.exists {
                *inbound.entry(target).or_insert(0) += 1;
            } else {
                issues.push(format!("broken link: {} -> {}", link.source, link.target));
            }
        }
    }

    for page in &pages {
        if page.rel_path == index_rel
            || page.rel_path == log_rel
            || page.rel_path.starts_with(&sources_rel)
        {
            continue;
        }
        if page.rel_path.starts_with(&analyses_rel)
            && inbound.get(&page.rel_path).copied().unwrap_or(0) == 0
        {
            issues.push(format!("orphan page: {}", page.rel_path.display()));
        }
    }

    Ok(issues)
}

fn load_workspace() -> Result<(Config, WikiPaths)> {
    let root = std::env::current_dir().context("failed to get current directory")?;
    let config_path = root.join(CONFIG_FILE);
    let config_text = fs::read_to_string(&config_path).with_context(|| {
        format!(
            "failed to read {}. Run `llm-wiki init` first.",
            config_path.display()
        )
    })?;
    let config: Config = toml::from_str(&config_text).context("failed to parse config")?;
    let paths = wiki_paths(&root, &config);
    Ok((config, paths))
}

fn wiki_paths(root: &Path, config: &Config) -> WikiPaths {
    let raw_dir = root.join(&config.raw_dir);
    let wiki_dir = root.join(&config.wiki_dir);
    WikiPaths {
        root: root.to_path_buf(),
        raw_dir,
        sources_dir: wiki_dir.join("sources"),
        analyses_dir: wiki_dir.join("analyses"),
        index_path: wiki_dir.join("index.md"),
        log_path: wiki_dir.join("log.md"),
        schema_path: root.join(SCHEMA_FILE),
        wiki_dir,
    }
}

fn default_schema(config: &Config) -> String {
    format!(
        "# Wiki Schema\n\n## Purpose\n\nThis wiki is a persistent markdown knowledge base maintained by an LLM. The CLI exists to provide deterministic filesystem, indexing, retrieval, and validation helpers.\n\n## Core Idea\n\n- Treat the wiki as a persistent, compounding artifact rather than a one-shot retrieval layer.\n- Raw sources are the source of truth; the LLM reads them and maintains synthesized wiki pages.\n- Good answers and durable analyses should usually be filed back into the wiki instead of living only in chat history.\n- The schema should stay practical and local to this repository; broader workflow guidance can live in separate source material.\n\n## Directories\n\n- Raw sources live in `{}` and are immutable.\n- Generated wiki pages live in `{}`.\n- `index.md` is the content catalog.\n- `log.md` is append-only and chronological.\n\n## CLI Contract\n\n- Use the CLI for deterministic operations: source registration, search, page lookup, backlinks, reindexing, and linting.\n- Do not use the CLI as a second reasoning model. The LLM should read pages itself and synthesize answers directly.\n- After editing wiki pages directly, run `llm-wiki reindex` when summaries or page listings may have changed.\n\n## Page Conventions\n\n- Prefer short markdown pages with descriptive titles.\n- Link related pages with markdown links.\n- Keep claims grounded in source pages when possible.\n- Save durable analyses back into the wiki rather than leaving them only in chat history.\n",
        config.raw_dir.display(),
        config.wiki_dir.display()
    )
}

fn source_stub_page(title: &str, source_path: &Path, source_text: &str) -> String {
    let excerpt = source_text.lines().take(20).collect::<Vec<_>>().join("\n");
    format!(
        "# {}\n\nSource: `{}`\n\n## Summary\n\nSource registered. This page is ready for the LLM to expand into a real summary and to link into the rest of the wiki.\n\n## Excerpt\n\n```text\n{}\n```\n",
        title,
        source_path.display(),
        excerpt.trim()
    )
}

fn reindex(paths: &WikiPaths) -> Result<()> {
    let pages = load_pages(paths)?;
    let index_rel = paths.index_path.strip_prefix(&paths.root)?.to_path_buf();
    let log_rel = paths.log_path.strip_prefix(&paths.root)?.to_path_buf();
    let sources_rel = paths.sources_dir.strip_prefix(&paths.root)?.to_path_buf();
    let analyses_rel = paths.analyses_dir.strip_prefix(&paths.root)?.to_path_buf();
    let mut sources = Vec::new();
    let mut analyses = Vec::new();
    let mut other = Vec::new();

    for page in pages {
        if page.rel_path == index_rel || page.rel_path == log_rel {
            continue;
        }
        let entry = format!(
            "- [{}]({}) - {}",
            escape_markdown_link_text(&page.title),
            index_link_path(&paths.index_path, &page.abs_path).display(),
            sanitize_summary(first_meaningful_line(&page.body).unwrap_or("wiki page"))
        );
        if page.rel_path.starts_with(&sources_rel) {
            sources.push(entry);
        } else if page.rel_path.starts_with(&analyses_rel) {
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

    let indexed = indexed_pages_from_text(paths, &index)?;
    let missing = load_pages(paths)?
        .into_iter()
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

fn write_text_atomic(path: &Path, text: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path {} has no parent directory", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("path {} has no file name", path.display()))?;
    let tmp_path = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let contents = ensure_trailing_newline(text);

    fs::write(&tmp_path, contents)
        .with_context(|| format!("failed to write temporary file {}", tmp_path.display()))?;
    File::open(&tmp_path)
        .with_context(|| format!("failed to open temporary file {}", tmp_path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync temporary file {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to replace {} with {}",
            path.display(),
            tmp_path.display()
        )
    })?;
    File::open(parent)
        .with_context(|| format!("failed to open directory {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync directory {}", parent.display()))?;
    Ok(())
}

fn load_pages(paths: &WikiPaths) -> Result<Vec<Page>> {
    let mut pages = Vec::new();
    for entry in WalkDir::new(&paths.wiki_dir)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();
        if !entry.file_type().is_file()
            || path.extension().and_then(|ext| ext.to_str()) != Some("md")
        {
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
    pages.sort_by_key(|page| page.rel_path.clone());
    Ok(pages)
}

fn search_pages(pages: &[Page], query: &str, top: usize) -> Vec<SearchHit> {
    let tokens = tokenize(query);
    let mut hits = pages
        .iter()
        .filter(|page| !page.rel_path.ends_with("log.md"))
        .filter_map(|page| {
            let haystack = format!("{}\n{}", page.title, page.body).to_lowercase();
            let score = tokens
                .iter()
                .map(|token| haystack.matches(token).count())
                .sum::<usize>();
            if score == 0 {
                return None;
            }
            Some(SearchHit {
                path: display_rel(&page.rel_path),
                title: page.title.clone(),
                score,
                snippet: sanitize_summary(first_meaningful_line(&page.body).unwrap_or("")),
            })
        })
        .collect::<Vec<_>>();
    hits.sort_by_key(|hit| (Reverse(hit.score), hit.path.clone()));
    hits.truncate(top);
    hits
}

fn resolve_page<'a>(paths: &WikiPaths, pages: &'a [Page], query: &str) -> Result<&'a Page> {
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
            page.rel_path == Path::new(query)
                || page.rel_path == Path::new(&format!("{query_norm}.md"))
                || page.abs_path == candidate
                || page
                    .rel_path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name == query_norm || name == query_slug)
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

fn extract_links(
    paths: &WikiPaths,
    page: &Page,
    page_set: &HashSet<PathBuf>,
) -> Result<Vec<LinkRecord>> {
    let markdown_link_re = Regex::new(r"\[[^\]]+\]\(([^)]+\.md)\)")?;
    let wiki_link_re = Regex::new(r"\[\[([^\]|#]+)(?:\|[^\]]+)?\]\]")?;
    let base = page.abs_path.parent().unwrap_or_else(|| Path::new(""));
    let mut links = Vec::new();

    for caps in markdown_link_re.captures_iter(&page.body) {
        if let Some(target) = caps.get(1) {
            let rel = normalize_abs_link_to_rel(
                &paths.root,
                &normalize_link_target(base, target.as_str()),
            );
            let exists = page_set.contains(&rel);
            links.push(LinkRecord {
                source: display_rel(&page.rel_path),
                target: display_rel(&rel),
                exists,
            });
        }
    }

    for caps in wiki_link_re.captures_iter(&page.body) {
        if let Some(target) = caps.get(1) {
            let rel = paths
                .sources_dir
                .join(format!("{}.md", slugify(target.as_str())))
                .strip_prefix(&paths.root)?
                .to_path_buf();
            let exists = page_set.contains(&rel);
            links.push(LinkRecord {
                source: display_rel(&page.rel_path),
                target: display_rel(&rel),
                exists,
            });
        }
    }

    Ok(links)
}

fn indexed_pages(paths: &WikiPaths) -> Result<HashSet<PathBuf>> {
    let index = fs::read_to_string(&paths.index_path)
        .with_context(|| format!("failed to read {}", paths.index_path.display()))?;
    indexed_pages_from_text(paths, &index)
}

fn indexed_pages_from_text(paths: &WikiPaths, index: &str) -> Result<HashSet<PathBuf>> {
    let link_re = Regex::new(r"\[(?:\\.|[^\\\]])+\]\(([^)]+\.md)\)")?;
    let base = paths.index_path.parent().unwrap_or_else(|| Path::new(""));
    Ok(link_re
        .captures_iter(index)
        .filter_map(|caps| caps.get(1).map(|m| normalize_link_target(base, m.as_str())))
        .map(|path| normalize_abs_link_to_rel(&paths.root, &path))
        .collect())
}

fn append_log(log_path: &Path, op: &str, title: &str, details: &str) -> Result<()> {
    let mut log = fs::read_to_string(log_path)
        .with_context(|| format!("failed to read {}", log_path.display()))?;
    let stamp = Local::now().format("%Y-%m-%d %H:%M");
    let _ = write!(log, "\n## [{stamp}] {op} | {title}\n\n{details}\n");
    fs::write(log_path, ensure_trailing_newline(&log))
        .with_context(|| format!("failed to write {}", log_path.display()))?;
    Ok(())
}

fn absolute_from_root(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn normalize_link_target(base_dir: &Path, target: &str) -> PathBuf {
    let target_path = Path::new(target);
    if target_path.is_absolute() {
        target_path.to_path_buf()
    } else {
        base_dir.join(target_path).components().collect()
    }
}

fn normalize_abs_link_to_rel(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

fn index_link_path(index_path: &Path, target_path: &Path) -> PathBuf {
    let base = index_path.parent().unwrap_or_else(|| Path::new(""));
    target_path
        .strip_prefix(base)
        .unwrap_or(target_path)
        .to_path_buf()
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

fn extract_title(path: &Path, body: &str) -> String {
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

fn infer_title(source_path: &Path, source_text: &str) -> String {
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

fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in input.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

fn tokenize(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() > 1)
        .map(str::to_lowercase)
        .collect()
}

fn sanitize_summary(summary: &str) -> String {
    summary.replace('\n', " ").trim().to_string()
}

fn escape_markdown_link_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn first_meaningful_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("# "))
}

fn display_rel(path: &Path) -> String {
    path.display().to_string()
}

fn write_if_missing(path: &Path, contents: &str) -> Result<()> {
    if !path.exists() {
        fs::write(path, ensure_trailing_newline(contents))
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(())
}

fn ensure_trailing_newline(text: &str) -> String {
    if text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn reindex_includes_root_pages_with_special_characters_in_titles() -> Result<()> {
        let root = temp_test_dir("reindex-special-title");
        let paths = test_paths(&root);

        fs::create_dir_all(&paths.wiki_dir)?;
        fs::write(&paths.index_path, "# Index\n")?;
        fs::write(&paths.log_path, "# Log\n")?;
        fs::write(
            paths.wiki_dir.join("source-security-context.md"),
            "# Source Security Context [Draft]\n\nA newly added page.\n",
        )?;

        reindex(&paths)?;

        let index = fs::read_to_string(&paths.index_path)?;
        assert!(
            index.contains(r#"[Source Security Context \[Draft\]](source-security-context.md)"#)
        );

        let indexed = indexed_pages(&paths)?;
        assert!(indexed.contains(Path::new("wiki/source-security-context.md")));

        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn lint_sees_new_page_immediately_after_reindex() -> Result<()> {
        let root = temp_test_dir("lint-after-reindex");
        let paths = test_paths(&root);

        fs::create_dir_all(&paths.wiki_dir)?;
        fs::write(&paths.index_path, "# Index\n")?;
        fs::write(&paths.log_path, "# Log\n")?;
        fs::write(
            paths.wiki_dir.join("source-foo.md"),
            "# Source Foo\n\nA newly added page.\n",
        )?;

        reindex(&paths)?;

        assert!(lint_issues(&paths)?.is_empty());
        assert!(lint_issues(&paths)?.is_empty());

        fs::remove_dir_all(&root)?;
        Ok(())
    }

    fn test_paths(root: &Path) -> WikiPaths {
        let wiki_dir = root.join("wiki");
        WikiPaths {
            root: root.to_path_buf(),
            raw_dir: root.join("raw"),
            wiki_dir: wiki_dir.clone(),
            sources_dir: wiki_dir.join("sources"),
            analyses_dir: wiki_dir.join("analyses"),
            index_path: wiki_dir.join("index.md"),
            log_path: wiki_dir.join("log.md"),
            schema_path: root.join(SCHEMA_FILE),
        }
    }

    fn temp_test_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("llm-wiki-{prefix}-{unique}"))
    }
}
