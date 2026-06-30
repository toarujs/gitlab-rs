use axum::{
    extract::Request,
    http::HeaderMap,
    middleware::Next,
    response::Response,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceClass {
    Mobile,
    Tablet,
    Desktop,
}

impl DeviceClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeviceClass::Mobile => "mobile",
            DeviceClass::Tablet => "tablet",
            DeviceClass::Desktop => "desktop",
        }
    }

    pub fn is_mobile_or_tablet(&self) -> bool {
        matches!(self, DeviceClass::Mobile | DeviceClass::Tablet)
    }
}

const MOBILE_UA_PATTERNS: &[&str] = &[
    "Mobi", "Android", "iPhone", "iPad", "iPod", "webOS", "BlackBerry",
    "Opera Mini", "IEMobile", "Mobile Safari",
];

const TABLET_UA_PATTERNS: &[&str] = &[
    "iPad", "Android 3", "Android 4", "Android 5", "Android 6", "Android 7",
    "Tablet", "PlayBook", "Silk",
];

pub fn classify_user_agent(ua: &str) -> DeviceClass {
    if ua.is_empty() {
        return DeviceClass::Desktop;
    }

    for pattern in TABLET_UA_PATTERNS {
        if ua.contains(pattern) {
            return DeviceClass::Tablet;
        }
    }

    for pattern in MOBILE_UA_PATTERNS {
        if ua.contains(pattern) {
            return DeviceClass::Mobile;
        }
    }

    DeviceClass::Desktop
}

fn classify_request(headers: &HeaderMap) -> DeviceClass {
    let cookie_device = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|c| {
                let c = c.trim();
                if c.starts_with("gitlab_device=") {
                    let val = c.strip_prefix("gitlab_device=").unwrap_or("");
                    match val {
                        "mobile" => Some(DeviceClass::Mobile),
                        "tablet" => Some(DeviceClass::Tablet),
                        "desktop" => Some(DeviceClass::Desktop),
                        _ => None,
                    }
                } else {
                    None
                }
            })
        });

    if let Some(device) = cookie_device {
        return device;
    }

    let ua = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    classify_user_agent(ua)
}

pub async fn device_detection_middleware(mut req: Request, next: Next) -> Response {
    let device = classify_request(req.headers());
    req.extensions_mut().insert(device);

    let x_device = if device.is_mobile_or_tablet() {
        "mobile"
    } else {
        "desktop"
    };
    req.headers_mut()
        .insert("X-Gitlab-Device", x_device.parse().unwrap());

    let mut response = next.run(req).await;

    if device.is_mobile_or_tablet() {
        response.headers_mut().append(
            "Set-Cookie",
            format!(
                "gitlab_device={}; Path=/; Max-Age=2592000; SameSite=Lax",
                device.as_str()
            )
            .parse()
            .unwrap(),
        );
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iphone_ua() {
        assert_eq!(
            classify_user_agent("Mozilla/5.0 (iPhone; CPU iPhone OS 16_0 like Mac OS X)"),
            DeviceClass::Mobile
        );
    }

    #[test]
    fn test_android_mobile_ua() {
        assert_eq!(
            classify_user_agent("Mozilla/5.0 (Linux; Android 13; Pixel 7)"),
            DeviceClass::Mobile
        );
    }

    #[test]
    fn test_ipad_ua() {
        assert_eq!(
            classify_user_agent("Mozilla/5.0 (iPad; CPU OS 16_0 like Mac OS X)"),
            DeviceClass::Tablet
        );
    }

    #[test]
    fn test_desktop_ua() {
        assert_eq!(
            classify_user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/120.0"),
            DeviceClass::Desktop
        );
    }

    #[test]
    fn test_empty_ua() {
        assert_eq!(classify_user_agent(""), DeviceClass::Desktop);
    }

    #[test]
    fn test_mobile_safari() {
        assert_eq!(
            classify_user_agent(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"
            ),
            DeviceClass::Mobile
        );
    }
}
