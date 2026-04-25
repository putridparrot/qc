use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use rpassword::read_password;

#[derive(Clone, Debug)]
pub struct Shortcut {
    pub name: String,
    pub tags: Vec<String>,
    pub command: String,
}

#[derive(Clone, Debug)]
pub struct TemplateField {
    pub name: String,
    pub default_value: Option<String>,
    pub sensitive: bool,
}

fn parse_shortcut_key(key: &str) -> (String, Vec<String>) {
    let key = key.trim();
    let Some(open_index) = key.find('[') else {
        return (key.to_owned(), Vec::new());
    };

    let Some(close_index) = key.rfind(']') else {
        return (key.to_owned(), Vec::new());
    };

    if close_index <= open_index {
        return (key.to_owned(), Vec::new());
    }

    let name = key[..open_index].trim();
    let raw_tags = key[open_index + 1..close_index].trim();

    if name.is_empty() {
        return (key.to_owned(), Vec::new());
    }

    let tags = raw_tags
        .split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    (name.to_owned(), tags)
}

fn format_shortcut_key(name: &str, tags: &[String]) -> String {
    if tags.is_empty() {
        name.to_owned()
    } else {
        format!("{}[{}]", name, tags.join(","))
    }
}

fn parse_template_field(raw: &str) -> Option<TemplateField> {
    let mut raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    let sensitive = raw.ends_with('!');
    if sensitive {
        raw = raw.strip_suffix('!').unwrap_or(raw).trim();
    }

    let (name, default_value) = match raw.split_once('?') {
        Some((name, default)) => {
            let name = name.trim();
            let default = default.trim();
            let default_value = if default.is_empty() {
                None
            } else {
                Some(default.to_owned())
            };
            (name, default_value)
        }
        None => (raw, None),
    };

    if name.is_empty() {
        return None;
    }

    Some(TemplateField {
        name: name.to_owned(),
        default_value,
        sensitive,
    })
}

pub fn load_shortcuts(path: impl AsRef<Path>) -> Result<Vec<Shortcut>> {
    let path = path.as_ref();

    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read shortcut file: {}", path.display()))?;

    let mut shortcuts = Vec::new();

    for (index, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((name, command)) = line.split_once('=') else {
            bail!(
                "Invalid shortcut definition at {}:{}; expected name=command",
                path.display(),
                index + 1
            );
        };

        let (name, tags) = parse_shortcut_key(name);
        let command = command.trim();

        if name.is_empty() || command.is_empty() {
            bail!(
                "Invalid shortcut definition at {}:{}; name and command must be non-empty",
                path.display(),
                index + 1
            );
        }

        shortcuts.push(Shortcut {
            name,
            tags,
            command: command.to_owned(),
        });
    }

    Ok(shortcuts)
}

pub fn shortcut_names(shortcuts: &[Shortcut]) -> Vec<String> {
    shortcuts
        .iter()
        .map(|shortcut| shortcut.name.clone())
        .collect()
}

fn write_shortcuts(path: impl AsRef<Path>, shortcuts: &[Shortcut]) -> Result<()> {
    let path = path.as_ref();
    let content = if shortcuts.is_empty() {
        String::new()
    } else {
        let lines = shortcuts
            .iter()
            .map(|shortcut| {
                format!(
                    "{}={}",
                    format_shortcut_key(&shortcut.name, &shortcut.tags),
                    shortcut.command
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("{lines}\n")
    };

    fs::write(path, content)
        .with_context(|| format!("Failed to write shortcut file: {}", path.display()))
}

pub fn add_shortcut_with_tags(
    path: impl AsRef<Path>,
    name: &str,
    command: &str,
    tags: &[String],
) -> Result<(Vec<Shortcut>, bool)> {
    let path = path.as_ref();
    let mut shortcuts = load_shortcuts(path)?;

    if name.trim().is_empty() || command.trim().is_empty() {
        bail!("Shortcut name and command must be non-empty");
    }

    if let Some(existing) = shortcuts.iter_mut().find(|shortcut| shortcut.name == name) {
        existing.command = command.to_owned();
        existing.tags = tags.to_vec();
        write_shortcuts(path, &shortcuts)?;
        return Ok((shortcuts, false));
    }

    shortcuts.push(Shortcut {
        name: name.to_owned(),
        tags: tags.to_vec(),
        command: command.to_owned(),
    });

    write_shortcuts(path, &shortcuts)?;
    Ok((shortcuts, true))
}

pub fn delete_shortcut(path: impl AsRef<Path>, name: &str) -> Result<(Vec<Shortcut>, bool)> {
    let path = path.as_ref();
    let mut shortcuts = load_shortcuts(path)?;
    let original_len = shortcuts.len();
    shortcuts.retain(|shortcut| shortcut.name != name);

    if shortcuts.len() == original_len {
        return Ok((shortcuts, false));
    }

    write_shortcuts(path, &shortcuts)?;
    Ok((shortcuts, true))
}

pub fn find_shortcut<'a>(shortcuts: &'a [Shortcut], input: &str) -> Option<&'a Shortcut> {
    shortcuts.iter().find(|shortcut| shortcut.name == input)
}

pub fn filter_shortcuts_by_tag<'a>(shortcuts: &'a [Shortcut], tag: &str) -> Vec<&'a Shortcut> {
    shortcuts
        .iter()
        .filter(|shortcut| {
            shortcut
                .tags
                .iter()
                .any(|shortcut_tag| shortcut_tag.eq_ignore_ascii_case(tag))
        })
        .collect()
}

