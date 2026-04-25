mod config;
mod hinter;
mod history;
mod shortcuts;

use std::borrow::Cow;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{AppConfig, SafetyPolicy, load_config, save_config};
use crate::hinter::{HintHighlighter, SharedHints, ShortcutHinter};
use crate::history::{
    append_history, clear_history, dedupe_history, delete_history_entry, delete_history_range,
    load_history, prune_history,
};
use crate::shortcuts::{
    Shortcut, add_shortcut_with_tags, delete_shortcut, filter_shortcuts_by_tag, find_shortcut,
    is_dangerous, load_shortcuts, prompt_for_args, run_command, shortcut_names,
};
use anyhow::{Context, bail};
use nu_ansi_term::{Color, Style};
use reedline::{Prompt, PromptEditMode, PromptHistorySearch, Reedline, Signal};

const CONFIG_FILE: &str = "config.txt";
const SHORTCUTS_FILE: &str = "shortcuts.txt";
const HISTORY_FILE: &str = "history.txt";
const HISTORY_PINS_FILE: &str = "history_pins.txt";
const HISTORY_USAGE_FILE: &str = "history_usage.txt";
const PLACEHOLDER_VALUES_FILE: &str = "placeholder_values.txt";
const AUDIT_LOG_FILE: &str = "audit.log";
const EXPORT_HEADER: &str = "# qc-export:v1";
const BACKUP_DIR: &str = "backups";
const LAST_BACKUP_FILE: &str = ".qc_last_backup";

struct CmdStylePrompt;

impl CmdStylePrompt {
    fn current_dir_display() -> String {
        env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| ".".to_owned())
    }
}

impl Prompt for CmdStylePrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _prompt_mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Owned(format!("QC {}>", Self::current_dir_display()))
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed(">> ")
    }

    fn render_prompt_history_search_indicator(
        &self,
        _history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        Cow::Borrowed("history> ")
    }
}

#[derive(Clone, Debug)]
enum FindResult {
    Shortcut(String),
    History(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BuiltinCommand {
    Help,
    Doctor,
    SetDryRun(bool),
    ProfileList,
    ProfileUse(String),
    Completion(String),
    ShortcutsList,
    ShortcutsTag(String),
    ShortcutsAdd(String),
    ShortcutsDel(String),
    HistoryList,
    HistoryRanked,
    HistoryAdd(String),
    HistoryPin(String),
    HistoryUnpin(String),
    HistoryDel(String),
    HistoryDedupe,
    HistoryClear,
    Find(String),
    FindBang(String),
    Export(String),
    Import(String),
    Undo,
    Exit,
    Reload,
    Unknown,
}

fn parse_builtin_command(input: &str) -> Option<BuiltinCommand> {
    if !input.starts_with(':') {
        return None;
    }

    if input == ":help" || input == ":?" {
        return Some(BuiltinCommand::Help);
    }

    if input == ":doctor" {
        return Some(BuiltinCommand::Doctor);
    }

    if input == ":set dry-run on" {
        return Some(BuiltinCommand::SetDryRun(true));
    }

    if input == ":set dry-run off" {
        return Some(BuiltinCommand::SetDryRun(false));
    }

    if input == ":profile list" {
        return Some(BuiltinCommand::ProfileList);
    }

    if let Some(profile) = input.strip_prefix(":profile use ") {
        return Some(BuiltinCommand::ProfileUse(profile.trim().to_owned()));
    }

    if let Some(shell) = input.strip_prefix(":completion ") {
        return Some(BuiltinCommand::Completion(shell.trim().to_owned()));
    }

    if input == ":exit" || input == ":quit" || input == ":q" {
        return Some(BuiltinCommand::Exit);
    }

    if input == ":reload" || input == ":r" {
        return Some(BuiltinCommand::Reload);
    }

    if input == ":shortcuts" || input == ":s" {
        return Some(BuiltinCommand::ShortcutsList);
    }

    if let Some(tag) = input.strip_prefix(":shortcuts tag ") {
        return Some(BuiltinCommand::ShortcutsTag(tag.trim().to_owned()));
    }

    if let Some(spec) = input.strip_prefix(":shortcuts add ") {
        return Some(BuiltinCommand::ShortcutsAdd(spec.to_owned()));
    }

    if let Some(name) = input.strip_prefix(":shortcuts del ") {
        return Some(BuiltinCommand::ShortcutsDel(name.trim().to_owned()));
    }

    if input == ":history" || input == ":h" {
        return Some(BuiltinCommand::HistoryList);
    }

    if input == ":history ranked" {
        return Some(BuiltinCommand::HistoryRanked);
    }

    if let Some(command) = input.strip_prefix(":history add ") {
        return Some(BuiltinCommand::HistoryAdd(command.to_owned()));
    }

    if let Some(index) = input.strip_prefix(":history pin ") {
        return Some(BuiltinCommand::HistoryPin(index.trim().to_owned()));
    }

    if let Some(index) = input.strip_prefix(":history unpin ") {
        return Some(BuiltinCommand::HistoryUnpin(index.trim().to_owned()));
    }

    if let Some(index) = input.strip_prefix(":history del ") {
        return Some(BuiltinCommand::HistoryDel(index.trim().to_owned()));
    }

    if input == ":history dedupe" {
        return Some(BuiltinCommand::HistoryDedupe);
    }

    if input == ":history clear" {
        return Some(BuiltinCommand::HistoryClear);
    }

    if let Some(query) = input.strip_prefix(":find ") {
        return Some(BuiltinCommand::Find(query.to_owned()));
    }

    if let Some(query) = input.strip_prefix(":find! ") {
        return Some(BuiltinCommand::FindBang(query.to_owned()));
    }

    if let Some(path) = input.strip_prefix(":export ") {
        return Some(BuiltinCommand::Export(path.trim().to_owned()));
    }

    if let Some(path) = input.strip_prefix(":import ") {
        return Some(BuiltinCommand::Import(path.trim().to_owned()));
    }

    if input == ":undo" {
        return Some(BuiltinCommand::Undo);
    }

    Some(BuiltinCommand::Unknown)
}

fn build_hint_names(sc_names: &[String], history_entries: &[String]) -> Vec<String> {
    let mut hint_names = sc_names.to_vec();

    for entry in history_entries {
        if !hint_names.contains(entry) {
            hint_names.push(entry.clone());
        }
    }

    hint_names
}

fn shortcuts_file_for_profile(profile: &str) -> String {
    if profile.eq_ignore_ascii_case("default") {
        SHORTCUTS_FILE.to_owned()
    } else {
        format!("shortcuts.{}.txt", profile)
    }
}

fn list_profiles() -> anyhow::Result<Vec<String>> {
    let mut profiles = vec!["default".to_owned()];
    for entry in fs::read_dir(".")? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(profile) = name
            .strip_prefix("shortcuts.")
            .and_then(|s| s.strip_suffix(".txt"))
            && !profile.is_empty()
            && !profiles.contains(&profile.to_owned())
        {
            profiles.push(profile.to_owned());
        }
    }
    profiles.sort();
    Ok(profiles)
}

fn append_audit_log(profile: &str, status: &str, command: &str) -> anyhow::Result<()> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let line = format!(
        "{}|{}|{}|{}\n",
        ts,
        profile,
        status,
        command.replace('\n', " ")
    );
    let mut existing = fs::read_to_string(AUDIT_LOG_FILE).unwrap_or_default();
    existing.push_str(&line);
    fs::write(AUDIT_LOG_FILE, existing)
        .with_context(|| format!("Failed to write {}", AUDIT_LOG_FILE))
}

