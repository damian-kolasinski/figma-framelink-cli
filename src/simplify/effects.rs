//! Effects transformer — port of `transformers/effects.ts`.

use serde_json::{Map, Value};

use super::pixel_round;
use crate::simplify::style::format_rgba;

pub fn build_simplified_effects(n: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(effects) = n.get("effects").and_then(|v| v.as_array()) else {
        return out;
    };
    let visible: Vec<&Value> = effects
        .iter()
        .filter(|e| e.get("visible").and_then(|v| v.as_bool()).unwrap_or(true))
        .collect();

    let mut shadows: Vec<String> = vec![];
    for e in &visible {
        match e.get("type").and_then(|v| v.as_str()) {
            Some("DROP_SHADOW") => {
                let ox = e
                    .pointer("/offset/x")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let oy = e
                    .pointer("/offset/y")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let r = e.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let s = e.get("spread").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let c = e.get("color").cloned().unwrap_or(Value::Null);
                shadows.push(format!(
                    "{ox}px {oy}px {r}px {s}px {}",
                    format_rgba(&c, 1.0)
                ));
            }
            Some("INNER_SHADOW") => {
                let ox = e
                    .pointer("/offset/x")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let oy = e
                    .pointer("/offset/y")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let r = e.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let s = e.get("spread").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let c = e.get("color").cloned().unwrap_or(Value::Null);
                shadows.push(format!(
                    "inset {}px {}px {}px {}px {}",
                    super::js_num(ox),
                    super::js_num(oy),
                    super::js_num(r),
                    super::js_num(s),
                    format_rgba(&c, 1.0)
                ));
            }
            _ => {}
        }
    }
    // Figma blur ≈ 2× CSS blur.
    let filters: Vec<String> = visible
        .iter()
        .filter(|e| e.get("type").and_then(|v| v.as_str()) == Some("LAYER_BLUR"))
        .filter_map(|e| e.get("radius").and_then(|v| v.as_f64()))
        .filter(|r| *r > 0.0)
        .map(|r| format!("blur({}px)", super::js_num(pixel_round(r / 2.0))))
        .collect();
    let backdrops: Vec<String> = visible
        .iter()
        .filter(|e| e.get("type").and_then(|v| v.as_str()) == Some("BACKGROUND_BLUR"))
        .filter_map(|e| e.get("radius").and_then(|v| v.as_f64()))
        .filter(|r| *r > 0.0)
        .map(|r| format!("blur({}px)", super::js_num(pixel_round(r / 2.0))))
        .collect();

    if !shadows.is_empty() {
        let key = if n.get("type").and_then(|v| v.as_str()) == Some("TEXT") {
            "textShadow"
        } else {
            "boxShadow"
        };
        out.insert(key.to_string(), Value::String(shadows.join(", ")));
    }
    if !filters.is_empty() {
        out.insert("filter".to_string(), Value::String(filters.join(" ")));
    }
    if !backdrops.is_empty() {
        out.insert(
            "backdropFilter".to_string(),
            Value::String(backdrops.join(" ")),
        );
    }
    out
}
