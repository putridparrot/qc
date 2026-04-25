use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryLimit {
    Disabled,
    Unlimited,
    Limited(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafetyPolicy {
    Warn,
    Confirm,
    Block,
}

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub max_history_items: isize,
    pub safety_policy: SafetyPolicy,
    pub dry_run: bool,
    pub active_profile: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            max_history_items: 100,
            safety_policy: SafetyPolicy::Confirm,
            dry_run: false,
            active_profile: "default".to_owned(),
        }
    }
}

impl AppConfig {
    pub fn history_limit(&self) -> HistoryLimit {
        match self.max_history_items {
            -1 => HistoryLimit::Unlimited,
            0 => HistoryLimit::Disabled,
            value if value > 0 => HistoryLimit::Limited(value as usize),
            _ => HistoryLimit::Disabled,
        }
    }
}

pub fn load_config(path: impl AsRef<Path>) -> Result<AppConfig> {
    let path = path.as_ref();

    if !path.exists() {
        return Ok(AppConfig::default());
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;

    let mut config = AppConfig::default();

    for (index, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            bail!(
                "Invalid config entry at {}:{}; expected key=value",
                path.display(),
                index + 1
            );
        };

        match key.trim() {
            "max_history_items" => {
                config.max_history_items = value.trim().parse().with_context(|| {
                    format!(
                        "Invalid max_history_items value at {}:{}",
                        path.display(),
                        index + 1
                    )
                })?;
                if config.max_history_items < -1 {
                    bail!(
                        "Invalid max_history_items value at {}:{}; expected -1, 0, or a positive integer",
                        path.display(),
                        index + 1
                    );
                }
            }
            "safety_policy" => {
                config.safety_policy = match value.trim().to_ascii_lowercase().as_str() {
                    "warn" => SafetyPolicy::Warn,
                    "confirm" => SafetyPolicy::Confirm,
                    "block" => SafetyPolicy::Block,
                    _ => bail!(
                        "Invalid safety_policy value at {}:{}; expected warn, confirm, or block",
                        path.display(),
                        index + 1
                    ),
                }
            }
            "dry_run" => {
                config.dry_run = match value.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" => true,
                    "false" | "0" | "no" | "off" => false,
                    _ => bail!(
                        "Invalid dry_run value at {}:{}; expected true or false",
                        path.display(),
                        index + 1
                    ),
                }
            }
            "active_profile" => {
                let profile = value.trim();
                if profile.is_empty() {
                    bail!(
                        "Invalid active_profile at {}:{}; value must be non-empty",
                        path.display(),
                        index + 1
                    );
                }
                config.active_profile = profile.to_owned();
            }
            unknown => {
                bail!(
                    "Unknown config key '{}' at {}:{}",
                    unknown,
                    path.display(),
                    index + 1
                );
            }
        }
    }

    Ok(config)
}

pub fn save_config(path: impl AsRef<Path>, config: &AppConfig) -> Result<()> {
    let path = path.as_ref();
    let safety_policy = match config.safety_policy {
        SafetyPolicy::Warn => "warn",
        SafetyPolicy::Confirm => "confirm",
        SafetyPolicy::Block => "block",
    };

    let content = format!(
        "max_history_items={}\n\
safety_policy={}\n\
dry_run={}\n\
active_profile={}\n",
        config.max_history_items, safety_policy, config.dry_run, config.active_profile
    );

    fs::write(path, content)
        .with_context(|| format!("Failed to write config file: {}", path.display()))
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
        std::env::temp_dir().join(format!("qc-{name}-{ts}.txt"))
    }

    #[test]
    fn history_limit_mapping_works() {
        let disabled = AppConfig {
            max_history_items: 0,
            safety_policy: SafetyPolicy::Confirm,
            dry_run: false,
            active_profile: "default".to_owned(),
        };
        let unlimited = AppConfig {
            max_history_items: -1,
            safety_policy: SafetyPolicy::Confirm,
            dry_run: false,
            active_profile: "default".to_owned(),
        };
        let limited = AppConfig {
            max_history_items: 25,
            safety_policy: SafetyPolicy::Confirm,
            dry_run: false,
            active_profile: "default".to_owned(),
        };

        assert_eq!(disabled.history_limit(), HistoryLimit::Disabled);
        assert_eq!(unlimited.history_limit(), HistoryLimit::Unlimited);
        assert_eq!(limited.history_limit(), HistoryLimit::Limited(25));
    }

    #[test]
    fn load_config_parses_values() {
        let path = temp_file("config-valid");
        fs::write(
            &path,
            "max_history_items=10\nsafety_policy=block\ndry_run=true\nactive_profile=prod\n",
        )
        .expect("write config");

        let cfg = load_config(&path).expect("load config");
        assert_eq!(cfg.max_history_items, 10);
        assert_eq!(cfg.safety_policy, SafetyPolicy::Block);
        assert!(cfg.dry_run);
        assert_eq!(cfg.active_profile, "prod");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn load_config_rejects_invalid_safety_policy() {
        let path = temp_file("config-invalid-safety");
        fs::write(&path, "safety_policy=banana\n").expect("write config");

        let err = load_config(&path).expect_err("expected parse error");
        let msg = format!("{err:#}");
        assert!(msg.contains("Invalid safety_policy value"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn save_config_round_trips() {
        let path = temp_file("config-save-roundtrip");
        let config = AppConfig {
            max_history_items: 7,
            safety_policy: SafetyPolicy::Warn,
            dry_run: true,
            active_profile: "dev".to_owned(),
        };

        save_config(&path, &config).expect("save config");
        let reloaded = load_config(&path).expect("reload config");
        assert_eq!(reloaded.max_history_items, 7);
        assert_eq!(reloaded.safety_policy, SafetyPolicy::Warn);
        assert!(reloaded.dry_run);
        assert_eq!(reloaded.active_profile, "dev");

        let _ = fs::remove_file(path);
    }
}
