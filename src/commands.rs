use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::cli::{Commands, IngestArgs, InitArgs, JsonArgs, PageArgs, SearchArgs};
use crate::config::{
    CONFIG_DIR, CONFIG_FILE, Config, default_schema, load_workspace, read_config, validate_config,
    wiki_paths,
};
use crate::fsutil::{display_rel, write_if_missing, write_text_atomic};
use crate::index::{lint_issues, reindex};
use crate::ingest::ingest;
use crate::pages::{PageIndex, PageView, extract_links, load_pages, resolve_page, search_pages};
use crate::text::ensure_trailing_newline;

/// Runs one command against the wiki rooted at `root`, writing results to `out`.
pub fn run(command: Commands, root: &Path, out: &mut impl Write) -> Result<()> {
    match command {
        Commands::Init(args) => cmd_init(args, root, out),
        Commands::Ingest(args) => cmd_ingest(args, root, out),
        Commands::Search(args) => cmd_search(&args, root, out),
        Commands::Show(args) => cmd_show(&args, root, out),
        Commands::Links(args) => cmd_links(&args, root, out),
        Commands::Backlinks(args) => cmd_backlinks(&args, root, out),
        Commands::List(args) => cmd_list(&args, root, out),
        Commands::Reindex(args) => cmd_reindex(&args, root, out),
        Commands::Lint(args) => cmd_lint(&args, root, out),
    }
}

fn cmd_init(args: InitArgs, root: &Path, out: &mut impl Write) -> Result<()> {
    let config = Config {
        raw_dir: args.raw_dir,
        wiki_dir: args.wiki_dir,
    };
    validate_config(&config)?;
    let config_path = root.join(CONFIG_FILE);
    if config_path.exists() {
        let existing = read_config(&config_path)?;
        if existing.raw_dir != config.raw_dir || existing.wiki_dir != config.wiki_dir {
            bail!(
                "already initialized with raw_dir={} wiki_dir={}; refusing to change settings \
                 (edit {} by hand if intended)",
                existing.raw_dir.display(),
                existing.wiki_dir.display(),
                config_path.display()
            );
        }
    }
    let paths = wiki_paths(root, &config);

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
    write_text_atomic(&config_path, &toml::to_string_pretty(&config)?)
        .context("failed to write config")?;
    reindex(&paths)?;

    writeln!(out, "Initialized llm-wiki workspace")?;
    writeln!(out, "raw: {}", paths.raw_dir.display())?;
    writeln!(out, "wiki: {}", paths.wiki_dir.display())?;
    Ok(())
}

fn cmd_ingest(args: IngestArgs, root: &Path, out: &mut impl Write) -> Result<()> {
    let (_, paths) = load_workspace(root)?;
    let rel_page = ingest(&paths, &args.source, args.title)?;
    writeln!(out, "Registered source at {}", rel_page.display())?;
    Ok(())
}

fn cmd_search(args: &SearchArgs, root: &Path, out: &mut impl Write) -> Result<()> {
    let (_, paths) = load_workspace(root)?;
    let pages = load_pages(&paths)?;
    let excluded = [
        paths.index_path.strip_prefix(&paths.root)?,
        paths.log_path.strip_prefix(&paths.root)?,
    ];
    let hits = search_pages(&pages, &args.query, args.top, &excluded)?;

    if args.json {
        writeln!(out, "{}", serde_json::to_string_pretty(&hits)?)?;
        return Ok(());
    }

    for (idx, hit) in hits.iter().enumerate() {
        writeln!(out, "{}. {} ({})", idx + 1, hit.title, hit.path)?;
        writeln!(out, "   score={} {}", hit.score, hit.snippet)?;
    }
    Ok(())
}

fn cmd_show(args: &PageArgs, root: &Path, out: &mut impl Write) -> Result<()> {
    let (_, paths) = load_workspace(root)?;
    let pages = load_pages(&paths)?;
    let page = resolve_page(&paths, &pages, &args.page)?;

    if args.json {
        let view = PageView {
            path: display_rel(&page.rel_path),
            title: page.title.clone(),
            body: page.body.clone(),
        };
        writeln!(out, "{}", serde_json::to_string_pretty(&view)?)?;
    } else {
        write!(out, "{}", ensure_trailing_newline(&page.body))?;
    }
    Ok(())
}