fn generate_completion_script(shell: &str, shortcut_names: &[String]) -> anyhow::Result<String> {
    let builtins = [
        ":help",
        ":doctor",
        ":set",
        ":profile",
        ":completion",
        ":shortcuts",
        ":history",
        ":find",
        ":find!",
        ":export",
        ":import",
        ":undo",
        ":reload",
        ":exit",
        ":quit",
        ":q",
    ];

    match shell.to_ascii_lowercase().as_str() {
        "bash" => {
            let mut words = builtins.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            words.extend(shortcut_names.iter().cloned());
            Ok(format!(
                "_qc_complete() {{\n  local cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n  COMPREPLY=( $(compgen -W \"{}\" -- \"$cur\") )\n}}\ncomplete -F _qc_complete qc\n",
                words.join(" ")
            ))
        }
        "powershell" => {
            let mut words = builtins
                .iter()
                .map(|s| format!("'{}'", s))
                .collect::<Vec<_>>();
            words.extend(shortcut_names.iter().map(|s| format!("'{}'", s)));
            Ok(format!(
                "Register-ArgumentCompleter -Native -CommandName qc -ScriptBlock {{\n  param($wordToComplete)\n  @({}) | Where-Object {{ $_ -like \"$wordToComplete*\" }} | ForEach-Object {{ [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_) }}\n}}\n",
                words.join(",")
            ))
        }
        _ => bail!("Unsupported shell '{}'; use bash or powershell", shell),
    }
}

fn run_doctor(config: &AppConfig, shortcuts_file: &str) -> anyhow::Result<()> {
    println!("Doctor report:");
    println!("  active_profile: {}", config.active_profile);
    println!("  shortcuts_file: {}", shortcuts_file);

    match load_config(CONFIG_FILE) {
        Ok(_) => println!("  config: ok"),
        Err(e) => println!("  config: error ({e:#})"),
    }
    match load_shortcuts(shortcuts_file) {
        Ok(shortcuts) => {
            println!("  shortcuts: ok ({} entries)", shortcuts.len());
            let prod_dangerous = shortcuts
                .iter()
                .filter(|s| {
                    s.tags.iter().any(|t| t.eq_ignore_ascii_case("prod"))
                        && is_dangerous(&s.command)
                })
                .count();
            if prod_dangerous > 0 {
                println!(
                    "  warning: {} prod-tagged shortcut(s) match dangerous patterns",
                    prod_dangerous
                );
            }
        }
        Err(e) => println!("  shortcuts: error ({e:#})"),
    }
    match load_history(HISTORY_FILE) {
        Ok(history) => println!("  history: ok ({} entries)", history.len()),
        Err(e) => println!("  history: error ({e:#})"),
    }

    Ok(())
}

