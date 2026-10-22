use anyhow::{Context, Result, anyhow};
use chrono::Local;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use crate::text::{ensure_trailing_newline, single_line};

pub fn write_text_atomic(path: &Path, text: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path {} has no parent directory", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("path {} has no file name", path.display()))?;
    let tmp_path = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let contents = ensure_trailing_newline(text);

    let write_tmp = || -> std::io::Result<()> {
        let mut tmp = File::create(&tmp_path)?;
        tmp.write_all(contents.as_bytes())?;
        tmp.sync_all()
    };
    if let Err(err) = write_tmp() {
        let _ = fs::remove_file(&tmp_path);
        return Err(err)
            .with_context(|| format!("failed to write temporary file {}", tmp_path.display()));
    }
    if let Err(err) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err).with_context(|| {
            format!(
                "failed to replace {} with {}",
                path.display(),
                tmp_path.display()
            )
        });
    }
    File::open(parent)
        .with_context(|| format!("failed to open directory {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync directory {}", parent.display()))?;
    Ok(())
}

pub fn write_if_missing(path: &Path, contents: &str) -> Result<()> {
    // create_new makes the existence check and the creation a single atomic step.
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file
            .write_all(ensure_trailing_newline(contents).as_bytes())
            .with_context(|| format!("failed to write {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to create {}", path.display())),
    }
}

pub fn append_log(log_path: &Path, op: &str, title: &str, details: &str) -> Result<()> {
    let stamp = Local::now().format("%Y-%m-%d %H:%M");
    let entry = format!(
        "\n## [{stamp}] {op} | {}\n\n{}\n",
        single_line(title),
        details.trim_end()
    );
    // O_APPEND keeps the log append-only: existing content is never rewritten.
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut log| log.write_all(entry.as_bytes()))
        .with_context(|| format!("failed to append to {}", log_path.display()))
}

pub fn absolute_from_root(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

pub fn normalize_link_target(base_dir: &Path, target: &str) -> PathBuf {
    let target_path = Path::new(target);
    if target_path.is_absolute() {
        normalize_lexical(target_path)
    } else {
        normalize_lexical(&base_dir.join(target_path))
    }
}

pub fn normalize_lexical(path: &Path) -> PathBuf {
    let mut components: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if matches!(components.last(), Some(Component::Normal(_))) => {
                components.pop();
            }
            other => components.push(other),
        }
    }
    components.into_iter().collect()
}

pub fn normalize_abs_link_to_rel(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

pub fn index_link_path(index_path: &Path, target_path: &Path) -> PathBuf {
    let base = index_path.parent().unwrap_or_else(|| Path::new(""));
    target_path
        .strip_prefix(base)
        .unwrap_or(target_path)
        .to_path_buf()
}

pub fn display_rel(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    #[test]
    fn normalize_lexical_resolves_dots_without_escaping_root() {
        assert_eq!(
            normalize_lexical(Path::new("/a/b/../c/./d")),
            Path::new("/a/c/d")
        );
        assert_eq!(normalize_lexical(Path::new("a/../../b")), Path::new("../b"));
        assert_eq!(normalize_lexical(Path::new("/../a")), Path::new("/../a"));
    }

    #[test]
    fn index_link_path_is_relative_to_index_dir() {
        assert_eq!(
            index_link_path(
                Path::new("/r/wiki/index.md"),
                Path::new("/r/wiki/sources/a.md")
            ),
            Path::new("sources/a.md")
        );
        assert_eq!(
            index_link_path(Path::new("/r/wiki/index.md"), Path::new("/elsewhere/a.md")),
            Path::new("/elsewhere/a.md")
        );
    }

    #[test]
    fn write_text_atomic_replaces_content_and_leaves_no_temp_files() -> Result<()> {
        let tmp = TempDir::new("atomic");
        let target = tmp.0.join("file.md");
        fs::write(&target, "old")?;

        write_text_atomic(&target, "new")?;

        assert_eq!(fs::read_to_string(&target)?, "new\n");
        let leftovers = fs::read_dir(&tmp.0)?.count();
        assert_eq!(leftovers, 1);
        Ok(())
    }

    #[test]
    fn write_if_missing_does_not_overwrite() -> Result<()> {
        let tmp = TempDir::new("if-missing");
        let target = tmp.0.join("f.md");
        write_if_missing(&target, "first")?;
        write_if_missing(&target, "second")?;
        assert_eq!(fs::read_to_string(&target)?, "first\n");
        Ok(())
    }

    #[test]
    fn write_if_missing_reports_real_errors() {
        let tmp = TempDir::new("if-missing-err");
        assert!(write_if_missing(&tmp.0.join("no-such-dir").join("f.md"), "x").is_err());
    }

    #[test]
    fn append_log_appends_without_touching_existing_content() -> Result<()> {
        let tmp = TempDir::new("log");
        let log = tmp.0.join("log.md");
        fs::write(&log, "# Log\n")?;

        append_log(&log, "ingest", "A\nB", "details  ")?;
        append_log(&log, "ingest", "C", "more")?;

        let text = fs::read_to_string(&log)?;
        assert!(text.starts_with("# Log\n\n## ["));
        assert!(text.contains("] ingest | A B\n\ndetails\n"));
        assert!(text.ends_with("] ingest | C\n\nmore\n"));
        Ok(())
    }
}