pub fn run_command(command_line: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| String::from("cmd"));
        let mut command = Command::new(shell);
        command.arg("/C").arg(command_line);
        command
    };

    #[cfg(not(target_os = "windows"))]
    let mut command = {
        let mut command = Command::new("sh");
        command.arg("-c").arg(command_line);
        command
    };

    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("Failed to run command: {command_line}"))?;

    if !status.success() {
        bail!("Command exited with status {status}: {command_line}");
    }

    Ok(())
}

/// Extracts `{placeholder}` names from a command template, in order, without duplicates.
pub fn extract_template_fields(command: &str) -> Vec<TemplateField> {
    let mut placeholders = Vec::new();
    let mut chars = command.chars();

    while let Some(ch) = chars.next() {
        if ch == '{' {
            let mut token = String::new();
            let mut closed = false;
            for inner in chars.by_ref() {
                if inner == '}' {
                    closed = true;
                    break;
                }
                token.push(inner);
            }

            if !closed {
                continue;
            }

            let Some(field) = parse_template_field(&token) else {
                continue;
            };

            if placeholders
                .iter()
                .all(|existing: &TemplateField| existing.name != field.name)
            {
                placeholders.push(field);
            }
        }
    }

    placeholders
}

/// Replaces every `{key}` occurrence in `command` with the matching value from `args`.
pub fn expand_template(command: &str, args: &HashMap<String, String>) -> String {
    let mut result = command.to_owned();
    for (key, value) in args {
        result = result.replace(&format!("{{{key}}}"), value);
    }
    result
}

pub fn load_placeholder_values(path: impl AsRef<Path>) -> Result<HashMap<String, Vec<String>>> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let content = fs::read_to_string(path).with_context(|| {
        format!(
            "Failed to read placeholder history file: {}",
            path.display()
        )
    })?;

    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }

        map.entry(key.to_owned())
            .or_default()
            .push(value.to_owned());
    }

    Ok(map)
}

pub fn save_placeholder_values(
    path: impl AsRef<Path>,
    values: &HashMap<String, Vec<String>>,
) -> Result<()> {
    let path = path.as_ref();
    let mut keys = values.keys().cloned().collect::<Vec<_>>();
    keys.sort();

    let mut lines = Vec::new();
    for key in keys {
        if let Some(entries) = values.get(&key) {
            for value in entries {
                lines.push(format!("{}={}", key, value));
            }
        }
    }

    let content = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };

    fs::write(path, content).with_context(|| {
        format!(
            "Failed to write placeholder history file: {}",
            path.display()
        )
    })
}

fn remember_placeholder_value(
    values: &mut HashMap<String, Vec<String>>,
    key: &str,
    value: &str,
    max_values: usize,
) {
    let entry = values.entry(key.to_owned()).or_default();
    entry.retain(|existing| existing != value);
    entry.insert(0, value.to_owned());
    if entry.len() > max_values {
        entry.truncate(max_values);
    }
}

/// Prompts the user for each `{placeholder}` found in `command` and returns
/// the fully-expanded command string.
pub fn prompt_for_args(command: &str, placeholder_file: impl AsRef<Path>) -> Result<String> {
    let placeholders = extract_template_fields(command);
    if placeholders.is_empty() {
        return Ok(command.to_owned());
    }

    let placeholder_file = placeholder_file.as_ref();
    let mut remembered_values = load_placeholder_values(placeholder_file)?;
    let mut args = HashMap::new();

    for placeholder in &placeholders {
        let remembered = remembered_values
            .get(&placeholder.name)
            .cloned()
            .unwrap_or_default();
        let mut remembered_index = 0usize;

        loop {
            let remembered_default = remembered.get(remembered_index).cloned();
            let default = remembered_default.or_else(|| placeholder.default_value.clone());

            if placeholder.sensitive {
                print!("  {} [hidden]: ", placeholder.name);
            } else if let Some(default) = &default {
                print!(
                    "  {} [default: {}] (enter to accept, ! to cycle): ",
                    placeholder.name, default
                );
            } else {
                print!("  {}: ", placeholder.name);
            }

            io::stdout().flush().context("Failed to flush stdout")?;
            let raw_input = if placeholder.sensitive {
                read_password().context("Failed to read hidden input")?
            } else {
                let mut input = String::new();
                io::stdin()
                    .read_line(&mut input)
                    .context("Failed to read input")?;
                input
            };
            let input = raw_input.trim();

            if !placeholder.sensitive && input == "!" {
                if remembered.is_empty() {
                    println!("  No previous values for '{}'.", placeholder.name);
                } else {
                    remembered_index = (remembered_index + 1) % remembered.len();
                }
                continue;
            }

            let value = if input.is_empty() {
                default.unwrap_or_default()
            } else {
                input.to_owned()
            };

            if value.is_empty() {
                println!("  '{}' is required.", placeholder.name);
                continue;
            }

            if !placeholder.sensitive {
                remember_placeholder_value(&mut remembered_values, &placeholder.name, &value, 10);
            }
            args.insert(placeholder.name.clone(), value);
            break;
        }
    }

    save_placeholder_values(placeholder_file, &remembered_values)?;

    Ok(expand_template(command, &args))
}

