use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

const AFTER_LONG_HELP: &str = r"
HOW AN LLM AGENT SHOULD USE THIS TOOL

The wiki has three layers, all plain markdown files:

  1. Raw sources (default: raw/)     — immutable documents the LLM reads
  2. Wiki pages (default: wiki/)     — LLM-authored summaries, analyses, syntheses
  3. Schema (WIKI.md)               — conventions for the LLM maintainer

The CLI handles deterministic bookkeeping that is error-prone by hand:
index maintenance, link validation, atomic writes, and log entries.
The LLM reads raw sources directly and writes markdown pages into wiki/
with a normal file editor. After editing, the LLM calls CLI commands.

TYPICAL SESSION WORKFLOW

  1. init          — one-time setup of directories, config, index, log, schema
  2. ingest        — register each new source document (creates stub + reindexes)
  3. (LLM reads raw sources and writes/edits wiki pages directly on disk)
  4. reindex       — rebuild index.md after creating or editing pages
  5. lint          — health-check: broken links, orphans, missing index entries

When answering a user query, the LLM typically:

  1. search → find relevant pages
  2. show → read page contents as needed
  3. Read raw sources directly if deeper context is needed
  4. Synthesize an answer
  5. Optionally file the synthesis as a new wiki page and run reindex

COMMAND-LINE OUTPUT FORMATS

Most commands produce human-readable output by default.
Pass --json to get structured JSON suitable for programmatic/tool-call use:
  search --json → array of {path, title, score, snippet}
  show --json   → object  {path, title, body}
  links --json  → array of {source, target, exists}
  backlinks --json → array of {source, target, exists}

DIRECTORY LAYOUT AFTER INIT

  .llm-wiki/config.toml     — raw_dir and wiki_dir settings
  WIKI.md                   — schema: conventions for the LLM
  raw/                      — immutable source files (ingest copies here)
  wiki/
    index.md                — auto-maintained catalog of all pages
    log.md                  — append-only chronological action log
    sources/                — one stub/summary page per source
    analyses/               — syntheses, comparisons, entity pages

CORE CONTRACT

  • This tool requires .llm-wiki/config.toml to exist (created by init).
  • All commands except init MUST be run from the wiki root directory.
  • Run reindex after any manual page write edit or delete.
  • Run lint before ending a session to catch breakage.
  • raw/ files are immutable — never edit them. Read-only.
  • wiki/ files are owned by the LLM — edit them freely.
  • log.md is append-only — the CLI writes it; the LLM should not.
  • index.md is auto-generated — the CLI writes it; the LLM should not.

Search is simple token-based matching (no embeddings). At small-to-medium
scale this is sufficient. The index.md catalog provides fast overview.
";

