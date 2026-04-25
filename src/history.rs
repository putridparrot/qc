use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::config::HistoryLimit;

pub fn load_history(path: impl AsRef<Path>) -> Result<Vec<String>> {
    let path = path.as_ref();

    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read history file: {}", path.display()))?;

    let entries = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    Ok(entries)
}

pub fn append_history(
    path: impl AsRef<Path>,
    command: &str,
    history_limit: HistoryLimit,
) -> Result<Vec<String>> {
    let path = path.as_ref();

    if history_limit == HistoryLimit::Disabled {
        fs::write(path, "")
            .with_context(|| format!("Failed to write history file: {}", path.display()))?;
        return Ok(Vec::new());
    }

    let mut entries = load_history(path)?;
    if let Some(existing_index) = entries.iter().position(|entry| entry == command) {
        entries.remove(existing_index);
    }

    entries.push(command.to_owned());

    if let HistoryLimit::Limited(max_items) = history_limit
        && entries.len() > max_items
    {
        let start = entries.len() - max_items;
        entries = entries.split_off(start);
    }

    write_history(path, &entries)?;

    Ok(entries)
}

pub fn prune_history(path: impl AsRef<Path>, history_limit: HistoryLimit) -> Result<Vec<String>> {
    let path = path.as_ref();

    if history_limit == HistoryLimit::Disabled {
        fs::write(path, "")
            .with_context(|| format!("Failed to write history file: {}", path.display()))?;
        return Ok(Vec::new());
    }

    let mut entries = load_history(path)?;

    if let HistoryLimit::Limited(max_items) = history_limit
        && entries.len() > max_items
    {
        let start = entries.len() - max_items;
        entries = entries.split_off(start);
        write_history(path, &entries)?;
    }

    Ok(entries)
}

pub fn delete_history_entry(path: impl AsRef<Path>, one_based_index: usize) -> Result<Vec<String>> {
    if one_based_index == 0 {
        bail!("History index must be 1 or greater");
    }

    let path = path.as_ref();
    let mut entries = load_history(path)?;
    let zero_based = one_based_index - 1;

    if zero_based >= entries.len() {
        bail!(
            "History index out of range: {} (history length: {})",
            one_based_index,
            entries.len()
        );
    }

    entries.remove(zero_based);
    write_history(path, &entries)?;

    Ok(entries)
}

pub fn clear_history(path: impl AsRef<Path>) -> Result<Vec<String>> {
    let path = path.as_ref();
    write_history(path, &[])?;
    Ok(Vec::new())
}

pub fn write_history(path: impl AsRef<Path>, entries: &[String]) -> Result<()> {
    let path = path.as_ref();
    let content = if entries.is_empty() {
        String::new()
    } else {
        format!("{}\n", entries.join("\n"))
    };

    fs::write(path, content)
        .with_context(|| format!("Failed to write history file: {}", path.display()))
}

pub fn delete_history_range(
    path: impl AsRef<Path>,
    one_based_start: usize,
    one_based_end: usize,
) -> Result<Vec<String>> {
    if one_based_start == 0 || one_based_end == 0 {
        bail!("History range indexes must be 1 or greater");
    }

    if one_based_start > one_based_end {
        bail!("History range must be start-end where start <= end");
    }

    let path = path.as_ref();
    let mut entries = load_history(path)?;
    let start = one_based_start - 1;
    let end = one_based_end - 1;

    if start >= entries.len() || end >= entries.len() {
        bail!(
            "History range out of bounds: {}-{} (history length: {})",
            one_based_start,
            one_based_end,
            entries.len()
        );
    }

    entries.drain(start..=end);
    write_history(path, &entries)?;
    Ok(entries)
}

pub fn dedupe_history(path: impl AsRef<Path>) -> Result<Vec<String>> {
    let path = path.as_ref();
    let entries = load_history(path)?;
    let mut deduped = Vec::new();

    for entry in entries.into_iter().rev() {
        if !deduped.contains(&entry) {
            deduped.push(entry);
        }
    }

    deduped.reverse();
    write_history(path, &deduped)?;
    Ok(deduped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file(name: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("qc-history-{name}-{ts}.txt"))
    }

    #[test]
    fn append_history_moves_duplicate_to_end() {
        let path = temp_file("append-move-end");
        fs::write(&path, "a\nb\nc\n").expect("seed history");

        let entries = append_history(&path, "b", HistoryLimit::Unlimited).expect("append");
        assert_eq!(entries, vec!["a", "c", "b"]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn append_history_respects_limit() {
        let path = temp_file("append-limit");
        fs::write(&path, "a\nb\n").expect("seed history");

        let entries = append_history(&path, "c", HistoryLimit::Limited(2)).expect("append");
        assert_eq!(entries, vec!["b", "c"]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn delete_history_range_removes_slice() {
        let path = temp_file("delete-range");
        fs::write(&path, "one\ntwo\nthree\nfour\n").expect("seed history");

        let entries = delete_history_range(&path, 2, 3).expect("delete range");
        assert_eq!(entries, vec!["one", "four"]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn dedupe_history_keeps_latest_copy() {
        let path = temp_file("dedupe");
        fs::write(&path, "a\nb\na\nc\nb\n").expect("seed history");

        let entries = dedupe_history(&path).expect("dedupe");
        assert_eq!(entries, vec!["a", "c", "b"]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn prune_history_disabled_clears_file() {
        let path = temp_file("prune-disabled");
        fs::write(&path, "x\ny\n").expect("seed history");

        let entries = prune_history(&path, HistoryLimit::Disabled).expect("prune");
        assert!(entries.is_empty());
        let persisted = load_history(&path).expect("load");
        assert!(persisted.is_empty());

        let _ = fs::remove_file(path);
    }
}
