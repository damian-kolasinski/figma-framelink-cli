//! Component transformer — port of `transformers/component.ts`.

use serde_json::{Map, Value};

pub fn strip_property_name_suffix(name: &str) -> String {
    match name.find('#') {
        Some(i) => name[..i].to_string(),
        None => name.to_string(),
    }
}

pub fn simplify_property_definitions(defs: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (name, def) in defs {
        let t = def.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if t == "BOOLEAN" || t == "TEXT" {
            let mut d = Map::new();
            d.insert("type".to_string(), Value::String(t.to_lowercase()));
            if let Some(dv) = def.get("defaultValue") {
                d.insert("defaultValue".to_string(), dv.clone());
            }
            out.insert(strip_property_name_suffix(name), Value::Object(d));
        }
    }
    out
}

pub fn simplify_property_references(refs: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in refs {
        if k == "visible" || k == "characters" {
            let key = if k == "characters" {
                "text".to_string()
            } else {
                k.clone()
            };
            let val = v
                .as_str()
                .map(strip_property_name_suffix)
                .unwrap_or_default();
            out.insert(key, Value::String(val));
        }
    }
    out
}

pub fn simplify_component_properties(props: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (name, prop) in props {
        let t = prop.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if (t == "BOOLEAN" || t == "TEXT")
            && let Some(v) = prop.get("value")
        {
            out.insert(strip_property_name_suffix(name), v.clone());
        }
    }
    out
}

pub fn simplify_components(
    all: &Map<String, Value>,
    prop_defs: &Map<String, Value>,
) -> Map<String, Value> {
    let mut out = Map::new();
    for (id, comp) in all {
        let mut d = Map::new();
        d.insert("id".to_string(), Value::String(id.clone()));
        if let Some(k) = comp.get("key") {
            d.insert("key".to_string(), k.clone());
        }
        if let Some(n) = comp.get("name") {
            d.insert("name".to_string(), n.clone());
        }
        if let Some(s) = comp.get("componentSetId") {
            d.insert("componentSetId".to_string(), s.clone());
        }
        if let Some(defs) = prop_defs.get(id) {
            d.insert("propertyDefinitions".to_string(), defs.clone());
        }
        out.insert(id.clone(), Value::Object(d));
    }
    out
}

pub fn simplify_component_sets(
    all: &Map<String, Value>,
    prop_defs: &Map<String, Value>,
) -> Map<String, Value> {
    let mut out = Map::new();
    for (id, set) in all {
        let mut d = Map::new();
        d.insert("id".to_string(), Value::String(id.clone()));
        if let Some(k) = set.get("key") {
            d.insert("key".to_string(), k.clone());
        }
        if let Some(n) = set.get("name") {
            d.insert("name".to_string(), n.clone());
        }
        if let Some(desc) = set.get("description") {
            d.insert("description".to_string(), desc.clone());
        }
        if let Some(defs) = prop_defs.get(id) {
            d.insert("propertyDefinitions".to_string(), defs.clone());
        }
        out.insert(id.clone(), Value::Object(d));
    }
    out
}
