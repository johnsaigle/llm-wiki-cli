# llm-wiki-cli

A small, deterministic CLI that gives an LLM agent the filesystem primitives it needs to build and maintain a persistent markdown wiki. Inspired by [Andrej Karpathy's LLM Wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f).

> **The core idea:** Instead of retrieving raw documents from scratch on every query, the LLM incrementally builds a persistent, interlinked knowledge base. The wiki becomes a compounding artifact — cross-references are already there, contradictions have already been flagged, and synthesis reflects everything you've read.

This tool is designed to be invoked **directly by an LLM agent** via a tool call, MCP server, or simple shell execution — not by a human typing commands.

## Why this exists

Karpathy's gist describes a pattern where the LLM maintains three layers:

1. **Raw sources** — immutable source documents
2. **The wiki** — LLM-generated markdown pages, summaries, and syntheses
3. **The schema** — conventions and workflows that keep the LLM disciplined

The problem: an LLM agent editing markdown directly is error-prone. Links break. The index gets stale. Pages become orphans. Files are written non-atomically. This CLI provides deterministic, testable primitives so the agent can focus on reasoning while the tool handles bookkeeping.

## Installation

```bash
cargo install --path .
```

Or copy the binary to a location on your agent's `PATH`.

## Commands

| Command | Purpose | Typical agent use |
|---------|---------|-----------------|
| `llm-wiki init` | Scaffold a new wiki workspace with `raw/`, `wiki/sources/`, `wiki/analyses/`, `index.md`, `log.md`, and a `WIKI.md` schema | "Set up the wiki structure for this project" |
| `llm-wiki ingest <source> --title "..."` | Copy a source into `raw/`, create a stub page in `wiki/sources/`, and reindex | "Register this paper/article as a source" |
| `llm-wiki search <query> --top 5` | Token-based search across all wiki pages (excludes `log.md`) | "Find pages about Byzantine fault tolerance" |
| `llm-wiki show <page>` | Read a full page by path, filename, slug, or title | "Read the `consensus-mechanisms.md` page" |
| `llm-wiki links <page>` | List all outbound markdown/wikilink-style links from a page, noting broken ones | "Check what this page links to" |
| `llm-wiki backlinks <page>` | Find all pages that link to a given page | "See which pages reference this concept" |
| `llm-wiki reindex` | Rebuild `wiki/index.md` from all pages; verifies every page is indexed | "Update the index after creating new pages" |
| `llm-wiki lint` | Check for broken links, orphan pages, and missing index entries | "Health-check the wiki before ending the session" |

All commands support a `--json` flag where applicable, making them ideal for structured tool-call responses.

## Directory layout

After `llm-wiki init`:

```
.
├── .llm-wiki/
│   └── config.toml          # raw_dir and wiki_dir paths
├── WIKI.md                  # Schema: conventions for the LLM maintainer
├── raw/                     # Immutable source documents
│   └── article.pdf.md       # (example) converted source
└── wiki/
    ├── index.md             # Content catalog, auto-maintained
    ├── log.md               # Append-only chronological log
    ├── sources/
    │   └── article.md       # Stub or summary per source
    └── analyses/
        └── my-analysis.md   # Syntheses, comparisons, entity pages
```

## LLM agent integration

The intended operator is an LLM agent with the ability to run this CLI as a tool. For example, in an OpenCode or Claude Code context, you might expose:

- `llm_wiki_ingest(source_path, title)` — registers a source
- `llm_wiki_search(query, top_n)` — finds relevant pages
- `llm_wiki_show(page)` — reads page contents
- `llm_wiki_reindex()` — updates the index
- `llm_wiki_lint()` — checks wiki health

The agent reads raw sources itself, writes markdown pages directly into the filesystem, and uses the CLI for deterministic operations that are easy to get wrong by hand: indexing, link validation, logging, and atomic file writes.

### Key contract

- **Raw sources are immutable.** The agent reads them but never edits them.
- **The agent owns the wiki layer.** It creates pages, updates cross-references, and files syntheses.
- **The CLI owns bookkeeping.** After the agent edits pages, it calls `reindex`. Before ending a session, it calls `lint`.
- **Good answers get filed back.** An analysis, comparison, or connection discovered during a query should be saved as a new wiki page rather than left in chat history.

## Design choices

- **Deterministic & stateless:** The CLI reads the filesystem, does what you asked, and exits. No background server, no database, no hidden state.
- **Atomic writes:** File updates use write-to-temp + fsync + rename to avoid half-written files.
- **No embeddings, no vector DB:** At small-to-medium scale, the `index.md` catalog plus token-based search is enough. If you outgrow it, swap in [qmd](https://github.com/tobi/qmd) or similar.
- **Plain markdown:** The wiki is just a git repo of markdown files. Use Obsidian, VS Code, or any text editor to browse it.

## Inspiration

This tool implements the concrete filesystem layer for the pattern described in:

- **[LLM Wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)** by Andrej Karpathy

> "The idea here is different. Instead of just retrieving from raw documents at query time, the LLM incrementally builds and maintains a persistent wiki — a structured, interlinked collection of markdown files that sits between you and the raw sources."

## License

MIT