fn prompt_yes_no(message: &str, default_yes: bool) -> anyhow::Result<bool> {
    print!("{message}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let response = input.trim().to_lowercase();
    if response.is_empty() {
        return Ok(default_yes);
    }
    Ok(response == "y" || response == "yes")
}

fn preview_command(command: &str) -> anyhow::Result<bool> {
    println!("Preview: {command}");
    prompt_yes_no("Run this command? [Y/n]: ", true)
}

fn approve_command(command: &str, safety_policy: SafetyPolicy) -> anyhow::Result<bool> {
    if !is_dangerous(command) {
        return Ok(true);
    }

    match safety_policy {
        SafetyPolicy::Warn => {
            println!("Warning: dangerous pattern detected.");
            Ok(true)
        }
        SafetyPolicy::Confirm => prompt_yes_no(
            &format!("Warning: '{command}' looks dangerous. Continue? [y/N]: "),
            false,
        ),
        SafetyPolicy::Block => {
            if command.contains("--force") {
                println!("Dangerous command allowed because --force is present.");
                Ok(true)
            } else {
                println!("Blocked by safety policy. Add --force to override.");
                Ok(false)
            }
        }
    }
}

fn enforce_prod_phrase_for_dangerous(command: &str, tags: &[String]) -> anyhow::Result<bool> {
    let is_prod = tags.iter().any(|tag| tag.eq_ignore_ascii_case("prod"));
    if !is_prod || !is_dangerous(command) {
        return Ok(true);
    }

    print!("Prod safety check: type PROD to continue: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim() == "PROD")
}

fn parse_name_and_tags(raw: &str) -> (String, Vec<String>) {
    let raw = raw.trim();
    let Some(open_index) = raw.find('[') else {
        return (raw.to_owned(), Vec::new());
    };
    let Some(close_index) = raw.rfind(']') else {
        return (raw.to_owned(), Vec::new());
    };
    if close_index <= open_index {
        return (raw.to_owned(), Vec::new());
    }

    let name = raw[..open_index].trim();
    let tags = raw[open_index + 1..close_index]
        .split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    (name.to_owned(), tags)
}

fn parse_shortcut_add(spec: &str) -> anyhow::Result<(String, Vec<String>, String)> {
    let Some((name, command)) = spec.split_once('=') else {
        bail!("Expected ':shortcuts add <name>[tag1,tag2]=<command>'");
    };

    let (name, tags) = parse_name_and_tags(name);
    let command = command.trim().to_owned();
    if name.is_empty() || command.is_empty() {
        bail!("Shortcut name and command must be non-empty");
    }

    Ok((name, tags, command))
}

fn builtin_help_text() -> String {
    [
        "Built-in commands (reserved ':' namespace):",
        "  :doctor                                       Validate config and data files",
        "  :set dry-run on|off                           Toggle dry-run mode",
        "  :profile list                                 List available profiles",
        "  :profile use <name>                           Switch active profile",
        "  :completion bash|powershell                   Generate shell completion file",
        "  :shortcuts | :s                               List loaded shortcuts",
        "  :shortcuts tag <tag>                          List shortcuts by tag",
        "  :shortcuts add <name>[tag1,tag2]=<command>   Add/update shortcut",
        "  :shortcuts del <name>                 Delete shortcut",
        "  :history   | :h                               List persisted history",
        "  :history ranked                               List ranked by recency + frequency",
        "  :history add <command>                        Add a history entry",
        "  :history del <index> | <start-end>            Delete one/range of history entries",
        "  :history pin <index>                          Pin a history entry",
        "  :history unpin <index>                        Unpin a history entry",
        "  :history dedupe                               Remove duplicates while keeping latest",
        "  :history clear                                Clear all history",
        "  :find <text>                                  Search shortcuts and history",
        "  :find! <text>                                 Search and pick an item to run",
        "  :find run <index>                             Run a :find result",
        "  :export <file>                                Export shortcuts/history/config state",
        "  :import <file>                                Import shortcuts/history/config state",
        "  :undo                                         Restore latest backup",
        "  :exit | :quit | :q                           Exit qc",
        "  :reload | :r                                  Reload config and shortcuts",
        "  :help | :?                                    Show this help",
    ]
    .join("\n")
}

fn print_builtin_help() {
    println!("{}", builtin_help_text());
}

fn refresh_hints(shared_hints: &SharedHints, sc_names: &[String], history_entries: &[String]) {
    let mut hints = shared_hints.write().unwrap_or_else(|p| p.into_inner());
    *hints = build_hint_names(sc_names, history_entries);
}

fn load_string_set(path: impl AsRef<Path>) -> anyhow::Result<Vec<String>> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn write_string_set(path: impl AsRef<Path>, values: &[String]) -> anyhow::Result<()> {
    let path = path.as_ref();
    let content = if values.is_empty() {
        String::new()
    } else {
        format!("{}\n", values.join("\n"))
    };
    fs::write(path, content)
        .with_context(|| format!("Failed to write file: {}", path.display()))?;
    Ok(())
}

fn load_usage(path: impl AsRef<Path>) -> anyhow::Result<HashMap<String, u64>> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let mut usage = HashMap::new();
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read usage file: {}", path.display()))?;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if let Ok(parsed) = value.trim().parse::<u64>() {
            usage.insert(key.trim().to_owned(), parsed);
        }
    }
    Ok(usage)
}

fn save_usage(path: impl AsRef<Path>, usage: &HashMap<String, u64>) -> anyhow::Result<()> {
    let path = path.as_ref();
    let mut keys = usage.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    let lines = keys
        .into_iter()
        .map(|key| format!("{}={}", key, usage.get(&key).unwrap_or(&0)))
        .collect::<Vec<_>>();
    let content = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    fs::write(path, content)
        .with_context(|| format!("Failed to write usage file: {}", path.display()))?;
    Ok(())
}

fn increment_usage(path: impl AsRef<Path>, command: &str) -> anyhow::Result<()> {
    let path = path.as_ref();
    let mut usage = load_usage(path)?;
    let entry = usage.entry(command.to_owned()).or_insert(0);
    *entry += 1;
    save_usage(path, &usage)
}

fn format_shortcut(shortcut: &Shortcut) -> String {
    if shortcut.tags.is_empty() {
        format!("{} = {}", shortcut.name, shortcut.command)
    } else {
        format!(
            "{} [{}] = {}",
            shortcut.name,
            shortcut.tags.join(","),
            shortcut.command
        )
    }
}

fn create_backup(paths: &[&str]) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(BACKUP_DIR).with_context(|| format!("Failed to create {}", BACKUP_DIR))?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup_path = Path::new(BACKUP_DIR).join(format!("qc-backup-{ts}.txt"));

    let mut content = String::new();
    for path in paths {
        let file_content = fs::read_to_string(path).unwrap_or_default();
        content.push_str(&format!("---FILE:{}\n", path));
        content.push_str(&file_content);
        if !file_content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("---END\n");
    }

    fs::write(&backup_path, content)
        .with_context(|| format!("Failed to write backup: {}", backup_path.display()))?;
    fs::write(LAST_BACKUP_FILE, backup_path.to_string_lossy().as_ref())
        .with_context(|| format!("Failed to write {}", LAST_BACKUP_FILE))?;
    Ok(backup_path)
}

fn restore_from_backup(backup_path: impl AsRef<Path>) -> anyhow::Result<()> {
    let backup_path = backup_path.as_ref();
    let content = fs::read_to_string(backup_path)
        .with_context(|| format!("Failed to read backup: {}", backup_path.display()))?;

    let mut current_file: Option<String> = None;
    let mut lines = Vec::new();
    for line in content.lines() {
        if let Some(path) = line.strip_prefix("---FILE:") {
            current_file = Some(path.to_owned());
            lines.clear();
            continue;
        }

        if line == "---END" {
            if let Some(path) = current_file.take() {
                let file_content = if lines.is_empty() {
                    String::new()
                } else {
                    format!("{}\n", lines.join("\n"))
                };
                fs::write(&path, file_content)
                    .with_context(|| format!("Failed to restore file: {path}"))?;
            }
            lines.clear();
            continue;
        }

        if current_file.is_some() {
            lines.push(line.to_owned());
        }
    }

    Ok(())
}

