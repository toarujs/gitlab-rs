use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::Response;
use http_body_util::BodyExt;
use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

static ZH_CN: OnceLock<HashMap<String, String>> = OnceLock::new();
static ZH_TW: OnceLock<HashMap<String, String>> = OnceLock::new();
static JA: OnceLock<HashMap<String, String>> = OnceLock::new();

fn load_dict(raw: &'static str) -> HashMap<String, String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn dict_for(lang: &str) -> Option<&'static HashMap<String, String>> {
    match lang {
        "zh-cn" => Some(ZH_CN.get_or_init(|| {
            load_dict(include_str!("../assets/webide-nls/zh-cn.json"))
        })),
        "zh-tw" => Some(ZH_TW.get_or_init(|| {
            load_dict(include_str!("../assets/webide-nls/zh-tw.json"))
        })),
        "ja" => Some(JA.get_or_init(|| load_dict(include_str!("../assets/webide-nls/ja.json")))),
        _ => None,
    }
}

pub fn nls_lang_from_headers(headers: &HeaderMap) -> Option<&'static str> {
    let cookie = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    locale_from_cookie(cookie)
}

pub fn locale_from_cookie(cookie: &str) -> Option<&'static str> {
    let mut preferred: Option<&str> = None;
    for part in cookie.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("preferred_language=") {
            preferred = Some(v);
            break;
        }
        if preferred.is_none() {
            if let Some(v) = part.strip_prefix("gitlab_preferred_language=") {
                preferred = Some(v);
            }
        }
    }
    map_locale(preferred.unwrap_or(""))
}

fn map_locale(raw: &str) -> Option<&'static str> {
    let v = raw
        .trim()
        .trim_matches('"')
        .replace('_', "-")
        .to_ascii_lowercase();
    match v.as_str() {
        "zh-cn" | "zh-hans" | "zh" => Some("zh-cn"),
        "zh-tw" | "zh-hant" => Some("zh-tw"),
        "ja" | "ja-jp" => Some("ja"),
        _ => None,
    }
}

pub fn is_nls_messages_path(path: &str) -> bool {
    path.ends_with("/vscode/out/nls.messages.js") || path.ends_with("/nls.messages.js")
}

pub async fn maybe_translate_nls_response(
    path: &str,
    headers: &HeaderMap,
    response: Response,
) -> Response {
    if is_nls_messages_path(path) {
        translate_nls_response(response, headers).await
    } else {
        response
    }
}

fn maybe_decompress(bytes: &[u8], encoding: &str) -> Option<Vec<u8>> {
    let enc = encoding.to_ascii_lowercase();
    if enc.contains("gzip") {
        let mut decoder = flate2::read::GzDecoder::new(bytes);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).ok()?;
        return Some(out);
    }
    if enc.contains("br") || enc.contains("brotli") {
        let mut out = Vec::new();
        let mut reader = brotli::Decompressor::new(bytes, 4096);
        reader.read_to_end(&mut out).ok()?;
        return Some(out);
    }
    None
}

pub fn translate_nls_js(source: &str, lang: &str) -> Option<String> {
    let dict = dict_for(lang)?;
    if !source.contains("_VSCODE_NLS_MESSAGES") {
        return None;
    }
    let mut entries: Vec<(&String, &String)> = dict.iter().collect();
    entries.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    let mut out = source.to_string();
    for (en, tr) in entries {
        let Ok(from) = serde_json::to_string(en) else {
            continue;
        };
        let Ok(to) = serde_json::to_string(tr) else {
            continue;
        };
        if from != to {
            out = out.replace(&from, &to);
        }
    }
    let Ok(lang_js) = serde_json::to_string(lang) else {
        return Some(out);
    };
    let lang_stmt = format!("\nglobalThis._VSCODE_NLS_LANGUAGE={};", lang_js);
    if out.contains("_VSCODE_NLS_LANGUAGE=") {
        return Some(out);
    }
    if let Some(pos) = out.find("];") {
        out.insert_str(pos + 2, &lang_stmt);
    } else {
        out.push_str(&lang_stmt);
        out.push('\n');
    }
    Some(out)
}

