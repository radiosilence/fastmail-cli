use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub api_token: Option<String>,
}

impl Config {
    fn config_dir() -> Result<PathBuf> {
        let dir = dirs::home_dir()
            .ok_or_else(|| Error::Config("Could not find home directory".into()))?
            .join(".fastmail-cli");
        Ok(dir)
    }

    fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path)?;
        let config: Config = serde_json::from_str(&content)?;
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
        let content = serde_json::to_string_pretty(self)?;
        fs::write(&path, content)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }

        Ok(())
    }

    pub fn get_token(&self) -> Result<&str> {
        self.api_token.as_deref().ok_or(Error::NotAuthenticated)
    }

    pub fn set_token(&mut self, token: String) {
        self.api_token = Some(token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(config.api_token.is_none());
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
            api_token: Some("test-token".to_string()),
        };
        assert_eq!(config.get_token().unwrap(), "test-token");
    }

    #[test]
    fn test_config_set_token() {
        let mut config = Config::default();
        config.set_token("new-token".to_string());
        assert_eq!(config.api_token, Some("new-token".to_string()));
    }

    #[test]
    fn test_config_serialize_deserialize() {
        let config = Config {
            api_token: Some("test-token".to_string()),
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.api_token, Some("test-token".to_string()));
    }
}