#[derive(Parser)]
#[command(name = "llm-wiki")]
#[command(
    about = "Deterministic CLI helpers for an LLM-maintained markdown wiki",
    after_long_help = AFTER_LONG_HELP
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    #[command(
        about = "Scaffold a new wiki workspace",
        long_about = "Scaffold a new wiki workspace.\n\n\
            Creates .llm-wiki/config.toml with the given directory paths,\n\
            creates raw/ and wiki/ directories (with sources/, analyses/ subdirs),\n\
            writes initial index.md, log.md, and WIKI.md schema file.\n\
            Also runs an initial reindex.\n\n\
            Use this once at the start of a new wiki project.\n\
            If the directories already exist, new files are created but\n\
            existing files are NOT overwritten."
    )]
    Init(InitArgs),

    #[command(
        about = "Register a source document in the wiki",
        long_about = "Register a source document in the wiki.\n\n\
            Copies the source file into raw/ (immutable source storage),\n\
            then creates a stub page in wiki/sources/ with a title and\n\
            an excerpt of the source content. Rebuilds index.md and\n\
            appends an entry to log.md.\n\n\
            SIDE EFFECTS: copies source to raw/, creates wiki/sources/<slug>.md,\n\
            overwrites index.md, appends to log.md.\n\n\
            The stub page is meant to be expanded later by the LLM —\n\
            it should replace the placeholder text with a real summary\n\
            and link the page into the rest of the wiki."
    )]
    Ingest(IngestArgs),

    #[command(
        about = "Search wiki pages by token matching",
        long_about = "Search wiki pages by token matching.\n\n\
            Splits the query into alphanumeric tokens (min 2 chars) and\n\
            counts matches in each page's title + body. Returns the top N\n\
            matches sorted by score, with a content snippet.\n\n\
            Excludes index.md and log.md from results (they only catalog\n\
            or record pages, so they would match nearly every query).\n\n\
            This is a READ-ONLY operation — no files are modified.\n\n\
            For structured tool-call consumption, pass --json."
    )]
    Search(SearchArgs),

    #[command(
        about = "Display a wiki page's full content",
        long_about = "Display a wiki page's full content.\n\n\
            Resolves the page by trying (in order):\n\
              1. Exact relative path match\n\
              2. Filename stem match (with or without .md extension)\n\
              3. Title slug match\n\n\
            Returns title and full body text.\n\n\
            This is a READ-ONLY operation — no files are modified."
    )]
    Show(PageArgs),

    #[command(
        about = "List outbound links from a page",
        long_about = "List outbound links from a page.\n\n\
            Extracts both markdown links ([text](target.md)) and wikilinks\n\
            ([[target]] or [[target|display]]). For each link, reports whether\n\
            the target page exists in the wiki.\n\n\
            Broken links are marked [missing].\n\n\
            This is a READ-ONLY operation — no files are modified."
    )]
    Links(PageArgs),

    #[command(
        about = "Find all pages that link to a given page",
        long_about = "Find all pages that link to a given page.\n\n\
            Scans every wiki page (except the auto-generated index.md and\n\
            log.md) for links targeting the given page.\n\
            Useful for understanding what references a concept and for\n\
            detecting stale backlinks before renaming or deleting a page.\n\n\
            This is a READ-ONLY operation — no files are modified."
    )]
    Backlinks(PageArgs),

    #[command(
        about = "Rebuild the index from all wiki pages",
        long_about = "Rebuild index.md from all wiki pages.\n\n\
            Scans every .md file in wiki/, groups them into Sources,\n\
            Analyses, and Other sections, and writes a sorted catalog\n\
            to wiki/index.md with one link per page.\n\n\
            Automatically validates that every page made it into the\n\
            index and fails if any page was missed.\n\n\
            SIDE EFFECTS: overwrites wiki/index.md.\n\n\
            Run this after creating, editing, or deleting any wiki page\n\
            to keep the index current."
    )]
    Reindex,

    #[command(
        about = "Health-check the wiki for broken links and orphans",
        long_about = "Health-check the wiki for broken links and orphans.\n\n\
            Checks three things:\n\
              1. Broken links — any markdown/wikilink pointing to a missing page\n\
              2. Missing index entries — pages on disk not listed in index.md\n\
              3. Orphan analyses — analyses/ pages with no inbound links\n\n\
            Exits with status 0 (clean) or non-zero (issues found).\n\n\
            This is a READ-ONLY operation — no files are modified.\n\n\
            Run this before ending a session to verify wiki integrity."
    )]
    Lint,
}

#[derive(Args)]
pub struct InitArgs {
    #[arg(
        long,
        default_value = "raw",
        help = "Directory for immutable raw source files (relative to root)",
        long_help = "Directory for immutable raw source files, relative to the\n\
            wiki root. Ingest copies source files here. The LLM reads\n\
            from this directory but must never edit files in it."
    )]
    pub raw_dir: PathBuf,
    #[arg(
        long,
        default_value = "wiki",
        help = "Directory for LLM-authored wiki pages (relative to root)",
        long_help = "Directory for LLM-authored wiki pages, relative to the\n\
            wiki root. Contains index.md, log.md, sources/, and analyses/.\n\
            The LLM writes and edits pages directly inside this directory."
    )]
    pub wiki_dir: PathBuf,
}

#[derive(Args)]
pub struct IngestArgs {
    pub source: PathBuf,
    #[arg(
        long,
        help = "Title for the generated stub page (inferred from file if omitted)",
        long_help = "Title for the generated wiki stub page. If not provided,\n\
            the title is inferred from the source file: the first non-empty\n\
            line (leading '#' markers stripped), or the filename stem.\n\
            Ingest refuses to overwrite an existing raw file with different\n\
            content or an existing stub page; pass --title to pick a new name."
    )]
    pub title: Option<String>,
}

#[derive(Args)]
pub struct SearchArgs {
    pub query: String,
    #[arg(
        long,
        default_value_t = 5,
        help = "Maximum number of results to return"
    )]
    pub top: usize,
    #[arg(
        long,
        help = "Output search results as structured JSON instead of human-readable text",
        long_help = "Output results as structured JSON (array of objects with\n\
            path, title, score, and snippet fields) instead of\n\
            human-readable ranked list."
    )]
    pub json: bool,
}

#[derive(Args)]
pub struct PageArgs {
    pub page: String,
    #[arg(
        long,
        help = "Output page data as structured JSON instead of raw markdown",
        long_help = "Output page data as structured JSON (object with path,\n\
            title, and body fields) instead of raw markdown body text."
    )]
    pub json: bool,
}
