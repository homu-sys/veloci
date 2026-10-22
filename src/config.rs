use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repo {
    pub name: String,

    pub url: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub repo: Vec<Repo>,

    #[serde(default, flatten)]
    pub extra: HashMap<String, toml::Value>,
}

impl Config {
    pub fn path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join("veloci").join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing the configuration")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn set_repo(&mut self, repo: Repo) {
        match self.repo.iter_mut().find(|r| r.name == repo.name) {
            Some(existing) => *existing = repo,
            None => self.repo.push(repo),
        }
    }
}

pub fn cache_dir() -> PathBuf {
    let base = dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".veloci"));
    base.join("veloci")
}

pub fn index_path() -> PathBuf {
    cache_dir().join("index.veloci")
}

pub fn package_cache() -> PathBuf {
    cache_dir().join("pkg")
}

pub fn repo_name_from_url(url: &str) -> String {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("repo");
    let host = host.split('.').next().unwrap_or(host);
    if host.is_empty() {
        "repo".to_string()
    } else {
        host.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_a_repository_after_its_host() {
        assert_eq!(
            repo_name_from_url(
                "https://raw.githubusercontent.com/homu-sys/oxide-receipts/main/index"
            ),
            "raw"
        );
        assert_eq!(
            repo_name_from_url("https://example.org/index.json"),
            "example"
        );
    }
}
