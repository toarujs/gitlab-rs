use url::Url;

/// Build a clone URL that hits this cluster's GitLab entry (`clone_url`)
/// while keeping path and job credentials from `repo_url`.
pub fn rewrite_clone_url(clone_base: &str, repo_url: &str) -> String {
    let Ok(repo) = Url::parse(repo_url) else {
        return repo_url.to_string();
    };
    let Ok(mut base) = Url::parse(clone_base) else {
        return repo_url.to_string();
    };
    base.set_path(repo.path());
    if let Some(q) = repo.query() {
        base.set_query(Some(q));
    }
    if !repo.username().is_empty() {
        let _ = base.set_username(repo.username());
    }
    if let Some(pass) = repo.password() {
        let _ = base.set_password(Some(pass));
    }
    base.to_string()
}

pub fn inject_job_token(repo_url: &str, job_token: &str) -> String {
    let Ok(mut u) = Url::parse(repo_url) else {
        return repo_url.to_string();
    };
    if u.password().is_some() && !u.username().is_empty() {
        return u.to_string();
    }
    let _ = u.set_username("gitlab-ci-token");
    let _ = u.set_password(Some(job_token));
    u.to_string()
}

pub fn redact_secrets(s: &str) -> String {
    let mut out = s.to_string();
    if let Ok(u) = Url::parse(s) {
        if u.password().is_some() {
            let mut redacted = u.clone();
            let _ = redacted.set_password(Some("***"));
            out = redacted.to_string();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_host_keeps_path() {
        let got = rewrite_clone_url(
            "http://web",
            "http://gitlab-ci-token:jobtok@localhost/root/ci.git",
        );
        assert_eq!(got, "http://gitlab-ci-token:jobtok@web/root/ci.git");
    }

    #[test]
    fn injects_token_when_missing() {
        let got = inject_job_token("http://web/root/ci.git", "abc");
        assert_eq!(got, "http://gitlab-ci-token:abc@web/root/ci.git");
    }

    #[test]
    fn redacts_password() {
        let got = redact_secrets("http://gitlab-ci-token:secret@web/root/ci.git");
        assert!(got.contains("***"));
        assert!(!got.contains("secret"));
    }
}
