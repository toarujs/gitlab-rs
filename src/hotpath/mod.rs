//! Path templates and 5-minute hot-window stats for Puma proxy timing.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

pub const HOT_WINDOW: Duration = Duration::from_secs(300);
pub const HOT_THRESHOLD: usize = 50;

/// Collapse a request path into a low-cardinality template.
pub fn path_template(path: &str) -> String {
    let path = path.split('?').next().unwrap_or(path);
    if path.is_empty() {
        return "/".to_string();
    }

    let mut parts: Vec<String> = path.split('/').map(|s| s.to_string()).collect();
    let mut i = 0;
    while i < parts.len() {
        if parts[i].is_empty() {
            i += 1;
            continue;
        }

        if parts[i] == "files" && i + 1 < parts.len() {
            parts.truncate(i + 1);
            parts.push("*".to_string());
            break;
        }

        if matches!(parts[i].as_str(), "blob" | "raw" | "tree" | "commits") && i + 1 < parts.len()
        {
            parts[i + 1] = ":ref".to_string();
            if i + 2 < parts.len() {
                parts.truncate(i + 2);
                parts.push("*".to_string());
            }
            break;
        }

        if is_numeric(&parts[i]) {
            parts[i] = ":id".to_string();
        } else if is_git_sha(&parts[i]) {
            parts[i] = ":sha".to_string();
        } else if is_uuid(&parts[i]) {
            parts[i] = ":uuid".to_string();
        }
        i += 1;
    }

    let out = parts.join("/");
    if out.is_empty() {
        "/".to_string()
    } else {
        out
    }
}

fn is_numeric(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_git_sha(s: &str) -> bool {
    let len = s.len();
    (len == 40 || len == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b[8] == b'-'
        && b[13] == b'-'
        && b[18] == b'-'
        && b[23] == b'-'
        && s.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
}

#[derive(Debug, Default)]
pub struct SampleWindow {
    entries: VecDeque<(Instant, u64)>,
    last_aggregate: Option<Instant>,
}

impl SampleWindow {
    pub fn push(&mut self, now: Instant, duration_ms: u64) {
        self.entries.push_back((now, duration_ms));
        let cutoff = now.checked_sub(HOT_WINDOW).unwrap_or(now);
        while self.entries.front().map(|(t, _)| *t < cutoff).unwrap_or(false) {
            self.entries.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_hot(&self) -> bool {
        self.len() >= HOT_THRESHOLD
    }

    pub fn percentile(&self, p: f64) -> Option<u64> {
        if self.entries.is_empty() {
            return None;
        }
        let mut values: Vec<u64> = self.entries.iter().map(|(_, d)| *d).collect();
        values.sort_unstable();
        let max_idx = values.len() - 1;
        let idx = ((p / 100.0) * max_idx as f64).round() as usize;
        values.get(idx.min(max_idx)).copied()
    }

    /// True when the caller should emit an aggregate log (at most every 30s).
    pub fn should_log_aggregate(&mut self, now: Instant) -> bool {
        if !self.is_hot() {
            return false;
        }
        match self.last_aggregate {
            Some(prev) if now.duration_since(prev) < Duration::from_secs(30) => false,
            _ => {
                self.last_aggregate = Some(now);
                true
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct HotPathWindows {
    by_template: HashMap<String, SampleWindow>,
}

impl HotPathWindows {
    pub fn record(&mut self, template: &str, duration_ms: u64) -> HotWindowSnapshot {
        if self.by_template.len() >= 512 && !self.by_template.contains_key(template) {
            return HotWindowSnapshot {
                count: 0,
                is_hot: false,
                p50_ms: None,
                p95_ms: None,
                log_aggregate: false,
            };
        }
        let now = Instant::now();
        let window = self.by_template.entry(template.to_string()).or_default();
        window.push(now, duration_ms);
        let log_aggregate = window.should_log_aggregate(now);
        HotWindowSnapshot {
            count: window.len(),
            is_hot: window.is_hot(),
            p50_ms: window.percentile(50.0),
            p95_ms: window.percentile(95.0),
            log_aggregate,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HotWindowSnapshot {
    pub count: usize,
    pub is_hot: bool,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub log_aggregate: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_template_projects_files() {
        assert_eq!(
            path_template("/api/v4/projects/14/repository/files/.gitlab-ci.yml"),
            "/api/v4/projects/:id/repository/files/*"
        );
    }

    #[test]
    fn test_path_template_job_trace() {
        assert_eq!(
            path_template("/api/v4/projects/14/jobs/19/trace"),
            "/api/v4/projects/:id/jobs/:id/trace"
        );
    }

    #[test]
    fn test_path_template_blob_and_sha() {
        let sha = "baa20295c0d38c26ae5ae43331719da212ac9dc0";
        assert_eq!(
            path_template(&format!("/root/hello-cicd/-/blob/{}/README.md", sha)),
            "/root/hello-cicd/-/blob/:ref/*"
        );
        assert_eq!(
            path_template(&format!("/api/v4/projects/14/repository/commits/{}", sha)),
            "/api/v4/projects/:id/repository/commits/:ref"
        );
    }

    #[test]
    fn test_path_template_strips_query() {
        assert_eq!(
            path_template("/api/v4/projects?membership=true"),
            "/api/v4/projects"
        );
    }

    #[test]
    fn test_window_percentiles() {
        let mut w = SampleWindow::default();
        let start = Instant::now();
        for i in 1..=100 {
            w.push(start, i);
        }
        assert!(w.is_hot());
        let p50 = w.percentile(50.0).unwrap();
        let p95 = w.percentile(95.0).unwrap();
        assert!(p50 >= 49 && p50 <= 51, "p50={p50}");
        assert!(p95 >= 94 && p95 <= 96, "p95={p95}");
    }

    #[test]
    fn test_window_prunes_old_samples() {
        let mut w = SampleWindow::default();
        let old = Instant::now()
            .checked_sub(Duration::from_secs(400))
            .unwrap();
        w.push(old, 10);
        w.push(Instant::now(), 20);
        assert_eq!(w.len(), 1);
        assert_eq!(w.percentile(50.0), Some(20));
    }
}
