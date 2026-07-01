use axum::body::Body;
use axum::response::Response;
use flate2::write::GzEncoder;
use flate2::Compression;
use http_body_util::BodyExt;
use std::io::Write;

pub const LANG_SWITCHER_CSS: &str = r#"
#lang-switcher{position:fixed;bottom:20px;right:20px;z-index:9999;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif}
#lang-switcher .lang-btn{display:flex;align-items:center;gap:6px;padding:8px 14px;background:#fff;border:1px solid #dcdcde;border-radius:8px;cursor:pointer;font-size:14px;color:#333;box-shadow:0 2px 8px rgba(0,0,0,.12);transition:all .2s}
#lang-switcher .lang-btn:hover{background:#f0f0f0;border-color:#bbb}
#lang-switcher .lang-btn svg{width:12px;height:12px;transition:transform .2s}
#lang-switcher.open .lang-btn svg{transform:rotate(180deg)}
#lang-switcher .lang-menu{display:none;position:absolute;bottom:100%;right:0;margin-bottom:6px;background:#fff;border:1px solid #dcdcde;border-radius:8px;overflow:hidden;box-shadow:0 4px 16px rgba(0,0,0,.15);min-width:140px}
#lang-switcher.open .lang-menu{display:block}
#lang-switcher .lang-menu a{display:block;padding:10px 16px;color:#333;text-decoration:none;font-size:14px;transition:background .15s}
#lang-switcher .lang-menu a:hover{background:#f0f0f0}
#lang-switcher .lang-menu a.current{color:#6b4fbb;font-weight:600;background:#f5f0ff}
"#;

pub const LANG_SWITCHER_HTML: &str = r#"
<div id="lang-switcher">
  <div class="lang-menu">
    <a href="?locale=en">English</a>
    <a href="?locale=zh_CN">简体中文</a>
    <a href="?locale=zh_TW">繁體中文</a>
    <a href="?locale=ja">日本語</a>
  </div>
  <div class="lang-btn" onclick="document.getElementById('lang-switcher').classList.toggle('open')">
    <svg viewBox="0 0 16 16" fill="currentColor"><path d="M4.427 6.427l3.396 3.396a.25.25 0 00.354 0l3.396-3.396A.25.25 0 0011.396 6H4.604a.25.25 0 00-.177.427z"/></svg>
    Language
  </div>
</div>
<script>
(function(){
  var m=document.cookie.match(/(?:^|;\s*)gitlab_preferred_language=([^;]*)/);
  var cur=m?m[1]:'en';
  var links=document.querySelectorAll('#lang-switcher .lang-menu a');
  for(var i=0;i<links.length;i++){
    var href=links[i].getAttribute('href');
    if(href.indexOf('locale='+cur)!==-1) links[i].classList.add('current');
  }
  document.addEventListener('click',function(e){
    var sw=document.getElementById('lang-switcher');
    if(sw&&!sw.contains(e.target)) sw.classList.remove('open');
  });
})();
</script>
"#;

pub const VIEWPORT_RECHECK_JS: &str = r#"(function(){
var w=window,d=document,c=d.cookie;
var vpw=w.innerWidth||d.documentElement.clientWidth;
var cur='';
var m=c.match(/(?:^|;\s*)gitlab_device=([^;]*)/);
if(m)cur=m[1];
var target=(vpw<768)?'mobile':(vpw<1024)?'tablet':'desktop';
if(target!==cur){
var dt=new Date();
dt.setTime(dt.getTime()+2592000000);
d.cookie='gitlab_device='+target+';path=/;max-age=2592000;samesite=lax';
w.location.reload();
}
})();"#;

pub const PROFILE_AVATAR_NOTE_JS: &str = r#"(function(){
if(location.pathname.indexOf('/-/user_settings/profile')===-1)return;
var els=document.querySelectorAll('.help-block, .form-text, .gl-field-hint');
for(var i=0;i<els.length;i++){
var t=els[i].textContent;
if(t.indexOf('192')!==-1&&t.indexOf('10 MiB')!==-1){
els[i].innerHTML+=' <strong style="color:#d9534f">（头像更新后需刷新页面或重新登录才能看到变化）</strong>';
break;
}
}
})();"#;

pub const ABOUT_GITLAB_FIX_JS: &str = r#"(function(){
function fix(){
var as=document.querySelectorAll('a');
for(var i=0;i<as.length;i++){
var a=as[i];
var h=a.getAttribute('href')||'';
var t=a.textContent||'';
if(h.indexOf('about.gitlab.com')!==-1||t.indexOf('About GitLab')!==-1){
a.setAttribute('href','https://github.com/toarujs/gitlab-rs');
a.setAttribute('target','_blank');
a.setAttribute('rel','noopener noreferrer');
}
}
}
fix();
new MutationObserver(function(){fix()}).observe(document.documentElement,{childList:true,subtree:true});
})();"#;