fn restore_last_backup() -> anyhow::Result<()> {
    let path = fs::read_to_string(LAST_BACKUP_FILE)
        .with_context(|| format!("No backup available in {}", LAST_BACKUP_FILE))?;
    restore_from_backup(path.trim())
}

fn export_state(path: impl AsRef<Path>) -> anyhow::Result<()> {
    let path = path.as_ref();
    let mut content = String::new();
    content.push_str(EXPORT_HEADER);
    content.push('\n');
    for (name, file) in [
        ("config", CONFIG_FILE),
        ("shortcuts", SHORTCUTS_FILE),
        ("history", HISTORY_FILE),
        ("pins", HISTORY_PINS_FILE),
        ("usage", HISTORY_USAGE_FILE),
        ("placeholder_values", PLACEHOLDER_VALUES_FILE),
        ("audit", AUDIT_LOG_FILE),
    ] {
        content.push_str(&format!("[{name}]\n"));
        content.push_str(&fs::read_to_string(file).unwrap_or_default());
        if !content.ends_with('\n') {
            content.push('\n');
        }
    }

    fs::write(path, content)
        .with_context(|| format!("Failed to write export file: {}", path.display()))?;
    Ok(())
}

fn import_state(path: impl AsRef<Path>) -> anyhow::Result<()> {
    let path = path.as_ref();
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read import file: {}", path.display()))?;

    let mut lines = content.lines();
    let Some(header) = lines.next() else {
        bail!("Import file is empty");
    };
    if header.trim() != EXPORT_HEADER {
        bail!("Unsupported export format header: {}", header);
    }

    let mut current_section = String::new();
    let mut sections: HashMap<String, Vec<String>> = HashMap::new();
    for line in lines {
        if line.starts_with('[') && line.ends_with(']') {
            current_section = line.trim_matches(&['[', ']'][..]).to_owned();
            continue;
        }
        if !current_section.is_empty() {
            sections
                .entry(current_section.clone())
                .or_default()
                .push(line.to_owned());
        }
    }

    for (section, file) in [
        ("config", CONFIG_FILE),
        ("shortcuts", SHORTCUTS_FILE),
        ("history", HISTORY_FILE),
        ("pins", HISTORY_PINS_FILE),
        ("usage", HISTORY_USAGE_FILE),
        ("placeholder_values", PLACEHOLDER_VALUES_FILE),
        ("audit", AUDIT_LOG_FILE),
    ] {
        if let Some(lines) = sections.get(section) {
            let new_content = if lines.is_empty() {
                String::new()
            } else {
                format!("{}\n", lines.join("\n"))
            };
            fs::write(file, new_content).with_context(|| format!("Failed to write {file}"))?;
        }
    }

    Ok(())
}

fn run_executable_command(
    command: &str,
    config: &AppConfig,
    tags: &[String],
) -> anyhow::Result<bool> {
    if config.dry_run {
        if !preview_command(command)? {
            append_audit_log(&config.active_profile, "aborted-preview", command)?;
            println!("  Aborted.");
            return Ok(false);
        }
        println!("Dry-run enabled; command not executed.");
        append_audit_log(&config.active_profile, "dry-run", command)?;
        return Ok(true);
    }

    if !enforce_prod_phrase_for_dangerous(command, tags)? {
        append_audit_log(&config.active_profile, "aborted-prod-phrase", command)?;
        println!("  Aborted.");
        return Ok(false);
    }

    if !approve_command(command, config.safety_policy)? {
        append_audit_log(&config.active_profile, "aborted-safety", command)?;
        println!("  Aborted.");
        return Ok(false);
    }

    if let Err(error) = run_command(command) {
        append_audit_log(&config.active_profile, "failed", command)?;
        eprintln!("{error:#}");
    } else {
        append_audit_log(&config.active_profile, "ok", command)?;
    }

    increment_usage(HISTORY_USAGE_FILE, command)?;
    Ok(true)
}

fn execute_shortcut(shortcut: &Shortcut, config: &AppConfig) -> anyhow::Result<bool> {
    let command = match prompt_for_args(&shortcut.command, PLACEHOLDER_VALUES_FILE) {
        Ok(cmd) => cmd,
        Err(e) => {
            eprintln!("{e:#}");
            return Ok(false);
        }
    };

    println!("Running {} -> {}", shortcut.name, command);
    run_executable_command(&command, config, &shortcut.tags)
}

fn print_history(entries: &[String], pins: &[String], usage: &HashMap<String, u64>) {
    if entries.is_empty() {
        println!("  (history is empty)");
        return;
    }

    for (i, entry) in entries.iter().enumerate() {
        let pin = if pins.contains(entry) { "*" } else { " " };
        let count = usage.get(entry).copied().unwrap_or(0);
        println!("  {:>3} {} [{}] {}", i + 1, pin, count, entry);
    }
}

fn print_ranked_history(entries: &[String], usage: &HashMap<String, u64>) {
    let mut ranked = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let recency = (entries.len() - index) as u64;
            let frequency = usage.get(entry).copied().unwrap_or(0);
            let score = frequency * 1000 + recency;
            (entry.clone(), score, frequency, recency)
        })
        .collect::<Vec<_>>();

    ranked.sort_by_key(|item| std::cmp::Reverse(item.1));

    for (index, (entry, score, frequency, recency)) in ranked.iter().enumerate() {
        println!(
            "  {:>3} [score:{} freq:{} recency:{}] {}",
            index + 1,
            score,
            frequency,
            recency,
            entry
        );
    }
}

fn parse_history_range(value: &str) -> Option<(usize, usize)> {
    let (start, end) = value.split_once('-')?;
    Some((start.trim().parse().ok()?, end.trim().parse().ok()?))
}

fn refresh_runtime_state(
    shortcuts: &[Shortcut],
    shared_hints: &SharedHints,
) -> anyhow::Result<Vec<String>> {
    let sc_names = shortcut_names(shortcuts);
    let history_entries = load_history(HISTORY_FILE)?;
    refresh_hints(shared_hints, &sc_names, &history_entries);
    Ok(history_entries)
}

