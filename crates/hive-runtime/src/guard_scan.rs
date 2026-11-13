//! Content scanners for the workspace envelope: secret redaction (and, for
//! self-medication, the infection detectors). Pure, regex-based, and
//! deliberately conservative: a pattern is here only when a match is almost
//! certainly a credential, because a false redaction mangles a legitimate
//! reply.

use std::sync::OnceLock;

use regex::Regex;

/// One credential pattern.
struct SecretPattern {
    kind: &'static str,
    re: Regex,
}

fn secret_patterns() -> &'static [SecretPattern] {
    static P: OnceLock<Vec<SecretPattern>> = OnceLock::new();
    P.get_or_init(|| {
        let p = |kind, re: &str| SecretPattern { kind, re: Regex::new(re).expect("secret pattern") };
        vec![
            p("private_key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----"),
            p("aws_access_key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
            p("github_token", r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{60,})\b"),
            p("anthropic_key", r"\bsk-ant-[A-Za-z0-9_\-]{20,}"),
            p("openai_key", r"\bsk-(?:proj-|svcacct-)?[A-Za-z0-9_\-]{32,}"),
            p("slack_token", r"\bxox[abprs]-[A-Za-z0-9\-]{10,}"),
            p("google_api_key", r"\bAIza[0-9A-Za-z_\-]{35}\b"),
            p("stripe_key", r"\b[rs]k_live_[0-9A-Za-z]{24,}\b"),
            // A Ligus workspace invite carries the workspace key itself.
            p("workspace_invite", r"\b(?:hivews1|ligusws1):[A-Za-z0-9_\-]{16,}={0,2}"),
        ]
    })
}

/// Mask every credential in `text` as `[redacted <kind>]`. Returns the masked
/// text and how many credentials were masked.
pub fn redact_secrets(text: &str) -> (String, u32) {
    let mut out = text.to_string();
    let mut count = 0u32;
    for p in secret_patterns() {
        let n = p.re.find_iter(&out).count() as u32;
        if n > 0 {
            count += n;
            out = p.re.replace_all(&out, format!("[redacted {}]", p.kind).as_str()).into_owned();
        }
    }
    (out, count)
}

/// Credential kinds present in `text` (no values), for detectors.
pub fn secret_kinds(text: &str) -> Vec<&'static str> {
    secret_patterns().iter().filter(|p| p.re.is_match(text)).map(|p| p.kind).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_common_credentials() {
        let aws = "AKIAIOSFODNN7EXAMPLE";
        let gh = format!("ghp_{}", "a".repeat(36));
        let ant = format!("sk-ant-api03-{}", "x".repeat(40));
        let text = format!("use {aws} and {gh}\nkey: {ant}\n-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----");
        let (masked, n) = redact_secrets(&text);
        assert_eq!(n, 4);
        assert!(!masked.contains(aws) && !masked.contains(&gh) && !masked.contains(&ant) && !masked.contains("abc"));
        assert!(masked.contains("[redacted aws_access_key]"));
        assert!(masked.contains("[redacted private_key]"));
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        for t in [
            "the sk-learn library and task-1234",
            "AKIA is a prefix",
            "commit ghp_short",
            "see https://example.com/sk-ant",
            "fn main() { let skip = 1; }",
        ] {
            assert_eq!(redact_secrets(t), (t.to_string(), 0), "{t}");
        }
    }

    #[test]
    fn masks_workspace_invites() {
        let (m, n) = redact_secrets("join with hivews1:eyJyZWxheSI6Imh0dHBzOi8vcmVsYXkifQ== please");
        assert_eq!(n, 1);
        assert!(m.contains("[redacted workspace_invite]"));
        assert_eq!(secret_kinds("AKIAIOSFODNN7EXAMPLE"), vec!["aws_access_key"]);
    }
}