pub const WEB_VITALS_JS: &str = r#"(function(){
var s=document.createElement('script');
s.src='https://cdn.jsdelivr.net/npm/web-vitals@3/dist/web-vitals.iife.js';
s.onload=function(){
web-vitals.onLCP(function(m){});
web-vitals.onFID(function(m){});
web-vitals.onCLS(function(m){});
};
document.head.appendChild(s);
})();"#;

const NON_DEFER_PATTERNS: &[&str] = &[
    "turbolinks",
    "rails-ujs",
    "application.js",
    "gon.watch",
    "webpack",
    "runtime",
    "main.chunk",
    "vendor",
    "data-confirm-modal",
    "gitlab",
];

fn should_defer_script(tag: &str) -> bool {
    if tag.contains("defer") || tag.contains("async") || tag.contains("type=\"module\"") {
        return false;
    }
    if !tag.contains("src=") {
        return false;
    }
    let lower = tag.to_lowercase();
    for pattern in NON_DEFER_PATTERNS {
        if lower.contains(pattern) {
            return false;
        }
    }
    true
}

fn defer_scripts(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut remaining = html;
    while let Some(pos) = remaining.find("<script") {
        let tag_end = remaining[pos..].find('>').unwrap_or(0);
        let tag = &remaining[pos..pos + tag_end + 1];
        result.push_str(&remaining[..pos]);
        if should_defer_script(tag) {
            if let Some(src_pos) = tag.find("src=") {
                let src_val_start = tag[src_pos..].find('"')
                    .or_else(|| tag[src_pos..].find('\''))
                    .unwrap_or(0);
                let quote_char = tag.as_bytes()[src_pos + src_val_start];
                let abs_start = src_pos + src_val_start + 1;
                if let Some(quote_end) = tag[abs_start..].find(|c| c == quote_char as char) {
                    let src = &tag[abs_start..abs_start + quote_end];
                    result.push_str(&format!("<script src=\"{}\" defer></script>", src));
                    remaining = &remaining[pos + tag_end + 1..];
                    continue;
                }
            }
        }
        result.push_str(tag);
        remaining = &remaining[pos + tag_end + 1..];
    }
    result.push_str(remaining);
    result
}

pub async fn inject_into_response(response: Response) -> Response {
    let (parts, body) = response.into_parts();
    let content_type = parts.headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if !content_type.contains("text/html") {
        return Response::from_parts(parts, body);
    }

    let body_bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return Response::builder()
                .status(502)
                .body(Body::from("Bad Gateway"))
                .unwrap();
        }
    };

    let html = match std::str::from_utf8(&body_bytes) {
        Ok(s) => s.to_string(),
        Err(_) => return Response::from_parts(parts, Body::from(body_bytes)),
    };

    let injected = inject_mobile_html(&html);
    let injected = inject_lang_data(&injected);

    let mut new_parts = parts;
    new_parts.headers.remove("transfer-encoding");

    let mut encoder = GzEncoder::new(Vec::with_capacity(injected.len()), Compression::fast());
    if let Err(_) = encoder.write_all(injected.as_bytes()) {
        return Response::builder()
            .status(502)
            .body(Body::from("Bad Gateway"))
            .unwrap();
    }
    let compressed = match encoder.finish() {
        Ok(v) => v,
        Err(_) => return Response::builder()
            .status(502)
            .body(Body::from("Bad Gateway"))
            .unwrap(),
    };
    new_parts.headers.insert(
        "content-length",
        compressed.len().to_string().parse().unwrap(),
    );
    new_parts.headers.insert(
        "content-encoding",
        "gzip".parse().unwrap(),
    );

    Response::from_parts(new_parts, Body::from(compressed))
}

pub fn inject_lang_data(html: &str) -> String {
    let mut injected = html.to_string();

    // Add Chinese to GitLab's built-in language switcher data-locales
    if let Some(pos) = injected.find("js-language-switcher") {
        if let Some(data_start) = injected[pos..].find("data-locales=\"") {
            let abs_start = pos + data_start + "data-locales=\"".len();
            if let Some(rel_end) = injected[abs_start..].find("}]\"") {
                let array_end = abs_start + rel_end + 1;
                let zh = r#",{&quot;value&quot;:&quot;zh_CN&quot;,&quot;percentage&quot;:95,&quot;text&quot;:&quot;简体中文&quot;},{&quot;value&quot;:&quot;zh_TW&quot;,&quot;percentage&quot;:90,&quot;text&quot;:&quot;繁體中文&quot;}"#;
                injected.insert_str(array_end, zh);
            }
        }
    }

    injected
}

