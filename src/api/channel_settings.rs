#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSettings {
    #[serde(default)]
    pub subprotocols: Vec<String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub header: HashMap<String, Vec<String>>,
    #[serde(rename = "CAPem", default)]
    pub ca_pem: String,
    #[serde(rename = "MaxSessionTime", default)]
    pub max_session_time: i32,
}

impl ChannelSettings {
    pub fn validate(&self) -> Result<(), String> {
        if self.subprotocols.is_empty() {
            return Err("no subprotocol specified".to_string());
        }

        let parsed = url::Url::parse(&self.url).map_err(|_| "invalid URL".to_string())?;

        if parsed.scheme() != "ws" && parsed.scheme() != "wss" {
            return Err(format!(
                "invalid websocket scheme: {:?}",
                parsed.scheme()
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_valid() {
        let cs = ChannelSettings {
            subprotocols: vec!["terminal.gitlab.com".to_string()],
            url: "wss://terminal.gitlab.com".to_string(),
            header: HashMap::new(),
            ca_pem: String::new(),
            max_session_time: 0,
        };
        assert!(cs.validate().is_ok());
    }

    #[test]
    fn test_validate_no_subprotocol() {
        let cs = ChannelSettings {
            subprotocols: vec![],
            url: "wss://terminal.gitlab.com".to_string(),
            header: HashMap::new(),
            ca_pem: String::new(),
            max_session_time: 0,
        };
        assert!(cs.validate().is_err());
    }

    #[test]
    fn test_validate_invalid_scheme() {
        let cs = ChannelSettings {
            subprotocols: vec!["test".to_string()],
            url: "http://terminal.gitlab.com".to_string(),
            header: HashMap::new(),
            ca_pem: String::new(),
            max_session_time: 0,
        };
        assert!(cs.validate().is_err());
    }
}
