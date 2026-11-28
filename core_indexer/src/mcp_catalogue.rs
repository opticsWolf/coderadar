//! The MCP tool catalogue.
//!
//! One declarative table is the single source of truth for `tools/list` and
//! for dispatch defaults. The shared tools are captured from the Python
//! server (`rust/golden/mcp_tools.json`), so `inputSchema` stays identical by
//! construction; the Rust-only tools live next to them in
//! `rust/golden/mcp_extra_tools.json`.

use std::sync::OnceLock;

use serde_json::{json, Map, Value};

const SHARED_TOOLS: &str = include_str!("../../rust/golden/mcp_tools.json");
const EXTRA_TOOLS: &str = include_str!("../../rust/golden/mcp_extra_tools.json");
const OVERRIDES: &str = include_str!("../../rust/golden/mcp_schema_overrides.json");

/// One advertised MCP tool.
#[derive(Debug)]
pub struct ToolSpec {
    name: String,
    description: String,
    schema: Value,
}

impl ToolSpec {
    /// The tool name a client calls.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The human-readable summary clients show in a tool picker.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The JSON Schema for the tool's `arguments` object.
    pub fn schema(&self) -> &Value {
        &self.schema
    }

    fn properties(&self) -> Option<&Map<String, Value>> {
        self.schema.get("properties")?.as_object()
    }

    /// Every declared argument name, in schema order.
    pub fn arg_names(&self) -> Vec<&str> {
        self.properties()
            .map(|map| map.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// The arguments a client must send.
    pub fn required(&self) -> Vec<&str> {
        self.schema
            .get("required")
            .and_then(Value::as_array)
            .map(|rows| rows.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }

    /// Whether the schema declares `key`.
    pub fn declares(&self, key: &str) -> bool {
        self.properties().is_some_and(|map| map.contains_key(key))
    }

    /// The JSON default for `key`, when the schema has one.
    pub fn default_of(&self, key: &str) -> Option<&Value> {
        self.properties()?.get(key)?.get("default")
    }

    /// The MCP `Tool` object clients receive from `tools/list`.
    pub fn to_mcp(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.schema,
        })
    }
}

fn parse(raw: &str) -> Result<Vec<ToolSpec>, String> {
    let document: Value =
        serde_json::from_str(raw).map_err(|error| format!("tool catalogue: {error}"))?;
    let rows = document
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| "tool catalogue: missing `tools` array".to_string())?;
    let mut specs = Vec::with_capacity(rows.len());
    for row in rows {
        let name = row
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "tool catalogue: tool without a name".to_string())?;
        let description = row
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let schema = row
            .get("inputSchema")
            .cloned()
            .ok_or_else(|| format!("tool catalogue: `{name}` has no inputSchema"))?;
        if schema.get("type").and_then(Value::as_str) != Some("object") {
            return Err(format!(
                "tool catalogue: `{name}` inputSchema must be an object"
            ));
        }
        specs.push(ToolSpec {
            name: name.to_string(),
            description: description.to_string(),
            schema,
        });
    }
    Ok(specs)
}

/// Additive schema extensions: Rust accepts a few arguments the Python
/// server does not advertise (`apply`, `store_path`, ...). Merging them here
/// keeps the shared schema byte-identical while the extra keys still validate.
fn apply_overrides(specs: &mut [ToolSpec]) {
    let Ok(document) = serde_json::from_str::<Value>(OVERRIDES) else {
        eprintln!("tool catalogue: overrides are not valid JSON");
        return;
    };
    let Some(rows) = document.get("tools").and_then(Value::as_array) else {
        return;
    };
    for row in rows {
        let Some(name) = row.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(spec) = specs.iter_mut().find(|spec| spec.name() == name) else {
            eprintln!("tool catalogue: override for unknown tool `{name}`");
            continue;
        };
        let extra = row.get("properties").and_then(Value::as_object);
        let properties = spec
            .schema
            .get_mut("properties")
            .and_then(Value::as_object_mut);
        if let (Some(extra), Some(properties)) = (extra, properties) {
            for (key, value) in extra {
                properties.insert(key.clone(), value.clone());
            }
        }
    }
}

fn load() -> Vec<ToolSpec> {
    let mut specs = Vec::new();
    for raw in [SHARED_TOOLS, EXTRA_TOOLS] {
        match parse(raw) {
            Ok(mut batch) => specs.append(&mut batch),
            Err(error) => eprintln!("{error}"),
        }
    }
    apply_overrides(&mut specs);
    specs
}