fn cmd_links(args: &PageArgs, root: &Path, out: &mut impl Write) -> Result<()> {
    let (_, paths) = load_workspace(root)?;
    let pages = load_pages(&paths)?;
    let page = resolve_page(&paths, &pages, &args.page)?;
    let page_index = PageIndex::new(&pages);
    let links = extract_links(&paths, page, &page_index);

    if args.json {
        writeln!(out, "{}", serde_json::to_string_pretty(&links)?)?;
    } else {
        for link in links {
            writeln!(
                out,
                "{} -> {} [{}]",
                link.source,
                link.target,
                if link.exists { "ok" } else { "missing" }
            )?;
        }
    }
    Ok(())
}

fn cmd_backlinks(args: &PageArgs, root: &Path, out: &mut impl Write) -> Result<()> {
    let (_, paths) = load_workspace(root)?;
    let pages = load_pages(&paths)?;
    let target = resolve_page(&paths, &pages, &args.page)?;
    let page_index = PageIndex::new(&pages);
    let target_rel = display_rel(&target.rel_path);
    let index_rel = paths.index_path.strip_prefix(&paths.root)?;
    let log_rel = paths.log_path.strip_prefix(&paths.root)?;
    let backlinks = pages
        .iter()
        .filter(|page| page.rel_path != index_rel && page.rel_path != log_rel)
        .flat_map(|page| extract_links(&paths, page, &page_index))
        .filter(|link| link.target == target_rel)
        .collect::<Vec<_>>();

    if args.json {
        writeln!(out, "{}", serde_json::to_string_pretty(&backlinks)?)?;
    } else {
        for link in backlinks {
            writeln!(out, "{} -> {}", link.source, link.target)?;
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct PageSummary {
    path: String,
    title: String,
}

fn cmd_list(args: &JsonArgs, root: &Path, out: &mut impl Write) -> Result<()> {
    let (_, paths) = load_workspace(root)?;
    let summaries = load_pages(&paths)?
        .into_iter()
        .map(|page| PageSummary {
            path: display_rel(&page.rel_path),
            title: page.title,
        })
        .collect::<Vec<_>>();

    if args.json {
        writeln!(out, "{}", serde_json::to_string_pretty(&summaries)?)?;
    } else {
        for summary in summaries {
            writeln!(out, "{}  {}", summary.path, summary.title)?;
        }
    }
    Ok(())
}

fn cmd_reindex(args: &JsonArgs, root: &Path, out: &mut impl Write) -> Result<()> {
    let (_, paths) = load_workspace(root)?;
    let summary = reindex(&paths)?;

    if args.json {
        writeln!(out, "{}", serde_json::to_string_pretty(&summary)?)?;
    } else {
        writeln!(
            out,
            "Rebuilt {} ({} pages indexed)",
            display_rel(paths.index_path.strip_prefix(&paths.root)?),
            summary.pages_indexed
        )?;
    }
    Ok(())
}

#[derive(Serialize)]
struct LintReport {
    passed: bool,
    issues: Vec<String>,
}

fn cmd_lint(args: &JsonArgs, root: &Path, out: &mut impl Write) -> Result<()> {
    let (_, paths) = load_workspace(root)?;
    let issues = lint_issues(&paths)?;

    if args.json {
        let report = LintReport {
            passed: issues.is_empty(),
            issues,
        };
        writeln!(out, "{}", serde_json::to_string_pretty(&report)?)?;
        Ok(())
    } else if issues.is_empty() {
        writeln!(out, "Wiki lint passed.")?;
        Ok(())
    } else {
        for issue in &issues {
            writeln!(out, "- {issue}")?;
        }
        bail!("wiki lint found {} issue(s)", issues.len())
    }
}

#[cfg(test)]
mod tests {
    //! End-to-end tests that drive `run` the way an agent drives the CLI.

    use super::*;
    use crate::cli::Cli;
    use crate::test_support::TempDir;
    use clap::Parser;
    use std::path::PathBuf;

    struct Workspace {
        tmp: TempDir,
    }

    impl Workspace {
        fn new() -> Self {
            Self {
                tmp: TempDir::new("commands"),
            }
        }

        /// Runs `llm-wiki <args>`; returns stdout, or stdout plus the error chain on failure.
        fn run(&self, args: &[&str]) -> std::result::Result<String, String> {
            let cli = Cli::try_parse_from(std::iter::once("llm-wiki").chain(args.iter().copied()))
                .unwrap_or_else(|err| panic!("bad args {args:?}: {err}"));
            let mut out = Vec::new();
            let result = run(cli.command, &self.tmp.0, &mut out);
            let stdout = String::from_utf8(out).expect("utf-8 output");
            match result {
                Ok(()) => Ok(stdout),
                Err(err) => Err(format!("{stdout}{err:#}")),
            }
        }

        fn ok(&self, args: &[&str]) -> String {
            self.run(args)
                .unwrap_or_else(|err| panic!("`{}` failed: {err}", args.join(" ")))
        }

        fn fail(&self, args: &[&str]) -> String {
            match self.run(args) {
                Ok(out) => panic!("`{}` unexpectedly succeeded: {out}", args.join(" ")),
                Err(err) => err,
            }
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.tmp.0.join(rel)
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.path(rel);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(path, text).expect("write file");
        }

        fn read(&self, rel: &str) -> String {
            fs::read_to_string(self.path(rel)).expect("read file")
        }
    }

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("valid json")
    }

    #[test]
    fn commands_require_an_initialized_workspace() {
        let ws = Workspace::new();
        for cmd in [
            vec!["lint"],
            vec!["reindex"],
            vec!["search", "foo"],
            vec!["show", "x"],
        ] {
            let msg = ws.fail(&cmd);
            assert!(msg.contains("llm-wiki init"), "{cmd:?}: {msg}");
        }
    }

    #[test]
    fn init_scaffolds_workspace_and_is_idempotent() {
        let ws = Workspace::new();
        assert!(ws.ok(&["init"]).contains("Initialized llm-wiki workspace"));

        for dir in ["raw", "wiki/sources", "wiki/analyses", ".llm-wiki"] {
            assert!(ws.path(dir).is_dir(), "{dir} missing");
        }
        assert!(ws.read("WIKI.md").starts_with("# Wiki Schema"));
        assert!(ws.read("WIKI.md").contains("`raw`"));
        assert_eq!(ws.read("wiki/log.md"), "# Log\n");

        ws.write("WIKI.md", "# My own schema\n");
        ws.ok(&["init"]);
        assert_eq!(
            ws.read("WIKI.md"),
            "# My own schema\n",
            "existing files are kept"
        );
    }

    #[test]
    fn init_refuses_to_change_settings_or_use_unsafe_dirs() {
        let ws = Workspace::new();
        ws.ok(&["init", "--raw-dir", "src-raw", "--wiki-dir", "notes"]);
        ws.ok(&["init", "--raw-dir", "src-raw", "--wiki-dir", "notes"]);

        let changed_raw = ws.fail(&["init", "--raw-dir", "other", "--wiki-dir", "notes"]);
        assert!(changed_raw.contains("already initialized"), "{changed_raw}");
        let changed_wiki = ws.fail(&["init", "--raw-dir", "src-raw", "--wiki-dir", "other"]);
        assert!(
            changed_wiki.contains("already initialized"),
            "{changed_wiki}"
        );

        let fresh = Workspace::new();
        assert!(
            fresh
                .fail(&["init", "--raw-dir", "/etc"])
                .contains("relative path")
        );
        assert!(
            fresh
                .fail(&["init", "--wiki-dir", "../up"])
                .contains("relative path")
        );
        assert!(
            fresh
                .fail(&["init", "--raw-dir", "x", "--wiki-dir", "x"])
                .contains("different")
        );
        assert!(
            !fresh.path(".llm-wiki").exists(),
            "nothing created on invalid input"
        );
    }

    #[test]
    fn full_agent_workflow() {
        let ws = Workspace::new();
        ws.ok(&["init"]);
        ws.write(
            "paper.md",
            "# Raft Paper\n\nLeaders replicate logs to followers.\n",
        );

        let out = ws.ok(&["ingest", "paper.md"]);
        assert!(out.contains("wiki/sources/raft-paper.md"), "{out}");
        assert_eq!(
            ws.read("raw/paper.md"),
            "# Raft Paper\n\nLeaders replicate logs to followers.\n"
        );
        assert!(ws.read("wiki/log.md").contains("ingest | Raft Paper"));
        assert!(ws.read("wiki/index.md").contains("(sources/raft-paper.md)"));

        // The LLM writes an analysis that links to the source.
        ws.write(
            "wiki/analyses/replication.md",
            "# Replication\n\nBuilds on [the paper](../sources/raft-paper.md).\n",
        );
        assert!(
            ws.fail(&["lint"])
                .contains("missing index entry: wiki/analyses/replication.md")
        );
        assert!(ws.ok(&["reindex"]).contains("Rebuilt wiki/index.md"));
        // Analysis is not linked from anywhere yet.
        assert!(
            ws.fail(&["lint"])
                .contains("orphan page: wiki/analyses/replication.md")
        );
        ws.write(
            "wiki/sources/raft-paper.md",
            "# Raft Paper\n\nSummary. See [[replication]].\n",
        );
        ws.ok(&["reindex"]);
        assert!(ws.ok(&["lint"]).contains("Wiki lint passed."));

        // search (text + json)
        let text = ws.ok(&["search", "builds"]);
        assert!(text.starts_with("1. "), "{text}");
        let hits = json(&ws.ok(&["search", "replication", "--json", "--top", "1"]));
        assert_eq!(hits.as_array().expect("array").len(), 1);
        assert_eq!(hits[0]["path"], "wiki/analyses/replication.md");
        assert!(hits[0]["score"].as_u64().expect("score") > 0);

        // show (text + json)
        assert!(ws.ok(&["show", "replication"]).contains("Builds on"));
        let shown = json(&ws.ok(&["show", "Raft Paper", "--json"]));
        assert_eq!(shown["title"], "Raft Paper");
        assert_eq!(shown["path"], "wiki/sources/raft-paper.md");

        // links / backlinks
        let links = ws.ok(&["links", "replication"]);
        assert_eq!(
            links.trim(),
            "wiki/analyses/replication.md -> wiki/sources/raft-paper.md [ok]"
        );
        let back = ws.ok(&["backlinks", "wiki/analyses/replication.md"]);
        assert_eq!(
            back.trim(),
            "wiki/sources/raft-paper.md -> wiki/analyses/replication.md"
        );
        let back_json = json(&ws.ok(&["backlinks", "raft-paper", "--json"]));
        assert_eq!(back_json[0]["source"], "wiki/analyses/replication.md");
        assert_eq!(ws.ok(&["backlinks", "wiki/index.md"]).trim(), "");

        // Deleting a page without reindexing is caught by lint.
        fs::remove_file(ws.path("wiki/analyses/replication.md")).expect("remove page");
        let msg = ws.fail(&["lint"]);
        assert!(
            msg.contains("stale index entry: wiki/analyses/replication.md"),
            "{msg}"
        );
        assert!(
            msg.contains("broken link: wiki/sources/raft-paper.md -> wiki/sources/replication.md"),
            "{msg}"
        );
    }

    #[test]
    fn ingest_protects_existing_work() {
        let ws = Workspace::new();
        ws.ok(&["init"]);
        ws.write("a.md", "# Topic\n\nv1\n");
        ws.ok(&["ingest", "a.md"]);
        ws.write("wiki/sources/topic.md", "# Topic\n\nMy careful summary.\n");

        let msg = ws.fail(&["ingest", "a.md"]);
        assert!(msg.contains("already exists"), "{msg}");
        assert_eq!(
            ws.read("wiki/sources/topic.md"),
            "# Topic\n\nMy careful summary.\n"
        );

        ws.write("a.md", "# Topic\n\nv2\n");
        let msg = ws.fail(&["ingest", "a.md", "--title", "Topic v2"]);
        assert!(msg.contains("immutable"), "{msg}");
        assert_eq!(ws.read("raw/a.md"), "# Topic\n\nv1\n");
        assert!(!ws.path("wiki/sources/topic-v2.md").exists());
    }

    #[test]
    fn search_and_show_report_useful_errors() {
        let ws = Workspace::new();
        ws.ok(&["init"]);
        assert!(ws.fail(&["search", "?"]).contains("no searchable tokens"));
        assert!(ws.fail(&["show", "nothing"]).contains("no page matched"));
        assert_eq!(ws.ok(&["search", "nothing-here"]), "");
    }
}
