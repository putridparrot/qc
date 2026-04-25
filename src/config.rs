use std::collections::HashMap;
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExecutionPolicy {
    pub allow_patterns: Vec<String>,
    pub deny_patterns: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub max_history_items: isize,
    pub safety_policy: SafetyPolicy,
    pub dry_run: bool,
    pub active_profile: String,
    pub profile_policies: HashMap<String, ExecutionPolicy>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            max_history_items: 100,
            safety_policy: SafetyPolicy::Confirm,
            dry_run: false,
            active_profile: "default".to_owned(),
            profile_policies: HashMap::new(),
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

    pub fn policy_for_profile(&self, profile: &str) -> ExecutionPolicy {
        self.profile_policies
            .get(profile)
            .cloned()
            .unwrap_or_default()
    }
}

fn parse_patterns(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
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
                if let Some(rest) = unknown.strip_prefix("policy.") {
                    let Some((profile, policy_kind)) = rest.rsplit_once('.') else {
                        bail!(
                            "Invalid policy config key '{}' at {}:{}; expected policy.<profile>.allow|deny",
                            unknown,
                            path.display(),
                            index + 1
                        );
                    };
                    if profile.trim().is_empty() {
                        bail!(
                            "Invalid policy profile in key '{}' at {}:{}",
                            unknown,
                            path.display(),
                            index + 1
                        );
                    }

                    let policy = config
                        .profile_policies
                        .entry(profile.trim().to_owned())
                        .or_default();

                    match policy_kind.trim() {
                        "allow" => policy.allow_patterns = parse_patterns(value),
                        "deny" => policy.deny_patterns = parse_patterns(value),
                        _ => bail!(
                            "Invalid policy key '{}' at {}:{}; expected allow or deny suffix",
                            unknown,
                            path.display(),
                            index + 1
                        ),
                    }
                } else {
                    bail!(
                        "Unknown config key '{}' at {}:{}",
                        unknown,
                        path.display(),
                        index + 1
                    );
                }
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

    let mut content = format!(
        "max_history_items={}\n\
safety_policy={}\n\
dry_run={}\n\
active_profile={}\n",
        config.max_history_items, safety_policy, config.dry_run, config.active_profile
    );

    let mut profiles = config.profile_policies.keys().cloned().collect::<Vec<_>>();
    profiles.sort();
    for profile in profiles {
        if let Some(policy) = config.profile_policies.get(&profile) {
            if !policy.allow_patterns.is_empty() {
                content.push_str(&format!(
                    "policy.{}.allow={}\n",
                    profile,
                    policy.allow_patterns.join(",")
                ));
            }
            if !policy.deny_patterns.is_empty() {
                content.push_str(&format!(
                    "policy.{}.deny={}\n",
                    profile,
                    policy.deny_patterns.join(",")
                ));
            }
        }
    }

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
            profile_policies: HashMap::new(),
        };
        let unlimited = AppConfig {
            max_history_items: -1,
            safety_policy: SafetyPolicy::Confirm,
            dry_run: false,
            active_profile: "default".to_owned(),
            profile_policies: HashMap::new(),
        };
        let limited = AppConfig {
            max_history_items: 25,
            safety_policy: SafetyPolicy::Confirm,
            dry_run: false,
            active_profile: "default".to_owned(),
            profile_policies: HashMap::new(),
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
            "max_history_items=10\nsafety_policy=block\ndry_run=true\nactive_profile=prod\npolicy.prod.allow=kubectl,get\npolicy.prod.deny=rm -rf,drop table\n",
        )
        .expect("write config");

        let cfg = load_config(&path).expect("load config");
        assert_eq!(cfg.max_history_items, 10);
        assert_eq!(cfg.safety_policy, SafetyPolicy::Block);
        assert!(cfg.dry_run);
        assert_eq!(cfg.active_profile, "prod");
        let prod = cfg.policy_for_profile("prod");
        assert_eq!(prod.allow_patterns, vec!["kubectl", "get"]);
        assert_eq!(prod.deny_patterns, vec!["rm -rf", "drop table"]);

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
            profile_policies: HashMap::from([(
                "dev".to_owned(),
                ExecutionPolicy {
                    allow_patterns: vec!["kubectl".to_owned()],
                    deny_patterns: vec!["rm -rf".to_owned()],
                },
            )]),
        };

        save_config(&path, &config).expect("save config");
        let reloaded = load_config(&path).expect("reload config");
        assert_eq!(reloaded.max_history_items, 7);
        assert_eq!(reloaded.safety_policy, SafetyPolicy::Warn);
        assert!(reloaded.dry_run);
        assert_eq!(reloaded.active_profile, "dev");
        let dev = reloaded.policy_for_profile("dev");
        assert_eq!(dev.allow_patterns, vec!["kubectl"]);
        assert_eq!(dev.deny_patterns, vec!["rm -rf"]);

        let _ = fs::remove_file(path);
    }
}