/// Returns `true` when `command` matches a known dangerous pattern.
pub fn is_dangerous(command: &str) -> bool {
    let lower = command.to_lowercase();
    let patterns: &[&str] = &[
        "rm -rf",
        "rm -fr",
        "del /f",
        "del /s",
        "del /q",
        "format ",
        "mkfs",
        "dd if=",
        "drop table",
        "drop database",
        "truncate table",
        ":(){:|:&};:", // fork bomb
    ];
    patterns.iter().any(|p| lower.contains(p))
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
        std::env::temp_dir().join(format!("qc-shortcuts-{name}-{ts}.txt"))
    }

    #[test]
    fn load_shortcuts_parses_tags() {
        let path = temp_file("tags");
        fs::write(
            &path,
            "kube-logs[k8s,debug]=kubectl logs deployment/{app} --follow\n",
        )
        .expect("write shortcuts");

        let shortcuts = load_shortcuts(&path).expect("load shortcuts");
        assert_eq!(shortcuts.len(), 1);
        assert_eq!(shortcuts[0].name, "kube-logs");
        assert_eq!(shortcuts[0].tags, vec!["k8s", "debug"]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn extract_template_fields_parses_defaults() {
        let fields =
            extract_template_fields("kubectl logs {app} -n {namespace?default} --tail={lines?200}");
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "app");
        assert_eq!(fields[0].default_value, None);
        assert!(!fields[0].sensitive);
        assert_eq!(fields[1].name, "namespace");
        assert_eq!(fields[1].default_value.as_deref(), Some("default"));
        assert!(!fields[1].sensitive);
        assert_eq!(fields[2].name, "lines");
        assert_eq!(fields[2].default_value.as_deref(), Some("200"));
        assert!(!fields[2].sensitive);
    }

    #[test]
    fn extract_template_fields_parses_sensitive() {
        let fields = extract_template_fields(
            "curl -H 'Authorization: Bearer {token!}' -H 'x={ns?default!}'",
        );
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "token");
        assert!(fields[0].sensitive);
        assert_eq!(fields[0].default_value, None);
        assert_eq!(fields[1].name, "ns");
        assert!(fields[1].sensitive);
        assert_eq!(fields[1].default_value.as_deref(), Some("default"));
    }

    #[test]
    fn expand_template_replaces_all_keys() {
        let mut args = HashMap::new();
        args.insert("app".to_owned(), "my-api".to_owned());
        args.insert("ns".to_owned(), "default".to_owned());

        let expanded = expand_template("kubectl logs deploy/{app} -n {ns}", &args);
        assert_eq!(expanded, "kubectl logs deploy/my-api -n default");
    }

    #[test]
    fn filter_shortcuts_by_tag_is_case_insensitive() {
        let shortcuts = vec![
            Shortcut {
                name: "a".to_owned(),
                tags: vec!["k8s".to_owned(), "debug".to_owned()],
                command: "echo a".to_owned(),
            },
            Shortcut {
                name: "b".to_owned(),
                tags: vec!["git".to_owned()],
                command: "echo b".to_owned(),
            },
        ];

        let matches = filter_shortcuts_by_tag(&shortcuts, "K8S");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "a");
    }

    #[test]
    fn add_shortcut_with_tags_updates_existing() {
        let path = temp_file("add-update");
        fs::write(&path, "kube[old]=echo old\n").expect("seed shortcuts");

        let (shortcuts, added) = add_shortcut_with_tags(
            &path,
            "kube",
            "echo new",
            &["k8s".to_owned(), "ops".to_owned()],
        )
        .expect("add/update shortcut");

        assert!(!added);
        assert_eq!(shortcuts.len(), 1);
        assert_eq!(shortcuts[0].command, "echo new");
        assert_eq!(shortcuts[0].tags, vec!["k8s", "ops"]);

        let _ = fs::remove_file(path);
    }
}
