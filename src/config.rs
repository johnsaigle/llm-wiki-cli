use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::fsutil::normalize_lexical;

pub const CONFIG_DIR: &str = ".llm-wiki";

pub const CONFIG_FILE: &str = ".llm-wiki/config.toml";

pub const SCHEMA_FILE: &str = "WIKI.md";

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Config {
    pub raw_dir: PathBuf,
    pub wiki_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct WikiPaths {
    pub root: PathBuf,
    pub raw_dir: PathBuf,
    pub wiki_dir: PathBuf,
    pub sources_dir: PathBuf,
    pub analyses_dir: PathBuf,
    pub index_path: PathBuf,
    pub log_path: PathBuf,
    pub schema_path: PathBuf,
}

pub fn read_config(config_path: &Path) -> Result<Config> {
    let config_text = fs::read_to_string(config_path).with_context(|| {
        format!(
            "failed to read {}. Run `llm-wiki init` first.",
            config_path.display()
        )
    })?;
    toml::from_str(&config_text)
        .with_context(|| format!("failed to parse {}", config_path.display()))
}

/// Directories must stay inside the wiki root; an absolute path or `..` would make
/// `root.join(dir)` escape it (or ignore the root entirely) and break relative-path logic.
pub fn validate_config(config: &Config) -> Result<()> {
    for (name, dir) in [("raw_dir", &config.raw_dir), ("wiki_dir", &config.wiki_dir)] {
        let normal_only = dir
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        if dir.as_os_str().is_empty() || !normal_only {
            bail!(
                "{name} `{}` must be a relative path inside the wiki root (no `..`)",
                dir.display()
            );
        }
    }
    if normalize_lexical(&config.raw_dir) == normalize_lexical(&config.wiki_dir) {
        bail!("raw_dir and wiki_dir must be different directories");
    }
    Ok(())
}

pub fn wiki_paths(root: &Path, config: &Config) -> WikiPaths {
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

pub fn load_workspace(root: &Path) -> Result<(Config, WikiPaths)> {
    let config = read_config(&root.join(CONFIG_FILE))?;
    validate_config(&config)?;
    let paths = wiki_paths(root, &config);
    Ok((config, paths))
}

pub fn default_schema(config: &Config) -> String {
    format!(
        "# Wiki Schema\n\n## Purpose\n\nThis wiki is a persistent markdown knowledge base maintained by an LLM. The CLI exists to provide deterministic filesystem, indexing, retrieval, and validation helpers.\n\n## Core Idea\n\n- Treat the wiki as a persistent, compounding artifact rather than a one-shot retrieval layer.\n- Raw sources are the source of truth; the LLM reads them and maintains synthesized wiki pages.\n- Good answers and durable analyses should usually be filed back into the wiki instead of living only in chat history.\n- The schema should stay practical and local to this repository; broader workflow guidance can live in separate source material.\n\n## Directories\n\n- Raw sources live in `{}` and are immutable.\n- Generated wiki pages live in `{}`.\n- `index.md` is the content catalog.\n- `log.md` is append-only and chronological.\n\n## CLI Contract\n\n- Use the CLI for deterministic operations: source registration, search, page lookup, backlinks, reindexing, and linting.\n- Do not use the CLI as a second reasoning model. The LLM should read pages itself and synthesize answers directly.\n- After editing wiki pages directly, run `llm-wiki reindex` when summaries or page listings may have changed.\n\n## Page Conventions\n\n- Prefer short markdown pages with descriptive titles.\n- Link related pages with markdown links.\n- Keep claims grounded in source pages when possible.\n- Save durable analyses back into the wiki rather than leaving them only in chat history.\n",
        config.raw_dir.display(),
        config.wiki_dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    #[test]
    fn config_validation_rejects_escaping_or_overlapping_dirs() {
        let cfg = |raw: &str, wiki: &str| Config {
            raw_dir: raw.into(),
            wiki_dir: wiki.into(),
        };
        assert!(validate_config(&cfg("raw", "wiki")).is_ok());
        assert!(validate_config(&cfg("./raw", "docs/wiki")).is_ok());
        assert!(validate_config(&cfg("/abs", "wiki")).is_err());
        assert!(validate_config(&cfg("raw", "../wiki")).is_err());
        assert!(validate_config(&cfg("", "wiki")).is_err());
        assert!(validate_config(&cfg("same", "./same")).is_err());
    }

    #[test]
    fn load_workspace_style_config_round_trips() -> Result<()> {
        let tmp = TempDir::new("config");
        let path = tmp.0.join("config.toml");
        let config = Config {
            raw_dir: "raw".into(),
            wiki_dir: "docs/wiki".into(),
        };
        fs::write(&path, toml::to_string_pretty(&config)?)?;

        let loaded = read_config(&path)?;

        assert_eq!(loaded.raw_dir, Path::new("raw"));
        assert_eq!(loaded.wiki_dir, Path::new("docs/wiki"));
        assert!(read_config(&tmp.0.join("missing.toml")).is_err());
        Ok(())
    }

    #[test]
    fn default_schema_mentions_configured_directories() {
        let schema = default_schema(&Config {
            raw_dir: "sources-raw".into(),
            wiki_dir: "notes".into(),
        });
        assert!(schema.starts_with("# Wiki Schema\n"));
        assert!(schema.contains("`sources-raw`"));
        assert!(schema.contains("`notes`"));
    }
}
