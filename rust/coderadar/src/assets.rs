//! Bundled docs and agent skills, embedded so the Rust binary is standalone.

const QUERY_LANGUAGE: &str = include_str!("../../../py_agent/src/coderadar/docs/query-language.md");
const CLI_SKILL: &str =
    include_str!("../../../py_agent/src/coderadar/skills/coderadar-cli/SKILL.md");
const MCP_SKILL: &str =
    include_str!("../../../py_agent/src/coderadar/skills/coderadar-mcp/SKILL.md");

pub const DOC_NAMES: &[&str] = &["query-language"];
pub const DOC_DESCRIPTIONS: &[&str] =
    &["Query language and field reference (generated from the grammar)."];
pub const SKILL_NAMES: &[&str] = &["coderadar-cli", "coderadar-mcp"];

/// Verbatim text of a bundled reference document.
pub fn doc_text(name: &str) -> Option<&'static str> {
    match name {
        "query-language" => Some(QUERY_LANGUAGE),
        _ => None,
    }
}

/// Verbatim text of a bundled skill pack.
pub fn skill_text(name: &str) -> Option<&'static str> {
    match name {
        "coderadar-cli" => Some(CLI_SKILL),
        "coderadar-mcp" => Some(MCP_SKILL),
        _ => None,
    }
}

/// The `description:` field from a skill's YAML frontmatter.
pub fn skill_description(name: &str) -> Option<&'static str> {
    let text = skill_text(name)?;
    for line in text.lines().skip(1) {
        if line.trim() == "---" {
            break;
        }
        if let Some(value) = line.strip_prefix("description:") {
            let value = value.trim();
            return Some(
                value
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or(value),
            );
        }
    }
    None
}