fn handle_script_shortcut_command(input: &str) -> bool {
    parse_builtin_command(input).is_some()
}

enum BuiltinOutcome {
    Continue,
    Exit,
}

fn maybe_backup(scripted: bool, paths: &[&str]) -> anyhow::Result<()> {
    if !scripted {
        let _ = create_backup(paths)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn execute_builtin(
    command: BuiltinCommand,
    scripted: bool,
    config: &mut AppConfig,
    history_limit: &mut crate::config::HistoryLimit,
    shortcuts_file: &mut String,
    shortcuts: &mut Vec<Shortcut>,
    sc_names: &mut Vec<String>,
    shared_hints: &SharedHints,
    last_find_results: &mut Vec<FindResult>,
) -> anyhow::Result<BuiltinOutcome> {
    match command {
        BuiltinCommand::Help => {
            print_builtin_help();
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::Doctor => {
            run_doctor(config, shortcuts_file.as_str())?;
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::SetDryRun(enabled) => {
            config.dry_run = enabled;
            save_config(CONFIG_FILE, config)?;
            println!("dry_run set to {}", config.dry_run);
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::ProfileList => {
            for profile in list_profiles()? {
                println!("  {}", profile);
            }
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::ProfileUse(profile) => {
            let profile = profile.trim();
            if profile.is_empty() {
                eprintln!("Expected ':profile use <name>'");
                return Ok(BuiltinOutcome::Continue);
            }

            config.active_profile = profile.to_owned();
            *shortcuts_file = shortcuts_file_for_profile(profile);
            *shortcuts = load_shortcuts(shortcuts_file.as_str()).unwrap_or_default();
            *sc_names = shortcut_names(shortcuts);
            save_config(CONFIG_FILE, config)?;
            let history_entries = load_history(HISTORY_FILE).unwrap_or_default();
            refresh_hints(shared_hints, sc_names, &history_entries);
            println!(
                "Active profile set to '{}' using {}",
                profile, shortcuts_file
            );
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::Completion(shell) => {
            let script = generate_completion_script(&shell, sc_names)?;
            let out = match shell.to_ascii_lowercase().as_str() {
                "bash" => "qc-completion.bash",
                "powershell" => "qc-completion.ps1",
                _ => {
                    eprintln!("Unsupported shell '{}'; use bash or powershell", shell);
                    return Ok(BuiltinOutcome::Continue);
                }
            };
            fs::write(out, script)?;
            println!("Completion script written to {}", out);
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::Exit => Ok(BuiltinOutcome::Exit),
        BuiltinCommand::Reload => {
            match load_config(CONFIG_FILE) {
                Ok(new_config) => {
                    *config = new_config;
                    *history_limit = config.history_limit();
                    *shortcuts_file = shortcuts_file_for_profile(&config.active_profile);
                }
                Err(e) => eprintln!("Failed to reload config: {e:#}"),
            }
            match load_shortcuts(shortcuts_file.as_str()) {
                Ok(new_shortcuts) => {
                    *shortcuts = new_shortcuts;
                    *sc_names = shortcut_names(shortcuts);
                }
                Err(e) => eprintln!("Failed to reload shortcuts: {e:#}"),
            }
            let history_entries = load_history(HISTORY_FILE).unwrap_or_default();
            refresh_hints(shared_hints, sc_names, &history_entries);
            println!("Configuration reloaded.");
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::ShortcutsList => {
            if shortcuts.is_empty() {
                println!("  (no shortcuts loaded)");
            } else {
                for shortcut in shortcuts {
                    println!("  {}", format_shortcut(shortcut));
                }
            }
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::ShortcutsTag(tag) => {
            let tag = tag.trim();
            let matches = filter_shortcuts_by_tag(shortcuts, tag);
            if matches.is_empty() {
                println!("  (no shortcuts for tag '{}')", tag);
            } else {
                for shortcut in matches {
                    println!("  {}", format_shortcut(shortcut));
                }
            }
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::ShortcutsAdd(spec) => {
            maybe_backup(
                scripted,
                &[
                    shortcuts_file.as_str(),
                    HISTORY_FILE,
                    HISTORY_PINS_FILE,
                    HISTORY_USAGE_FILE,
                ],
            )?;

            match parse_shortcut_add(&spec).and_then(|(name, tags, command)| {
                add_shortcut_with_tags(shortcuts_file.as_str(), &name, &command, &tags)
            }) {
                Ok((new_shortcuts, true)) => {
                    println!("Shortcut added.");
                    *shortcuts = new_shortcuts;
                    *sc_names = shortcut_names(shortcuts);
                    let history_entries = load_history(HISTORY_FILE).unwrap_or_default();
                    refresh_hints(shared_hints, sc_names, &history_entries);
                }
                Ok((new_shortcuts, false)) => {
                    println!("Shortcut updated.");
                    *shortcuts = new_shortcuts;
                    *sc_names = shortcut_names(shortcuts);
                    let history_entries = load_history(HISTORY_FILE).unwrap_or_default();
                    refresh_hints(shared_hints, sc_names, &history_entries);
                }
                Err(e) => eprintln!("{e:#}"),
            }

            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::ShortcutsDel(name) => {
            maybe_backup(
                scripted,
                &[
                    shortcuts_file.as_str(),
                    HISTORY_FILE,
                    HISTORY_PINS_FILE,
                    HISTORY_USAGE_FILE,
                ],
            )?;

            let name = name.trim();
            if name.is_empty() {
                eprintln!("Expected ':shortcuts del <name>'");
                return Ok(BuiltinOutcome::Continue);
            }

            match delete_shortcut(shortcuts_file.as_str(), name) {
                Ok((new_shortcuts, true)) => {
                    println!("Shortcut deleted.");
                    *shortcuts = new_shortcuts;
                    *sc_names = shortcut_names(shortcuts);
                    let history_entries = load_history(HISTORY_FILE).unwrap_or_default();
                    refresh_hints(shared_hints, sc_names, &history_entries);
                }
                Ok((_new_shortcuts, false)) => {
                    println!("Shortcut '{name}' not found.");
                }
                Err(e) => eprintln!("{e:#}"),
            }

            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::HistoryList => {
            let entries = load_history(HISTORY_FILE).unwrap_or_default();
            let pins = load_string_set(HISTORY_PINS_FILE).unwrap_or_default();
            let usage = load_usage(HISTORY_USAGE_FILE).unwrap_or_default();
            print_history(&entries, &pins, &usage);
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::HistoryRanked => {
            let entries = load_history(HISTORY_FILE).unwrap_or_default();
            let usage = load_usage(HISTORY_USAGE_FILE).unwrap_or_default();
            print_ranked_history(&entries, &usage);
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::HistoryAdd(command) => {
            maybe_backup(
                scripted,
                &[
                    SHORTCUTS_FILE,
                    HISTORY_FILE,
                    HISTORY_PINS_FILE,
                    HISTORY_USAGE_FILE,
                ],
            )?;

            let command = command.trim();
            if command.is_empty() {
                eprintln!("Expected ':history add <command>'");
                return Ok(BuiltinOutcome::Continue);
            }

            match append_history(HISTORY_FILE, command, *history_limit) {
                Ok(history_entries) => {
                    println!("History entry added.");
                    refresh_hints(shared_hints, sc_names, &history_entries);
                }
                Err(e) => eprintln!("{e:#}"),
            }
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::HistoryPin(index) => {
            maybe_backup(
                scripted,
                &[
                    SHORTCUTS_FILE,
                    HISTORY_FILE,
                    HISTORY_PINS_FILE,
                    HISTORY_USAGE_FILE,
                ],
            )?;

            match index.trim().parse::<usize>() {
                Ok(one_based_index) => {
                    let entries = load_history(HISTORY_FILE).unwrap_or_default();
                    if one_based_index == 0 || one_based_index > entries.len() {
                        eprintln!("History index out of range.");
                        return Ok(BuiltinOutcome::Continue);
                    }
                    let entry = entries[one_based_index - 1].clone();
                    let mut pins = load_string_set(HISTORY_PINS_FILE).unwrap_or_default();
                    if !pins.contains(&entry) {
                        pins.push(entry);
                        write_string_set(HISTORY_PINS_FILE, &pins)?;
                    }
                    println!("History entry pinned.");
                }
                Err(_) => eprintln!("Expected ':history pin <index>' with a numeric index."),
            }
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::HistoryUnpin(index) => {
            maybe_backup(
                scripted,
                &[
                    SHORTCUTS_FILE,
                    HISTORY_FILE,
                    HISTORY_PINS_FILE,
                    HISTORY_USAGE_FILE,
                ],
            )?;

            match index.trim().parse::<usize>() {
                Ok(one_based_index) => {
                    let entries = load_history(HISTORY_FILE).unwrap_or_default();
                    if one_based_index == 0 || one_based_index > entries.len() {
                        eprintln!("History index out of range.");
                        return Ok(BuiltinOutcome::Continue);
                    }
                    let entry = entries[one_based_index - 1].clone();
                    let mut pins = load_string_set(HISTORY_PINS_FILE).unwrap_or_default();
                    let original_len = pins.len();
                    pins.retain(|pin| pin != &entry);
                    if pins.len() != original_len {
                        write_string_set(HISTORY_PINS_FILE, &pins)?;
                        println!("History entry unpinned.");
                    } else {
                        println!("History entry was not pinned.");
                    }
                }
                Err(_) => eprintln!("Expected ':history unpin <index>' with a numeric index."),
            }
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::HistoryDel(index) => {
            maybe_backup(
                scripted,
                &[
                    SHORTCUTS_FILE,
                    HISTORY_FILE,
                    HISTORY_PINS_FILE,
                    HISTORY_USAGE_FILE,
                ],
            )?;

            let index = index.trim();
            if let Some((start, end)) = parse_history_range(index) {
                match delete_history_range(HISTORY_FILE, start, end) {
                    Ok(history_entries) => {
                        println!("History range deleted.");
                        refresh_hints(shared_hints, sc_names, &history_entries);
                    }
                    Err(e) => eprintln!("{e:#}"),
                }
                return Ok(BuiltinOutcome::Continue);
            }

            match index.parse::<usize>() {
                Ok(one_based_index) => match delete_history_entry(HISTORY_FILE, one_based_index) {
                    Ok(history_entries) => {
                        println!("History entry deleted.");
                        refresh_hints(shared_hints, sc_names, &history_entries);
                    }
                    Err(e) => eprintln!("{e:#}"),
                },
                Err(_) => eprintln!("Expected ':history del <index>' with a numeric index."),
            }
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::HistoryDedupe => {
            maybe_backup(
                scripted,
                &[
                    SHORTCUTS_FILE,
                    HISTORY_FILE,
                    HISTORY_PINS_FILE,
                    HISTORY_USAGE_FILE,
                ],
            )?;

            match dedupe_history(HISTORY_FILE) {
                Ok(history_entries) => {
                    println!("History deduped.");
                    refresh_hints(shared_hints, sc_names, &history_entries);
                }
                Err(e) => eprintln!("{e:#}"),
            }
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::HistoryClear => {
            maybe_backup(
                scripted,
                &[
                    SHORTCUTS_FILE,
                    HISTORY_FILE,
                    HISTORY_PINS_FILE,
                    HISTORY_USAGE_FILE,
                ],
            )?;

            match clear_history(HISTORY_FILE) {
                Ok(history_entries) => {
                    println!("History cleared.");
                    refresh_hints(shared_hints, sc_names, &history_entries);
                    write_string_set(HISTORY_PINS_FILE, &[])?;
                }
                Err(e) => eprintln!("{e:#}"),
            }
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::Find(query) => {
            if let Some(index) = query.trim().strip_prefix("run ") {
                match index.trim().parse::<usize>() {
                    Ok(one_based_index) => {
                        if one_based_index == 0 || one_based_index > last_find_results.len() {
                            eprintln!("Find index out of range.");
                            return Ok(BuiltinOutcome::Continue);
                        }

                        match &last_find_results[one_based_index - 1] {
                            FindResult::Shortcut(shortcut_name) => {
                                if let Some(shortcut) = find_shortcut(shortcuts, shortcut_name) {
                                    let _ = execute_shortcut(shortcut, config)?;
                                } else {
                                    eprintln!("Shortcut no longer exists: {shortcut_name}");
                                }
                            }
                            FindResult::History(command) => {
                                if run_executable_command(command, config, &[])? {
                                    let history_entries =
                                        append_history(HISTORY_FILE, command, *history_limit)?;
                                    refresh_hints(shared_hints, sc_names, &history_entries);
                                }
                            }
                        }
                    }
                    Err(_) => eprintln!("Expected ':find run <index>'."),
                }

                return Ok(BuiltinOutcome::Continue);
            }

            let needle = query.trim().to_ascii_lowercase();
            last_find_results.clear();

            for shortcut in shortcuts.iter() {
                let haystack = format!(
                    "{} {} {}",
                    shortcut.name,
                    shortcut.tags.join(" "),
                    shortcut.command
                )
                .to_ascii_lowercase();
                if haystack.contains(&needle) {
                    last_find_results.push(FindResult::Shortcut(shortcut.name.clone()));
                    println!(
                        "  {:>3} [shortcut] {}",
                        last_find_results.len(),
                        format_shortcut(shortcut)
                    );
                }
            }

            for entry in load_history(HISTORY_FILE).unwrap_or_default() {
                if entry.to_ascii_lowercase().contains(&needle) {
                    last_find_results.push(FindResult::History(entry.clone()));
                    println!("  {:>3} [history] {}", last_find_results.len(), entry);
                }
            }

            if last_find_results.is_empty() {
                println!("  (no results)");
            } else {
                println!("Use ':find run <index>' to execute a result.");
            }

            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::FindBang(query) => {
            let needle = query.trim().to_ascii_lowercase();
            let mut choices: Vec<FindResult> = Vec::new();

            for shortcut in shortcuts.iter() {
                let haystack = format!(
                    "{} {} {}",
                    shortcut.name,
                    shortcut.tags.join(" "),
                    shortcut.command
                )
                .to_ascii_lowercase();
                if haystack.contains(&needle) {
                    choices.push(FindResult::Shortcut(shortcut.name.clone()));
                    println!(
                        "  {:>3} [shortcut] {}",
                        choices.len(),
                        format_shortcut(shortcut)
                    );
                }
            }
            for entry in load_history(HISTORY_FILE).unwrap_or_default() {
                if entry.to_ascii_lowercase().contains(&needle) {
                    choices.push(FindResult::History(entry.clone()));
                    println!("  {:>3} [history] {}", choices.len(), entry);
                }
            }

            if choices.is_empty() {
                println!("  (no results)");
                return Ok(BuiltinOutcome::Continue);
            }

            print!("Pick index to run (empty to cancel): ");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let trimmed = input.trim();
            if trimmed.is_empty() {
                println!("Cancelled.");
                return Ok(BuiltinOutcome::Continue);
            }
            match trimmed.parse::<usize>() {
                Ok(index) if index > 0 && index <= choices.len() => match &choices[index - 1] {
                    FindResult::Shortcut(name) => {
                        if let Some(shortcut) = find_shortcut(shortcuts, name) {
                            let _ = execute_shortcut(shortcut, config)?;
                        }
                    }
                    FindResult::History(command) => {
                        if run_executable_command(command, config, &[])? {
                            let history_entries =
                                append_history(HISTORY_FILE, command, *history_limit)?;
                            refresh_hints(shared_hints, sc_names, &history_entries);
                        }
                    }
                },
                _ => eprintln!("Invalid selection."),
            }

            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::Export(path) => {
            let path = path.trim();
            if path.is_empty() {
                eprintln!("Expected ':export <file>'.");
                return Ok(BuiltinOutcome::Continue);
            }
            match export_state(path) {
                Ok(()) => println!("State exported to {}", path),
                Err(e) => eprintln!("{e:#}"),
            }
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::Import(path) => {
            maybe_backup(
                scripted,
                &[
                    CONFIG_FILE,
                    SHORTCUTS_FILE,
                    shortcuts_file,
                    HISTORY_FILE,
                    HISTORY_PINS_FILE,
                    HISTORY_USAGE_FILE,
                    PLACEHOLDER_VALUES_FILE,
                ],
            )?;

            let path = path.trim();
            if path.is_empty() {
                eprintln!("Expected ':import <file>'.");
                return Ok(BuiltinOutcome::Continue);
            }

            match import_state(path) {
                Ok(()) => {
                    *config = load_config(CONFIG_FILE)?;
                    *history_limit = config.history_limit();
                    *shortcuts_file = shortcuts_file_for_profile(&config.active_profile);
                    *shortcuts = load_shortcuts(shortcuts_file.as_str()).unwrap_or_default();
                    *sc_names = shortcut_names(shortcuts);
                    let history_entries = refresh_runtime_state(shortcuts, shared_hints)?;
                    let _ = prune_history(HISTORY_FILE, *history_limit)?;
                    println!(
                        "Imported state from {} ({} history entries).",
                        path,
                        history_entries.len()
                    );
                }
                Err(e) => eprintln!("{e:#}"),
            }

            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::Undo => {
            match restore_last_backup() {
                Ok(()) => {
                    *config = load_config(CONFIG_FILE)?;
                    *history_limit = config.history_limit();
                    *shortcuts_file = shortcuts_file_for_profile(&config.active_profile);
                    *shortcuts = load_shortcuts(shortcuts_file.as_str()).unwrap_or_default();
                    *sc_names = shortcut_names(shortcuts);
                    let history_entries = refresh_runtime_state(shortcuts, shared_hints)?;
                    println!(
                        "Restored backup ({} history entries).",
                        history_entries.len()
                    );
                }
                Err(e) => eprintln!("{e:#}"),
            }
            Ok(BuiltinOutcome::Continue)
        }
        BuiltinCommand::Unknown => {
            if scripted {
                bail!("Unknown script command")
            } else {
                println!("Unknown built-in command.");
                println!("Use :help to list available built-ins.");
                Ok(BuiltinOutcome::Continue)
            }
        }
    }
}

#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::{BuiltinCommand, builtin_help_text, parse_builtin_command};

    #[test]
    fn parser_handles_common_aliases() {
        assert_eq!(parse_builtin_command(":help"), Some(BuiltinCommand::Help));
        assert_eq!(parse_builtin_command(":?"), Some(BuiltinCommand::Help));
        assert_eq!(parse_builtin_command(":q"), Some(BuiltinCommand::Exit));
        assert_eq!(parse_builtin_command(":quit"), Some(BuiltinCommand::Exit));
        assert_eq!(parse_builtin_command(":r"), Some(BuiltinCommand::Reload));
        assert_eq!(
            parse_builtin_command(":s"),
            Some(BuiltinCommand::ShortcutsList)
        );
        assert_eq!(
            parse_builtin_command(":h"),
            Some(BuiltinCommand::HistoryList)
        );
    }

    #[test]
    fn parser_extracts_arguments() {
        assert_eq!(
            parse_builtin_command(":shortcuts tag k8s"),
            Some(BuiltinCommand::ShortcutsTag("k8s".to_owned()))
        );
        assert_eq!(
            parse_builtin_command(":shortcuts add x=echo hi"),
            Some(BuiltinCommand::ShortcutsAdd("x=echo hi".to_owned()))
        );
        assert_eq!(
            parse_builtin_command(":history del 2-5"),
            Some(BuiltinCommand::HistoryDel("2-5".to_owned()))
        );
        assert_eq!(
            parse_builtin_command(":find run 3"),
            Some(BuiltinCommand::Find("run 3".to_owned()))
        );
    }

    #[test]
    fn parser_handles_non_builtin_and_unknown() {
        assert_eq!(parse_builtin_command("kubectl get pods"), None);
        assert_eq!(
            parse_builtin_command(":something-else"),
            Some(BuiltinCommand::Unknown)
        );
    }

    #[test]
    fn builtin_help_text_snapshot_contains_core_lines() {
        let snapshot = builtin_help_text();
        assert!(snapshot.starts_with("Built-in commands (reserved ':' namespace):"));
        assert!(snapshot.contains(":set dry-run on|off"));
        assert!(snapshot.contains(":profile use <name>"));
        assert!(snapshot.contains(":find! <text>"));
        assert!(snapshot.contains(":exit | :quit | :q"));
    }
}

fn main() -> anyhow::Result<()> {
    let mut config = load_config(CONFIG_FILE)?;
    let mut history_limit = config.history_limit();
    let mut shortcuts_file = shortcuts_file_for_profile(&config.active_profile);
    let mut shortcuts = load_shortcuts(&shortcuts_file).unwrap_or_else(|e| {
        eprintln!("Warning: {e:#}");
        vec![]
    });
    let mut sc_names = shortcut_names(&shortcuts);
    let history_entries = prune_history(HISTORY_FILE, history_limit)?;
    let hint_names = build_hint_names(&sc_names, &history_entries);

    let shared_hints: SharedHints = Arc::new(RwLock::new(hint_names));

    let hint_style = Style::new().dimmed();
    let partial_highlight_style = Style::new().italic();
    let exact_highlight_style = Style::new().italic().fg(Color::LightGreen);
    let fuzzy_highlight_style = Style::new().italic().fg(Color::Yellow);
    let unmatched_highlight_style = Style::new().dimmed();

    let hinter = ShortcutHinter::new(Arc::clone(&shared_hints), hint_style);
    let highlighter = HintHighlighter::new(
        Arc::clone(&shared_hints),
        partial_highlight_style,
        exact_highlight_style,
        fuzzy_highlight_style,
        unmatched_highlight_style,
    );

    let mut line_editor = Reedline::create()
        .with_hinter(Box::new(hinter))
        .with_highlighter(Box::new(highlighter));

    let prompt = CmdStylePrompt;
    let mut last_find_results: Vec<FindResult> = Vec::new();

    let args = env::args().collect::<Vec<_>>();
    if args.len() > 1 {
        let scripted = args[1..].join(" ");
        if !scripted.starts_with(':') {
            run_executable_command(&scripted, &config, &[])?;
            let history_entries = append_history(HISTORY_FILE, &scripted, history_limit)?;
            refresh_hints(&shared_hints, &sc_names, &history_entries);
            return Ok(());
        }

        if !handle_script_shortcut_command(&scripted) {
            bail!("Unknown script command: {scripted}");
        }

        let Some(command) = parse_builtin_command(&scripted) else {
            bail!("Unsupported script command: {scripted}");
        };
        let _ = execute_builtin(
            command,
            true,
            &mut config,
            &mut history_limit,
            &mut shortcuts_file,
            &mut shortcuts,
            &mut sc_names,
            &shared_hints,
            &mut last_find_results,
        )?;
        return Ok(());
    }

    loop {
        match line_editor.read_line(&prompt) {
            Ok(Signal::Success(buffer)) => {
                let input = buffer.trim();

                if input.is_empty() {
                    continue;
                }

                if let Some(command) = parse_builtin_command(input) {
                    match execute_builtin(
                        command,
                        false,
                        &mut config,
                        &mut history_limit,
                        &mut shortcuts_file,
                        &mut shortcuts,
                        &mut sc_names,
                        &shared_hints,
                        &mut last_find_results,
                    )? {
                        BuiltinOutcome::Continue => continue,
                        BuiltinOutcome::Exit => break,
                    }
                }

                // ── Shortcut or raw command ──────────────────────────────────────
                if let Some(shortcut) = find_shortcut(&shortcuts, input) {
                    let _ = execute_shortcut(shortcut, &config)?;
                } else {
                    match run_executable_command(input, &config, &[]) {
                        Ok(true) => match append_history(HISTORY_FILE, input, history_limit) {
                            Ok(history_entries) => {
                                refresh_hints(&shared_hints, &sc_names, &history_entries);
                            }
                            Err(error) => eprintln!("{error:#}"),
                        },
                        Ok(false) => {}
                        Err(error) => eprintln!("{error:#}"),
                    }
                }
            }
            Ok(Signal::CtrlD) => break,
            _ => {}
        }
    }

    Ok(())
}
