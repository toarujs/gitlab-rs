#![allow(dead_code, unused_imports)]
use std::collections::HashMap;
use url::Url;

pub struct OAuthProxy {
    pub rails_url: Url,
    pub iam_url: Option<Url>,
}

impl OAuthProxy {
    pub fn new(rails_url: Url) -> Self {
        Self {
            rails_url,
            iam_url: None,
        }
    }

    pub fn with_iam_url(mut self, url: Url) -> Self {
        self.iam_url = Some(url);
        self
    }

    pub fn is_iam_auth_enabled(&self) -> bool {
        self.iam_url.is_some()
    }

    pub fn is_token_request(path: &str) -> bool {
        path.starts_with("/oauth/token")
    }

    pub fn is_authorize_request(path: &str) -> bool {
        path.starts_with("/oauth/authorize")
    }

    pub fn is_userinfo_request(path: &str) -> bool {
        path.starts_with("/oauth/userinfo")
    }

    pub fn proxy_target(&self, path: &str) -> &Url {
        if self.iam_url.is_some() && Self::is_token_request(path) {
            self.iam_url.as_ref().unwrap()
        } else {
            &self.rails_url
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_token_request() {
        assert!(OAuthProxy::is_token_request("/oauth/token"));
        assert!(OAuthProxy::is_token_request("/oauth/token/info"));
        assert!(!OAuthProxy::is_token_request("/api/v4/users"));
    }

    #[test]
    fn test_is_authorize_request() {
        assert!(OAuthProxy::is_authorize_request("/oauth/authorize"));
        assert!(!OAuthProxy::is_authorize_request("/oauth/token"));
    }

    #[test]
    fn test_proxy_target_rails() {
        let rails = Url::parse("http://localhost:8080").unwrap();
        let proxy = OAuthProxy::new(rails.clone());
        assert_eq!(proxy.proxy_target("/api/v4/users"), &rails);
    }

    #[test]
    fn test_proxy_target_iam() {
        let rails = Url::parse("http://localhost:8080").unwrap();
        let iam = Url::parse("http://localhost:9090").unwrap();
        let proxy = OAuthProxy::new(rails).with_iam_url(iam.clone());
        assert_eq!(proxy.proxy_target("/oauth/token"), &iam);
    }
}