pub async fn translate_nls_response(response: Response, headers: &HeaderMap) -> Response {
    let Some(lang) = nls_lang_from_headers(headers) else {
        return response;
    };
    let (parts, body) = response.into_parts();
    if !parts.status.is_success() {
        return Response::from_parts(parts, body);
    }
    let encoding = parts
        .headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body_bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return Response::builder()
                .status(502)
                .body(Body::from("Bad Gateway"))
                .unwrap();
        }
    };
    let decoded = if encoding.is_empty() || encoding.eq_ignore_ascii_case("identity") {
        body_bytes.to_vec()
    } else {
        match maybe_decompress(&body_bytes, &encoding) {
            Some(v) => v,
            None => return Response::from_parts(parts, Body::from(body_bytes)),
        }
    };
    let Ok(source) = std::str::from_utf8(&decoded) else {
        return Response::from_parts(parts, Body::from(body_bytes));
    };
    let Some(translated) = translate_nls_js(source, lang) else {
        return Response::from_parts(parts, Body::from(body_bytes));
    };
    let mut new_parts = parts;
    new_parts.headers.remove("content-encoding");
    new_parts.headers.remove("transfer-encoding");
    new_parts.headers.remove("etag");
    new_parts.headers.insert(
        "content-type",
        HeaderValue::from_static("application/javascript; charset=utf-8"),
    );
    new_parts.headers.insert(
        "cache-control",
        HeaderValue::from_static("private, no-store, must-revalidate"),
    );
    new_parts.headers.insert(
        "content-length",
        HeaderValue::from_str(&translated.len().to_string()).unwrap(),
    );
    Response::from_parts(new_parts, Body::from(translated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_gitlab_cookie_locales() {
        assert_eq!(locale_from_cookie("preferred_language=zh_CN"), Some("zh-cn"));
        assert_eq!(locale_from_cookie("preferred_language=zh_TW"), Some("zh-tw"));
        assert_eq!(
            locale_from_cookie("gitlab_preferred_language=ja"),
            Some("ja")
        );
        assert_eq!(locale_from_cookie("preferred_language=en"), None);
        assert_eq!(
            locale_from_cookie("_gitlab_session=abc; preferred_language=zh_CN; other=1"),
            Some("zh-cn")
        );
    }

    #[test]
    fn translates_file_menu_and_sets_language() {
        let src = concat!(
            r#"globalThis._VSCODE_NLS_MESSAGES=["&&File","Save","Explorer","not-in-dict"];"#,
            "\n//# sourceMappingURL=nls.messages.js.map\n"
        );
        let out = translate_nls_js(src, "zh-cn").expect("translated");
        assert!(out.contains("文件(&&F)"));
        assert!(out.contains("保存"));
        assert!(out.contains("资源管理器"));
        assert!(out.contains("not-in-dict"));
        assert!(out.contains(r#"globalThis._VSCODE_NLS_LANGUAGE="zh-cn""#));
    }

    #[test]
    fn translates_even_with_js_template_literals() {
        let src = "globalThis._VSCODE_NLS_MESSAGES=[\"Save\",`{0}\n\nPlease`,\"Explorer\"];\n";
        let out = translate_nls_js(src, "zh-cn").expect("translated");
        assert!(out.contains("保存"));
        assert!(out.contains("资源管理器"));
        assert!(out.contains("`{0}"));
    }

    #[test]
    fn leaves_unknown_locale_untouched() {
        assert!(dict_for("en").is_none());
        assert!(translate_nls_js("[\"File\"]", "en").is_none());
    }

    #[test]
    fn detects_workbench_nls_path() {
        assert!(is_nls_messages_path(
            "/assets/webpack/gitlab-web-ide-vscode-workbench-0.0.1-dev-20260106142046/vscode/out/nls.messages.js"
        ));
        assert!(!is_nls_messages_path("/assets/application.js"));
    }
}
