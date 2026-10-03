// CodeRadar Stage 3.2 — hardcoded-secret detection with mandatory redaction.
//
// Port the IDEA from fossil's scaffolding tool, rewrite the patterns. The
// redaction rule is absolute: findings travel over MCP to agents, and agents
// are a hostile output channel — a finding must never carry a usable secret.

/// A secret shape worth flagging.
pub struct SecretPattern {
    pub name: &'static str,
    pub regex: regex::Regex,
    /// `(keyword_group, value_group)` for a pattern whose match is wider than
    /// the secret itself — the generic credential rule matches
    /// `token = "..."`, where only the quoted value is secret and the keyword
    /// is context worth keeping. `None` for vendor patterns: the whole match
    /// *is* the secret.
    pub groups: Option<(usize, usize)>,
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
            // enough to be a real secret, not `password: ""`. The value must
            // additionally look like a secret — see [`looks_like_a_secret`].
            (
                "hardcoded_credential",
                r#"(?i)\b(api[_-]?key|secret|passwd|password|token|bearer)\b\s*[:=]\s*["']([^"']{12,})["']"#,
            ),
        ];
        raw.iter()
            .map(|(name, re)| SecretPattern {
                name,
                regex: regex::Regex::new(re).expect("static regex must compile"),
                groups: (*name == "hardcoded_credential").then_some((1, 2)),
            })
            .collect()
    });
    &TABLE
}

/// One redacted hit: what to call it and what may be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretHit {
    /// Pattern name, plus the keyword for the generic rule
    /// (`hardcoded_credential (token)`) — the key name is not secret and is
    /// the part a human needs to find it.
    pub label: String,
    /// The match with every secret-bearing part redacted.
    pub snippet: String,
}

/// Find the first secret shape in `line`, already redacted.
///
/// The generic rule is deliberately strict about the *value*: keyword +
/// quoted value ≥ 12 chars matched `token="close_btn_color"` (a theme token
/// name) across Lace source, tests and markdown — 6 findings per occurrence,
/// none of them a credential.
pub fn scan_line(line: &str) -> Option<SecretHit> {
    for pat in patterns() {
        let Some(m) = pat.regex.find(line) else {
            continue;
        };
        match pat.groups {
            None => {
                return Some(SecretHit {
                    label: pat.name.to_string(),
                    snippet: redact(m.as_str()),
                })
            }
            Some((key_group, value_group)) => {
                let caps = pat.regex.captures(line)?;
                let value = caps.get(value_group).map(|g| g.as_str()).unwrap_or("");
                if !looks_like_a_secret(value) {
                    continue; // identifier-shaped value: a name, not a credential
                }
                let key = caps.get(key_group).map(|g| g.as_str()).unwrap_or("");
                return Some(SecretHit {
                    label: format!("{} ({key})", pat.name),
                    snippet: format!("{key}=\"{}\"", redact(value)),
                });
            }
        }
    }
    None
}

/// Whether a generic-rule value looks like a credential rather than a name.
///
/// Two ways to qualify: Shannon entropy ≥ 3.5 bits/char, or upper+lower+digit
/// mixed (a shape names and prose rarely take). Identifier spellings —
/// snake_case, kebab-case, dotted — are rejected outright: `close_btn_color`
/// and `my-api-key-name` are *labels*, and a name is exactly what a
/// configuration field is usually assigned.
pub fn looks_like_a_secret(value: &str) -> bool {
    if value.len() < 12 || is_identifier_like(value) {
        return false;
    }
    shannon_entropy(value) >= 3.5 || mixes_character_classes(value)
}

/// `close_btn_color`, `my-api-key`, `app.config.token`, `HTTPS_PROXY` — a
/// name, not a credential. One separator minimum: a single unbroken word
/// (`hunter2hunter2`) is left to the entropy test.
fn is_identifier_like(value: &str) -> bool {
    let snake = value.split('_').filter(|s| !s.is_empty()).count() >= 2
        && value
            .split('_')
            .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()));
    let kebab_or_dotted = (value.contains('-') || value.contains('.'))
        && value
            .split(['-', '.'])
            .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()));
    snake || kebab_or_dotted
}

fn mixes_character_classes(value: &str) -> bool {
    value.chars().any(|c| c.is_ascii_uppercase())
        && value.chars().any(|c| c.is_ascii_lowercase())
        && value.chars().any(|c| c.is_ascii_digit())
}

/// Shannon entropy in bits per character.
fn shannon_entropy(value: &str) -> f64 {
    let chars: Vec<char> = value.chars().collect();
    if chars.is_empty() {
        return 0.0;
    }
    let mut counts: std::collections::HashMap<char, usize> = std::collections::HashMap::new();
    for c in &chars {
        *counts.entry(*c).or_insert(0) += 1;
    }
    let len = chars.len() as f64;
    counts
        .values()
        .map(|&n| {
            let p = n as f64 / len;
            -p * p.log2()
        })
        .sum()
}

/// Redact a matched secret: keep the first 8 characters plus "***" — enough
/// for a human to recognize which occurrence it is, useless as a credential.
pub fn redact(matched: &str) -> String {
    let mut out: String = matched.chars().take(8).collect();
    out.push_str("***");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_rule_needs_a_secret_shaped_value() {
        // A theme token: a snake_case name, not a credential.
        assert!(scan_line(r#"token = "close_btn_color""#).is_none());
        assert!(scan_line(r#"secret="my_secret_key""#).is_none());
        assert!(scan_line(r#"api_key = "app.config.token""#).is_none());
        assert!(scan_line(r#"password = "my-password-name""#).is_none());
        // High-entropy or mixed-class values still hit.
        let hit = scan_line(r#"api_key = "a1b2c3d4e5f6g7h8""#).expect("entropy hit");
        assert_eq!(hit.label, "hardcoded_credential (api_key)");
        assert!(hit.snippet.starts_with("api_key=\""), "{}", hit.snippet);
        assert!(hit.snippet.ends_with("***\""), "{}", hit.snippet);
        assert!(scan_line(r#"token = "Abc123Xyz789Qrs""#).is_some());
    }

    #[test]
    fn vendor_patterns_still_fire_whole_match() {
        let hit = scan_line(r#"KEY = "AKIAABCDEFGHIJKLMNOP""#).expect("aws hit");
        assert_eq!(hit.label, "aws_access_key");
        assert_eq!(hit.snippet, "AKIAABCD***");
    }

    #[test]
    fn redaction_never_keeps_a_usable_secret() {
        let secret = "sk-abcdefghijklmnopqrstuvwxyzyxwvutsrqponm";
        let r = redact(secret);
        assert_eq!(r.len(), 11);
        assert!(
            !r.contains(&secret[8..]),
            "redacted output must not leak the tail"
        );
    }

    #[test]
    fn entropy_and_class_signals() {
        assert!(shannon_entropy("aaaaaaaa") < 0.1);
        assert!(shannon_entropy("a1b2c3d4e5f6g7h8") > 3.5);
        assert!(mixes_character_classes("Abc123Xyz789"));
        assert!(!mixes_character_classes("abc123xyz789"));
        assert!(!mixes_character_classes("ABC123XYZ789"));
    }
}
