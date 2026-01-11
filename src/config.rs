use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

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
        fs::create_dir_all(&dir)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }

        let path = Self::config_path()?;
        let content = toml::to_string_pretty(self)
            .map_err(|e| Error::Config(format!("Failed to serialize config: {}", e)))?;
        fs::write(&path, content)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }

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
        let config = Config::default();
        let result = config.get_token();
        assert!(matches!(result, Err(Error::NotAuthenticated)));
    }

    #[test]
    fn test_config_get_token_some() {
        let config = Config {
            core: CoreConfig {
                api_token: Some("test-token".to_string()),
            },
            ..Default::default()
        };
        assert_eq!(config.get_token().unwrap(), "test-token");
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
