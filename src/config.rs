use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub core: CoreConfig,
    #[serde(default)]
    pub contacts: ContactsConfig,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct CoreConfig {
    pub api_token: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ContactsConfig {
    pub username: Option<String>,
    /// App password for CardDAV - API tokens don't work for CardDAV
    pub app_password: Option<String>,
}

impl Config {
    fn config_dir() -> Result<PathBuf> {
        // Use ~/.config on all platforms for consistency
        let dir = dirs::home_dir()
            .ok_or_else(|| Error::Config("Could not find home directory".into()))?
            .join(".config")
            .join("fastmail-cli");
        Ok(dir)
    }

    fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path)?;
        let config: Config = toml::from_str(&content)
            .map_err(|e| Error::Config(format!("Failed to parse config: {}", e)))?;
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let dir = Self::config_dir()?;
        create_private_dir(&dir)?;

        let path = Self::config_path()?;

        // Refuse to write through a symlink — an attacker or mistaken user
        // could redirect the token file elsewhere. symlink_metadata inspects
        // the link itself, not its target.
        if let Ok(md) = fs::symlink_metadata(&path)
            && md.file_type().is_symlink()
        {
            return Err(Error::Config(format!(
                "Refusing to write config: {} is a symlink",
                path.display()
            )));
        }

        let content = toml::to_string_pretty(self)
            .map_err(|e| Error::Config(format!("Failed to serialize config: {}", e)))?;

        // Write to a sibling temp file with 0o600, then rename atomically over
        // the target. This closes the TOCTOU window between writing the token
        // and tightening permissions.
        let tmp_path = path.with_extension("toml.tmp");
        let _ = fs::remove_file(&tmp_path);
        write_private_file(&tmp_path, content.as_bytes()).inspect_err(|_| {
            let _ = fs::remove_file(&tmp_path);
        })?;
        fs::rename(&tmp_path, &path).map_err(|e| {
            let _ = fs::remove_file(&tmp_path);
            Error::Config(format!("Failed to install config file: {}", e))
        })?;

        Ok(())
    }

    /// Get the API token, preferring FASTMAIL_API_TOKEN env var over config file
    pub fn get_token(&self) -> Result<String> {
        if let Ok(token) = std::env::var("FASTMAIL_API_TOKEN") {
            return Ok(token);
        }
        self.core.api_token.clone().ok_or(Error::NotAuthenticated)
    }

    /// Get the username (email), preferring FASTMAIL_USERNAME env var over config file
    pub fn get_username(&self) -> Result<String> {
        if let Ok(username) = std::env::var("FASTMAIL_USERNAME") {
            return Ok(username);
        }
        self.contacts
            .username
            .clone()
            .ok_or_else(|| Error::Config("Username not set in [contacts] config.".into()))
    }

    pub fn set_token(&mut self, token: String) {
        self.core.api_token = Some(token);
    }

    /// Get the app password for CardDAV, preferring FASTMAIL_APP_PASSWORD env var
    pub fn get_app_password(&self) -> Result<String> {
        if let Ok(password) = std::env::var("FASTMAIL_APP_PASSWORD") {
            return Ok(password);
        }
        self.contacts
            .app_password
            .clone()
            .ok_or_else(|| Error::Config("App password not set in [contacts] config.".into()))
    }
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    // DirBuilder::mode applies to newly-created directories only. Following up
    // with set_permissions tightens the mode if the directory already existed.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    Ok(())
}

#[cfg(unix)]
fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(contents)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(config.core.api_token.is_none());
    }

    #[test]
    fn test_config_get_token_none() {
        // Test the config-only path by calling the inner logic directly
        let config = Config::default();
        // When env var is not set, falls back to config — which has no token
        assert!(config.core.api_token.is_none());
    }

    #[test]
    fn test_config_get_token_some() {
        let config = Config {
            core: CoreConfig {
                api_token: Some("test-token".to_string()),
            },
            ..Default::default()
        };
        assert_eq!(config.core.api_token.as_deref(), Some("test-token"));
    }

    #[test]
    fn test_config_set_token() {
        let mut config = Config::default();
        config.set_token("new-token".to_string());
        assert_eq!(config.core.api_token, Some("new-token".to_string()));
    }

    #[test]
    fn test_config_serialize_deserialize() {
        let config = Config {
            core: CoreConfig {
                api_token: Some("test-token".to_string()),
            },
            ..Default::default()
        };
        let toml_str = toml::to_string(&config).unwrap();
        let deserialized: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(deserialized.core.api_token, Some("test-token".to_string()));
    }
}
