use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerConfig {
    #[serde(default = "default_concurrent")]
    pub concurrent: u32,
    #[serde(default)]
    pub check_interval: u64,
    #[serde(default)]
    pub shutdown_timeout: u64,
    #[serde(default)]
    pub runners: Vec<RunnerSection>,
}

fn default_concurrent() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerSection {
    pub name: String,
    pub url: String,
    pub token: String,
    pub executor: String,
    #[serde(default)]
    pub clone_url: Option<String>,
    #[serde(default)]
    pub docker: Option<DockerSection>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DockerSection {
    #[serde(default = "default_image")]
    pub image: String,
    #[serde(default)]
    pub network_mode: Option<String>,
    #[serde(default)]
    pub pull_policy: Vec<String>,
    #[serde(default)]
    pub volumes: Vec<String>,
    #[serde(default)]
    pub privileged: bool,
    #[serde(default)]
    pub tls_verify: bool,
}

fn default_image() -> String {
    "ubuntu:latest".to_string()
}

impl RunnerConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("read config {}", path.display()))?;
        let cfg: RunnerConfig = toml::from_str(&raw).context("parse config.toml")?;
        if cfg.runners.is_empty() {
            bail!("config.toml has no [[runners]]");
        }
        for r in &cfg.runners {
            if r.executor != "docker" {
                bail!("unsupported executor: {}", r.executor);
            }
            if r.token.is_empty() || r.url.is_empty() {
                bail!("runner {} missing url or token", r.name);
            }
            if r.docker.is_none() {
                bail!("runner {} missing [runners.docker]", r.name);
            }
        }
        Ok(cfg)
    }

    pub fn request_interval(&self) -> u64 {
        if self.check_interval == 0 {
            3
        } else {
            self.check_interval
        }
    }
}

pub fn default_config_path() -> PathBuf {
    PathBuf::from("/etc/gitlab-runner/config.toml")
}

pub fn load_or_create_system_id(config_path: &Path) -> String {
    if let Ok(id) = std::env::var("RUNNER_SYSTEM_ID") {
        if !id.is_empty() {
            return id;
        }
    }
    let dir = config_path.parent().unwrap_or(Path::new("/etc/gitlab-runner"));
    let file = dir.join(".runner_system_id");
    if let Ok(existing) = fs::read_to_string(&file) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let id = format!("s_{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
    let _ = fs::create_dir_all(dir);
    let _ = fs::write(&file, &id);
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_production_shaped_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut f = fs::File::create(&path).unwrap();
        write!(
            f,
            r#"
concurrent = 1
check_interval = 0
shutdown_timeout = 0

[[runners]]
  name = "gitlab-rs-cs-runner"
  url = "http://web"
  clone_url = "http://web"
  token = "glrt-placeholder"
  executor = "docker"
  [runners.docker]
    image = "ubuntu:latest"
    privileged = false
    volumes = ["/cache"]
    network_mode = "gitlab-rs-net"
    pull_policy = ["if-not-present"]
"#
        )
        .unwrap();
        let cfg = RunnerConfig::load(&path).unwrap();
        assert_eq!(cfg.concurrent, 1);
        assert_eq!(cfg.request_interval(), 3);
        assert_eq!(cfg.runners[0].executor, "docker");
        assert_eq!(cfg.runners[0].docker.as_ref().unwrap().image, "ubuntu:latest");
        assert_eq!(
            cfg.runners[0].docker.as_ref().unwrap().network_mode.as_deref(),
            Some("gitlab-rs-net")
        );
    }

    #[test]
    fn rejects_unknown_executor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[[runners]]
  name = "x"
  url = "http://web"
  token = "t"
  executor = "shell"
"#,
        )
        .unwrap();
        let err = RunnerConfig::load(&path).unwrap_err().to_string();
        assert!(err.contains("shell"), "{err}");
    }
}