/// Every advertised tool, shared first, then the Rust-only additions.
pub fn specs() -> &'static [ToolSpec] {
    static SPECS: OnceLock<Vec<ToolSpec>> = OnceLock::new();
    SPECS.get_or_init(load)
}

/// Find one tool by name.
pub fn find(name: &str) -> Option<&'static ToolSpec> {
    specs().iter().find(|spec| spec.name() == name)
}

/// A tool call's arguments, read through the catalogue.
///
/// Getters fall back to the JSON Schema default, so a default lives in exactly
/// one place. `new` rejects arguments the schema does not declare, matching
/// the Python server's keyword strictness.
pub struct Args<'a> {
    spec: &'static ToolSpec,
    raw: &'a Value,
}

impl<'a> Args<'a> {
    /// Validate the argument object against the tool's schema.
    ///
    /// # Errors
    /// Returns a message for a non-object payload or an undeclared argument.
    pub fn new(spec: &'static ToolSpec, raw: &'a Value) -> Result<Self, String> {
        if !raw.is_null() && !raw.is_object() {
            return Err("InvalidRequest: tool arguments must be a JSON object".to_string());
        }
        if let Some(map) = raw.as_object() {
            for key in map.keys() {
                if !spec.declares(key) && !key.starts_with('_') {
                    return Err(format!(
                        "InvalidRequest: `{}` has no argument `{key}`",
                        spec.name()
                    ));
                }
            }
        }
        Ok(Self { spec, raw })
    }

    /// The raw JSON for `key`.
    pub fn raw(&self, key: &str) -> Option<&'a Value> {
        debug_assert!(self.spec.declares(key), "undeclared argument `{key}`");
        self.raw.get(key).filter(|value| !value.is_null())
    }

    /// A string argument, or its schema default, or `""`.
    pub fn str(&self, key: &str) -> String {
        self.opt_str(key).unwrap_or_default()
    }

    /// A string argument when present or defaulted.
    pub fn opt_str(&self, key: &str) -> Option<String> {
        if let Some(value) = self.raw(key).and_then(Value::as_str) {
            return Some(value.to_string());
        }
        self.default_of(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// An integer argument, or its schema default, or `0`.
    pub fn i64(&self, key: &str) -> i64 {
        if let Some(value) = self.raw(key).and_then(Value::as_i64) {
            return value;
        }
        self.default_of(key).and_then(Value::as_i64).unwrap_or(0)
    }

    /// A number argument, or its schema default, or `0.0`.
    pub fn f64(&self, key: &str) -> f64 {
        if let Some(value) = self.raw(key).and_then(Value::as_f64) {
            return value;
        }
        self.default_of(key).and_then(Value::as_f64).unwrap_or(0.0)
    }

    /// A boolean argument, or its schema default, or `false`.
    pub fn bool(&self, key: &str) -> bool {
        if let Some(value) = self.raw(key).and_then(Value::as_bool) {
            return value;
        }
        self.default_of(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// An array-of-strings argument, or its schema default.
    pub fn strings(&self, key: &str) -> Option<Vec<String>> {
        let array = self
            .raw(key)
            .or_else(|| self.default_of(key))
            .and_then(Value::as_array)?;
        Some(
            array
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        )
    }

    fn default_of(&self, key: &str) -> Option<&'a Value> {
        debug_assert!(self.spec.declares(key), "undeclared argument `{key}`");
        self.spec.default_of(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_parses_and_has_no_duplicates() {
        let specs = specs();
        assert_eq!(specs.len(), 49);
        let mut names: Vec<&str> = specs.iter().map(ToolSpec::name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 49);
    }

    #[test]
    fn shared_tools_keep_their_python_schema() {
        let explore = find("coderadar_explore").expect("explore");
        assert!(explore.description().starts_with("Explore the code graph"));
        let mut names = explore.arg_names();
        names.sort_unstable();
        assert_eq!(
            names,
            ["direction", "max_files", "project_path", "query", "symbols"]
        );
    }

    #[test]
    fn args_fall_back_to_schema_defaults() {
        let spec = find("coderadar_traverse").expect("traverse");
        let empty = json!({});
        let args = Args::new(spec, &empty).expect("empty args");
        assert_eq!(args.str("direction"), "both");
        assert_eq!(args.i64("max_depth"), 3);
        assert!(args.str("entity_id").is_empty());
    }

    #[test]
    fn undeclared_arguments_are_rejected() {
        let spec = find("coderadar_node").expect("node");
        let bad = json!({"nope": 1});
        let error = Args::new(spec, &bad).err().expect("error");
        assert!(error.contains("no argument `nope`"));
        let meta = json!({"_meta": {}});
        assert!(Args::new(spec, &meta).is_ok());
    }
}
