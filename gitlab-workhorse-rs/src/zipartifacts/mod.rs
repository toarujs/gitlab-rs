#![allow(dead_code, unused_imports)]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

pub fn decode_file_entry(entry: &str) -> Result<String, String> {
    let decoded = BASE64.decode(entry).map_err(|e| format!("base64 decode: {}", e))?;
    String::from_utf8(decoded).map_err(|e| format!("utf8 decode: {}", e))
}

pub const CODE_NOT_ZIP: i32 = 10;
pub const CODE_ENTRY_NOT_FOUND: i32 = 11;
pub const CODE_ARCHIVE_NOT_FOUND: i32 = 12;
pub const CODE_LIMITS_REACHED: i32 = 13;
pub const CODE_UNKNOWN_ERROR: i32 = 14;

pub fn error_label_by_code(code: i32) -> &'static str {
    match code {
        CODE_NOT_ZIP => "archive_invalid",
        CODE_ENTRY_NOT_FOUND => "entry_not_found",
        CODE_ARCHIVE_NOT_FOUND => "archive_not_found",
        CODE_LIMITS_REACHED => "limits_reached",
        _ => "unknown_error",
    }
}

pub fn exit_code_for_entry_not_found() -> i32 {
    CODE_ENTRY_NOT_FOUND
}

pub fn exit_code_for_archive_not_found() -> i32 {
    CODE_ARCHIVE_NOT_FOUND
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_file_entry() {
        let encoded = BASE64.encode(b"test.txt");
        let decoded = decode_file_entry(&encoded).unwrap();
        assert_eq!(decoded, "test.txt");
    }

    #[test]
    fn test_decode_file_entry_invalid() {
        assert!(decode_file_entry("not-valid-base64!!!").is_err());
    }

    #[test]
    fn test_error_labels() {
        assert_eq!(error_label_by_code(CODE_NOT_ZIP), "archive_invalid");
        assert_eq!(error_label_by_code(CODE_ENTRY_NOT_FOUND), "entry_not_found");
        assert_eq!(error_label_by_code(999), "unknown_error");
    }
}