pub fn inject_mobile_html(html: &str) -> String {
    if !html.contains("</head>") {
        return html.to_string();
    }

    let mut injected = String::with_capacity(html.len() + 2048);

    if let Some(head_pos) = html.find("</head>") {
        injected.push_str(&html[..head_pos]);

        if !html.contains("name=\"viewport\"") {
            injected.push_str(
                "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1.0,maximum-scale=5.0\">\n"
            );
        }

        injected.push_str(
            "<link rel=\"stylesheet\" href=\"/-/mobile.css\" media=\"screen and (max-width:767px)\">\n",
        );

        injected.push_str(
            "<link rel=\"preconnect\" href=\"/\" crossorigin>\n",
        );

        injected.push_str(
            "<link rel=\"dns-prefetch\" href=\"//cdn.jsdelivr.net\">\n",
        );

        injected.push_str(
            "<link rel=\"preload\" as=\"style\" href=\"/-/mobile.css\">\n",
        );

        injected.push_str("<script>");
        injected.push_str(VIEWPORT_RECHECK_JS);
        injected.push_str("</script>\n");

        injected.push_str("<script>");
        injected.push_str(PROFILE_AVATAR_NOTE_JS);
        injected.push_str("</script>\n");

        injected.push_str(&html[head_pos..]);
    } else {
        injected.push_str(html);
    }

    if injected.contains("<img ") {
        injected = injected.replace(
            "<img loading=\"lazy\" loading=\"lazy\" ",
            "<img loading=\"lazy\" ",
        );
        injected = injected.replace("<img ", "<img loading=\"lazy\" decoding=\"async\" ");
        injected = injected.replace(
            "<img loading=\"lazy\" decoding=\"async\" loading=\"lazy\" decoding=\"async\" ",
            "<img loading=\"lazy\" decoding=\"async\" ",
        );
    }

    injected = defer_scripts(&injected);

    // Inject language switcher before </body>
    if let Some(body_end) = injected.rfind("</body>") {
        injected.insert_str(body_end, "\n<style>");
        injected.insert_str(body_end + 7, LANG_SWITCHER_CSS);
        injected.insert_str(body_end + 7 + LANG_SWITCHER_CSS.len(), "</style>\n");
        let mut offset = body_end + 7 + LANG_SWITCHER_CSS.len() + 8;
        injected.insert_str(offset, LANG_SWITCHER_HTML);
        offset += LANG_SWITCHER_HTML.len();
        injected.insert_str(offset, "\n<script>");
        offset += 9;
        injected.insert_str(offset, ABOUT_GITLAB_FIX_JS);
        offset += ABOUT_GITLAB_FIX_JS.len();
        injected.insert_str(offset, "</script>\n");
    }

    injected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_viewport_and_css() {
        let html = "<!DOCTYPE html><html><head><title>Test</title></head><body></body></html>";
        let result = inject_mobile_html(html);
        assert!(result.contains("name=\"viewport\""));
        assert!(result.contains("/-/mobile.css"));
        assert!(result.contains("gitlab_device="));
    }

    #[test]
    fn test_inject_preload_and_prefetch() {
        let html = "<!DOCTYPE html><html><head></head><body></body></html>";
        let result = inject_mobile_html(html);
        assert!(result.contains("rel=\"preload\""));
        assert!(result.contains("rel=\"dns-prefetch\""));
    }

    #[test]
    fn test_inject_no_duplicate_viewport() {
        let html = "<!DOCTYPE html><html><head><meta name=\"viewport\" content=\"width=device-width\"><title>Test</title></head><body></body></html>";
        let result = inject_mobile_html(html);
        assert_eq!(result.matches("name=\"viewport\"").count(), 1);
    }

    #[test]
    fn test_inject_lazy_images() {
        let html = "<!DOCTYPE html><html><head></head><body><img src=\"a.png\"><img src=\"b.png\"></body></html>";
        let result = inject_mobile_html(html);
        assert!(result.contains("loading=\"lazy\""));
        assert!(result.contains("decoding=\"async\""));
        assert_eq!(result.matches("loading=\"lazy\"").count(), 2);
    }

    #[test]
    fn test_defer_external_scripts() {
        let html = r#"<!DOCTYPE html><html><head></head><body><script src="/assets/foo.js"></script></body></html>"#;
        let result = inject_mobile_html(html);
        assert!(result.contains("src=\"/assets/foo.js\" defer"));
    }

    #[test]
    fn test_no_defer_rails_ujs() {
        let html = r#"<!DOCTYPE html><html><head></head><body><script src="/assets/rails-ujs.js"></script></body></html>"#;
        let result = inject_mobile_html(html);
        assert!(!result.contains("defer"));
    }

    #[test]
    fn test_no_defer_module_scripts() {
        let html = r#"<!DOCTYPE html><html><head></head><body><script type="module" src="/app.js"></script></body></html>"#;
        let result = inject_mobile_html(html);
        assert!(!result.contains("defer"));
    }

    #[test]
    fn test_no_head_tag() {
        let html = "<html><body>no head</body></html>";
        let result = inject_mobile_html(html);
        assert_eq!(result, html);
    }

    #[test]
    fn test_defer_scripts_util() {
        let html = r#"<script src="/foo.js"></script><script src="/rails-ujs.js"></script>"#;
        let result = defer_scripts(html);
        assert!(result.contains("/foo.js\" defer"));
        assert!(!result.contains("/rails-ujs.js\" defer"));
    }
}
