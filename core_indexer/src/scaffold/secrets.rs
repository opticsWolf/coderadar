// CodeRadar Stage 3.2 — hardcoded-secret detection with mandatory redaction.
//
// Port the IDEA from fossil's scaffolding tool, rewrite the patterns. The
// redaction rule is absolute: findings travel over MCP to agents, and agents
// are a hostile output channel — a finding must never carry a usable secret.

/// A secret shape worth flagging.
pub struct SecretPattern {
    pub name: &'static str,
    pub regex: regex::Regex,
}

/// Compiled pattern table. Order matters only for display; every hit is
/// reported per line.
pub fn patterns() -> &'static Vec<SecretPattern> {
    static TABLE: std::sync::LazyLock<Vec<SecretPattern>> = std::sync::LazyLock::new(|| {
        let raw: &[(&str, &str)] = &[
            ("aws_access_key", r"\bAKIA[0-9A-Z]{16}\b"),
            ("github_token", r"\bgh[pousr]_[A-Za-z0-9]{30,}\b"),
            ("github_fine_grained", r"\bgithub_pat_[A-Za-z0-9_]{20,}\b"),
            ("stripe_live_key", r"\b[sp]k_live_[A-Za-z0-9]{16,}\b"),
            ("slack_token", r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b"),
            ("openai_key", r"\bsk-[A-Za-z0-9_-]{32,}\b"),
            ("private_key_header", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
            // Generic credential assignment: keyword + quoted value long
            // enough to be a real secret, not `password: ""`.
            (
                "hardcoded_credential",
                r#"(?i)\b(api[_-]?key|secret|passwd|password|token|bearer)\b\s*[:=]\s*["'][^"']{12,}["']"#,
            ),
        ];
        raw.iter()
            .map(|(name, re)| SecretPattern {
                name,
                regex: regex::Regex::new(re).expect("static regex must compile"),
            })
            .collect()
    });
    &TABLE
}

/// Redact a matched secret: keep the first 8 characters plus "***" — enough
/// for a human to recognize which occurrence it is, useless as a credential.
pub fn redact(matched: &str) -> String {
    let mut out: String = matched.chars().take(8).collect();
    out.push_str("***");
    out
}
