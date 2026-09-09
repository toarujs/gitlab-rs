//! Decode `_gitlab_session` into a Rails user id.

use serde_json::Value;

/// Extract the GitLab session id from a Cookie header.
pub fn session_id_from_cookie_header(cookie: &str) -> Option<String> {
    for part in cookie.split(';') {
        let part = part.trim();
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        if name.starts_with("_gitlab_session") {
            let decoded = percent_decode(value.trim());
            if !decoded.is_empty() {
                return Some(decoded);
            }
        }
    }
    None
}

pub fn redis_session_keys(session_id: &str) -> Vec<String> {
    vec![
        format!("session:gitlab:{}", session_id),
        format!("session:gitlab:2::{}", session_id),
        session_id.to_string(),
    ]
}

/// Pull `user_id` out of a Redis session payload (JSON or Ruby Marshal).
pub fn user_id_from_session_bytes(data: &[u8]) -> Option<i64> {
    if let Ok(text) = std::str::from_utf8(data) {
        let trimmed = text.trim();
        if trimmed.starts_with('{') {
            if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
                if let Some(id) = user_id_from_json(&value) {
                    return Some(id);
                }
            }
        }
    }
    user_id_from_marshal(data)
}

fn user_id_from_json(value: &Value) -> Option<i64> {
    if let Some(key) = value.get("warden.user.user.key") {
        if let Some(id) = nested_user_id(key) {
            return Some(id);
        }
    }
    value.get("user_id").and_then(|v| v.as_i64())
}

fn nested_user_id(value: &Value) -> Option<i64> {
    let arr = value.as_array()?;
    let first = arr.first()?;
    if let Some(id) = first.as_i64() {
        return (id > 0).then_some(id);
    }
    let inner = first.as_array()?.first()?;
    let id = inner.as_i64()?;
    (id > 0).then_some(id)
}

fn user_id_from_marshal(data: &[u8]) -> Option<i64> {
    let needle = b"warden.user.user.key";
    let pos = data.windows(needle.len()).position(|w| w == needle)?;
    let rest = &data[pos + needle.len()..];
    let limit = rest.len().min(80);
    for i in 0..limit {
        if rest[i] == b'i' {
            if let Some(id) = read_marshal_fixnum(&rest[i + 1..]) {
                if id > 0 {
                    return Some(id);
                }
            }
        }
    }
    None
}

fn read_marshal_fixnum(data: &[u8]) -> Option<i64> {
    let n = *data.first()? as i8;
    match n {
        0 => Some(0),
        1 => data.get(1).map(|b| i64::from(*b)),
        2 => {
            let b = data.get(1..3)?;
            Some(i64::from(i16::from_le_bytes([b[0], b[1]])))
        }
        3 => {
            let b = data.get(1..4)?;
            Some(i64::from(i32::from_le_bytes([b[0], b[1], b[2], 0])))
        }
        4 => {
            let b = data.get(1..5)?;
            Some(i64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])))
        }
        -1 => data.get(1).map(|b| i64::from(*b as i8)),
        -2 => {
            let b = data.get(1..3)?;
            Some(i64::from(i16::from_le_bytes([b[0], b[1]])))
        }
        _ => Some(i64::from(n) - 5),
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_gitlab_session_cookie() {
        let header = "preferred_language=en; _gitlab_session=abc123; other=1";
        assert_eq!(
            session_id_from_cookie_header(header).as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn extracts_user_id_from_json_session() {
        let json = r#"{"warden.user.user.key":[[42],"$2a$10$abc"]}"#;
        assert_eq!(user_id_from_session_bytes(json.as_bytes()), Some(42));
    }

    #[test]
    fn extracts_user_id_from_marshal_session() {
        let mut data = b"\x04\x08I\"\x19warden.user.user.key\x06:\x06ET[\x07[\x06i".to_vec();
        data.push(7); // marshal fixnum 2 (encoded as 2+5)
        data.extend_from_slice(b"I\"\tsalt");
        assert_eq!(user_id_from_session_bytes(&data), Some(2));
    }

    #[test]
    fn missing_session_is_none() {
        assert_eq!(session_id_from_cookie_header("foo=bar"), None);
        assert_eq!(user_id_from_session_bytes(b"not a session"), None);
    }
}
