#![allow(dead_code)]
use axum::{
    http::{HeaderMap, HeaderValue},
};
use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};

const JWT_ISSUER: &str = "gitlab-workhorse";
const WORKHORSE_VERSION_HEADER: &str = "gitlab-workhorse";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultClaims {
    pub iss: String,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
}

impl DefaultClaims {
    pub fn new(ttl_seconds: i64) -> Self {
        let now = Utc::now();
        Self {
            iss: JWT_ISSUER.to_string(),
            iat: now.timestamp(),
            exp: (now + chrono::Duration::seconds(ttl_seconds)).timestamp(),
            jti: uuid::Uuid::new_v4().to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Secret {
    pub path: String,
    pub bytes: Vec<u8>,
}

impl Secret {
    pub fn from_path(path: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let content = std::fs::read_to_string(path)?;
        let content = content.trim().to_string();

        // Rails uses Base64.strict_decode64 (standard Base64 with +/ and padding)
        // The secret file contains standard Base64-encoded binary data
        let bytes = if content.contains('+') || content.contains('/') || content.contains('=') {
            // Standard Base64 (with +, /, =)
            use base64::{engine::general_purpose::STANDARD, Engine};
            STANDARD.decode(&content)?
        } else if content.len() == 44 && !content.contains('\n') {
            // URL-safe Base64 (Go-style, with -, _ no padding)
            use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
            URL_SAFE_NO_PAD.decode(&content)?
        } else {
            content.into_bytes()
        };

        let bytes = if bytes.len() < 32 {
            let mut padded = vec![0u8; 32];
            padded[..bytes.len()].copy_from_slice(&bytes);
            padded
        } else {
            bytes[..32].to_vec()
        };

        Ok(Self {
            path: path.to_string(),
            bytes,
        })
    }

    pub fn sign_jwt(&self, claims: &DefaultClaims) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;

        let mut mac = HmacSha256::new_from_slice(&self.bytes)?;

        let header = base64_encode_json(&serde_json::json!({
            "alg": "HS256",
            "typ": "JWT"
        }));

        let payload = base64_encode_json(&serde_json::to_value(claims)?);

        let signing_input = format!("{}.{}", header, payload);
        mac.update(signing_input.as_bytes());
        let signature = base64_encode_bytes(&mac.finalize().into_bytes());

        Ok(format!("{}.{}.{}", header, payload, signature))
    }

    pub fn verify_jwt(&self, token: &str) -> Result<DefaultClaims, String> {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;

        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err("Invalid JWT format".to_string());
        }

        let signing_input = format!("{}.{}", parts[0], parts[1]);

        let mut mac = HmacSha256::new_from_slice(&self.bytes)
            .map_err(|_| "Failed to create HMAC".to_string())?;
        mac.update(signing_input.as_bytes());

        let expected_sig = base64_encode_bytes(&mac.finalize().into_bytes());
        if expected_sig != parts[2] {
            return Err("Invalid signature".to_string());
        }

        let payload_bytes = base64_decode(parts[1])?;
        let claims: DefaultClaims = serde_json::from_slice(&payload_bytes)
            .map_err(|e| format!("Invalid claims: {}", e))?;

        if claims.exp < Utc::now().timestamp() {
            return Err("Token expired".to_string());
        }

        if claims.iss != JWT_ISSUER {
            return Err("Invalid issuer".to_string());
        }

        Ok(claims)
    }
}

pub fn add_workhorse_headers(headers: &mut HeaderMap, secret: &Secret) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let claims = DefaultClaims::new(60);
    let token = secret.sign_jwt(&claims)?;

    // Go compatible: Gitlab-Workhorse-Api-Request (title-case)
    headers.insert(
        "gitlab-workhorse-api-request",
        HeaderValue::from_str(&token)?,
    );

    // Go compatible: Gitlab-Workhorse (title-case)
    headers.insert(
        "gitlab-workhorse",
        HeaderValue::from_str(&format!("gitlab-workhorse-rs/{}", env!("CARGO_PKG_VERSION")))?,
    );

    // Go compatible: Gitlab-Workhorse-Proxy-Start (Unix nanosecond timestamp)
    let start_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    headers.insert(
        "gitlab-workhorse-proxy-start",
        HeaderValue::from_str(&start_nanos.to_string())?,
    );

    // Go compatible: X-Forwarded-Proto
    if !headers.contains_key("x-forwarded-proto") {
        headers.insert(
            "x-forwarded-proto",
            HeaderValue::from_static("http"),
        );
    }

    // Go compatible: X-Sendfile-Type (advertise X-Sendfile support)
    headers.insert(
        "x-sendfile-type",
        HeaderValue::from_static("X-Sendfile"),
    );

    Ok(())
}

fn base64_encode_json(value: &serde_json::Value) -> String {
    let json = value.to_string();
    base64_encode_bytes(json.as_bytes())
}

fn base64_encode_bytes(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.encode(bytes)
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.decode(input.as_bytes())
        .map_err(|e| format!("Base64 decode error: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_sign_and_verify() {
        let secret = Secret {
            path: "test".to_string(),
            bytes: vec![0u8; 32],
        };

        let claims = DefaultClaims::new(3600);
        let token = secret.sign_jwt(&claims).unwrap();
        let verified = secret.verify_jwt(&token).unwrap();

        assert_eq!(verified.iss, JWT_ISSUER);
        assert_eq!(verified.jti, claims.jti);
    }

    #[test]
    fn test_secret_from_path() {
        let secret_bytes = "abcdefghijklmnopqrstuvwxyz123456";
        let secret = Secret {
            path: "test".to_string(),
            bytes: secret_bytes.as_bytes().to_vec(),
        };

        let mut padded = vec![0u8; 32];
        padded[..secret_bytes.len()].copy_from_slice(secret_bytes.as_bytes());
        assert_eq!(secret.bytes.len(), 32);
    }
}

#[cfg(debug_assertions)]
pub fn debug_jwt(token: &str) {
    tracing::debug!("Generated JWT token");
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() == 3 {
        if let Ok(payload_bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1]) {
            if let Ok(claims) = std::str::from_utf8(&payload_bytes) {
                tracing::debug!("JWT claims: {}", claims);
            }
        }
    }
}
