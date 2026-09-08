//! Output serialization: yaml / json / tree.
//! Mirrors upstream `utils/serialize*.ts` + `serializable-design.ts`.

use serde_json::{Map, Value};

use crate::simplify::SimplifiedDesign;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Yaml,
    Json,
    Tree,
}

pub fn serialize_design(design: &SimplifiedDesign, format: OutputFormat) -> String {
    let wrapped = wrap_for_serialization(design);
    match format {
        OutputFormat::Json => {
            serde_json::to_string_pretty(&wrapped).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Yaml => serde_yaml::to_string(&wrapped).unwrap_or_default(),
        OutputFormat::Tree => serialize_as_tree(&wrapped),
    }
}

fn wrap_for_serialization(design: &SimplifiedDesign) -> Value {
    let nodes: Vec<Value> = design
        .nodes
        .iter()
        .map(|n| strip_noise_name(n, &design.elements))
        .collect();
    serde_json::json!({
        "metadata": {
            "name": design.name,
            "components": design.components,
            "componentSets": design.component_sets,
        },
        "nodes": nodes,
        "globalVars": { "styles": design.global_vars },
        "elements": design.elements,
    })
}

fn node_type_of(node: &Value, elements: &Map<String, Value>) -> Option<String> {
    if let Some(t) = node.get("type").and_then(|v| v.as_str()) {
        return Some(t.to_string());
    }
    if let Some(t) = node.get("template").and_then(|v| v.as_str()) {
        return elements
            .get(t)
            .and_then(|e| e.get("type"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }
    None
}

fn is_noise_name(name: &str, node_type: Option<&str>) -> bool {
    if node_type == Some("TEXT") {
        return true;
    }
    // Auto-generated `Word 123` layer names (mirror upstream pattern).
    const KINDS: [&str; 20] = [
        "Frame",
        "Rectangle",
        "Ellipse",
        "Line",
        "Vector",
        "Group",
        "Component",
        "Instance",
        "Polygon",
        "Star",
        "Text",
        "Image",
        "Slice",
        "Section",
        "Boolean",
        "Union",
        "Subtract",
        "Intersect",
        "Exclude",
        "Arrow",
    ];
    if let Some((head, tail)) = name.rsplit_once(' ')
        && tail.chars().all(|c| c.is_ascii_digit())
        && !tail.is_empty()
    {
        // "Subtract 1" etc: head must be exactly a kind word.
        if KINDS.contains(&head) {
            return true;
        }
    }
    false
}

fn strip_noise_name(node: &Value, elements: &Map<String, Value>) -> Value {
    let Some(obj) = node.as_object() else {
        return node.clone();
    };
    let mut next = obj.clone();
    if let Some(kids) = obj.get("children").and_then(|v| v.as_array()) {
        next.insert(
            "children".to_string(),
            Value::Array(kids.iter().map(|k| strip_noise_name(k, elements)).collect()),
        );
    }
    if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
        let t = node_type_of(node, elements);
        if is_noise_name(name, t.as_deref()) {
            // shift_remove preserves the remaining fields' order.
            next.shift_remove("name");
        }
    }
    Value::Object(next)
}

// ---------------------------------------------------------------------------
// Tree format (token-efficient indented lines)
// ---------------------------------------------------------------------------

fn serialize_as_tree(design: &Value) -> String {
    let mut sections: Vec<String> = vec![];
    let name = design
        .pointer("/metadata/name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    sections.push(format!("NAME: {}", json_quote(name)));

    let styles = design
        .pointer("/globalVars/styles")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    if !styles.is_empty() {
        sections.push(format!(
            "\nGLOBAL_VARS:\n{}",
            serde_yaml::to_string(&styles).unwrap_or_default()
        ));
    }
    let elements = design
        .get("elements")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    if !elements.is_empty() {
        sections.push(format!(
            "ELEMENTS:\n{}",
            serde_yaml::to_string(&elements).unwrap_or_default()
        ));
    }
    let components = design
        .pointer("/metadata/components")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    if !components.is_empty() {
        sections.push(format!(
            "COMPONENTS:\n{}",
            serde_yaml::to_string(&components).unwrap_or_default()
        ));
    }
    let sets = design
        .pointer("/metadata/componentSets")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    if !sets.is_empty() {
        sections.push(format!(
            "COMPONENT_SETS:\n{}",
            serde_yaml::to_string(&sets).unwrap_or_default()
        ));
    }
    let mut lines = vec!["NODES:".to_string()];
    if let Some(nodes) = design.get("nodes").and_then(|v| v.as_array()) {
        for n in nodes {
            render_node(n, 0, &mut lines, &elements);
        }
    }
    sections.push(lines.join("\n"));
    sections.join("\n")
}

fn render_node(node: &Value, depth: usize, out: &mut Vec<String>, elements: &Map<String, Value>) {
    let indent = "  ".repeat(depth);
    let mut parts: Vec<String> = vec![];
    let element = node
        .get("template")
        .and_then(|v| v.as_str())
        .and_then(|t| elements.get(t));
    let ntype = element
        .and_then(|e| e.get("type"))
        .and_then(|v| v.as_str())
        .or_else(|| node.get("type").and_then(|v| v.as_str()))
        .unwrap_or("?");
    parts.push(format!("[{ntype}]"));
    if let Some(name) = node.get("name").and_then(|v| v.as_str()) {
        parts.push(json_quote(name));
    }
    parts.push(format!(
        "#{}",
        node.get("id").and_then(|v| v.as_str()).unwrap_or("?")
    ));
    if let Some(t) = node.get("template").and_then(|v| v.as_str()) {
        parts.push(format!("template={t}"));
    }
    for key in [
        "layout",
        "fills",
        "strokes",
        "strokeWeight",
        "strokeWeights",
        "strokeDashes",
        "effects",
        "opacity",
        "borderRadius",
        "styles",
        "componentId",
        "componentProperties",
        "componentPropertyReferences",
        "textStyle",
        "boldWeight",
        "text",
    ] {
        let Some(v) = node.get(key) else { continue };
        match key {
            "strokeDashes" => {
                if let Some(a) = v.as_array() {
                    parts.push(format!(
                        "strokeDashes={}",
                        a.iter()
                            .map(|x| x.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    ));
                }
            }
            "opacity" | "boldWeight" => parts.push(format!("{key}={v}")),
            "text" => {
                if let Some(s) = v.as_str() {
                    parts.push(format!("text={}", json_quote(s)));
                }
            }
            "componentProperties" | "componentPropertyReferences" => {
                parts.push(format!(
                    "{key}={}",
                    serde_json::to_string(v).unwrap_or_default()
                ));
            }
            _ => parts.push(format!("{key}={}", render_style_value(v))),
        }
    }
    out.push(format!("{indent}{}", parts.join(" ")));
    if let Some(kids) = node.get("children").and_then(|v| v.as_array()) {
        for k in kids {
            render_node(k, depth + 1, out, elements);
        }
    }
}

fn render_style_value(v: &Value) -> String {
    match v {
        Value::String(s) => maybe_quote(s),
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}

fn json_quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
}

fn maybe_quote(s: &str) -> String {
    if s.chars().any(|c| c.is_whitespace() || c == '"') {
        json_quote(s)
    } else {
        s.to_string()
    }
}
