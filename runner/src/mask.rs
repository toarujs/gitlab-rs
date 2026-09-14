pub fn mask_text(text: &str, secrets: &[String]) -> String {
    if secrets.is_empty() {
        return text.to_string();
    }
    let mut out = text.to_string();
    for secret in secrets {
        if secret.is_empty() {
            continue;
        }
        if out.contains(secret) {
            out = out.replace(secret, "[MASKED]");
        }
    }
    out
}

pub fn collect_secrets<I, S>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut secrets: Vec<String> = values
        .into_iter()
        .map(|s| s.as_ref().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    secrets.dedup();
    secrets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_longest_secret_first() {
        let secrets = collect_secrets(["ab", "abcd"]);
        let out = mask_text("token=abcd leftover", &secrets);
        assert_eq!(out, "token=[MASKED] leftover");
        assert!(!out.contains("abcd"));
    }

    #[test]
    fn skips_empty_secret() {
        let secrets = collect_secrets(["", "secret-value"]);
        let out = mask_text("secret-value in log", &secrets);
        assert_eq!(out, "[MASKED] in log");
    }
}
