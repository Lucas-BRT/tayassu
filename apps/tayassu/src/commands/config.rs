use crate::app::{App, Execute};
use anyhow::{Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use std::io::Write;
use tayassu_store::AppConfig;

/// A user-editable `config.toml` setting.
trait ConfigSetting {
    /// Key name as shown to the user.
    fn key(&self) -> &'static str;
    /// Current value rendered as text.
    fn get(&self, config: &AppConfig) -> String;
    /// Parses `raw` and stores it.
    fn set(&self, config: &mut AppConfig, raw: &str) -> Result<()>;
}

/// Settings editable through `tayassu config`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Setting {
    /// Default account selection policy.
    DefaultPolicy,
    /// Background release check and terminal update notice (`on`/`off`).
    UpdateCheck,
}

impl Setting {
    const ALL: [Self; 2] = [Self::DefaultPolicy, Self::UpdateCheck];
}

impl ConfigSetting for Setting {
    fn key(&self) -> &'static str {
        match self {
            Self::DefaultPolicy => "default-policy",
            Self::UpdateCheck => "update-check",
        }
    }

    fn get(&self, config: &AppConfig) -> String {
        match self {
            Self::DefaultPolicy => config.default_policy.to_string(),
            Self::UpdateCheck => if config.update_check { "on" } else { "off" }.to_owned(),
        }
    }

    fn set(&self, config: &mut AppConfig, raw: &str) -> Result<()> {
        match self {
            Self::DefaultPolicy => config.default_policy = raw.parse()?,
            Self::UpdateCheck => {
                config.update_check = match raw.trim().to_ascii_lowercase().as_str() {
                    "on" | "true" | "yes" | "1" => true,
                    "off" | "false" | "no" | "0" => false,
                    _ => bail!("invalid boolean '{raw}'; expected on/off"),
                };
            }
        }
        Ok(())
    }
}

/// Read or write global settings without editing `config.toml` by hand.
#[derive(Debug, Args)]
pub struct Config {
    #[command(subcommand)]
    action: ConfigAction,
}

#[derive(Debug, Subcommand)]
enum ConfigAction {
    /// List all settings.
    Ls,
    /// Print one setting.
    Get {
        /// Setting name.
        key: Setting,
    },
    /// Change one setting.
    Set {
        /// Setting name.
        key: Setting,
        /// New value.
        value: String,
    },
}

impl Execute for Config {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        match self.action {
            ConfigAction::Ls => {
                for setting in Setting::ALL {
                    writeln!(out, "{} = {}", setting.key(), setting.get(&app.config))?;
                }
            }
            ConfigAction::Get { key } => writeln!(out, "{}", key.get(&app.config))?,
            ConfigAction::Set { key, value } => {
                key.set(&mut app.config, &value)?;
                app.save_config()?;
                writeln!(out, "{} = {}", key.key(), key.get(&app.config))?;
            }
        }
        Ok(())
    }
}
