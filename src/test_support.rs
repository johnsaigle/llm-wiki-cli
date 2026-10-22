use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{SCHEMA_FILE, WikiPaths};

/// A scratch directory that is removed on drop, even when the test fails.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(prefix: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("llm-wiki-{prefix}-{unique}-{n}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A fully scaffolded workspace (what `init` produces), rooted in a temp dir.
pub fn workspace(prefix: &str) -> Result<(TempDir, WikiPaths)> {
    let tmp = TempDir::new(prefix);
    let paths = test_paths(&tmp.0);
    fs::create_dir_all(&paths.raw_dir)?;
    fs::create_dir_all(&paths.sources_dir)?;
    fs::create_dir_all(&paths.analyses_dir)?;
    fs::write(&paths.index_path, "# Index\n")?;
    fs::write(&paths.log_path, "# Log\n")?;
    Ok((tmp, paths))
}

pub fn test_paths(root: &Path) -> WikiPaths {
    let wiki_dir = root.join("wiki");
    WikiPaths {
        root: root.to_path_buf(),
        raw_dir: root.join("raw"),
        sources_dir: wiki_dir.join("sources"),
        analyses_dir: wiki_dir.join("analyses"),
        index_path: wiki_dir.join("index.md"),
        log_path: wiki_dir.join("log.md"),
        schema_path: root.join(SCHEMA_FILE),
        wiki_dir,
    }
}
