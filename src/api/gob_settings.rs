#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GOBSettings {
    #[serde(default)]
    pub backend: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

impl GOBSettings {
    pub fn upstream(&self) -> Result<Url, String> {
        if self.backend.is_empty() {
            return Err("gob backend not specified".to_string());
        }

        let u = Url::parse(&self.backend).map_err(|e| format!("invalid URL: {}", e))?;

        if u.scheme() != "http" && u.scheme() != "https" {
            return Err("gob only supports http/https protocols".to_string());
        }

        Ok(u)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_http() {
        let gob = GOBSettings {
            backend: "http://observe.gitlab.com".to_string(),
            headers: HashMap::new(),
        };
        assert!(gob.upstream().is_ok());
    }

    #[test]
    fn test_valid_https() {
        let gob = GOBSettings {
            backend: "https://observe.gitlab.com".to_string(),
            headers: HashMap::new(),
        };
        assert!(gob.upstream().is_ok());
    }

    #[test]
    fn test_empty_backend() {
        let gob = GOBSettings {
            backend: "".to_string(),
            headers: HashMap::new(),
        };
        assert!(gob.upstream().is_err());
    }

    #[test]
    fn test_invalid_scheme() {
        let gob = GOBSettings {
            backend: "tcp://observe.gitlab.com".to_string(),
            headers: HashMap::new(),
        };
        assert!(gob.upstream().is_err());
    }
}
