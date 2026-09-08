//! Text transformer — port of `transformers/text.ts`:
//! base text-style extraction + rich-text (markdown + {tsN} refs) rendering.

use serde_json::{Map, Value};

use super::pixel_round;
use crate::simplify::style::parse_paint;

pub fn is_text_node(n: &Value) -> bool {
    n.get("type").and_then(|v| v.as_str()) == Some("TEXT")
}

pub fn has_text_style(n: &Value) -> bool {
    n.get("style")
        .and_then(|v| v.as_object())
        .map(|m| !m.is_empty())
        .unwrap_or(false)
}

fn em_round(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn pick_non_zero_flags(flags: Option<&Map<String, Value>>) -> Option<Map<String, Value>> {
    let f = flags?;
    let nz: Map<String, Value> = f
        .iter()
        .filter(|(_, v)| v.as_u64().unwrap_or(0) != 0 || v.as_f64().unwrap_or(0.0) != 0.0)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if nz.is_empty() { None } else { Some(nz) }
}

fn format_line_height(source: &Map<String, Value>, font_size: Option<f64>) -> Option<String> {
    let unit = source.get("lineHeightUnit").and_then(|v| v.as_str());
    let px = source.get("lineHeightPx").and_then(|v| v.as_f64());
    let pct = source
        .get("lineHeightPercentFontSize")
        .and_then(|v| v.as_f64());
    match unit {
        Some("INTRINSIC_%") => None,
        Some("PIXELS") => px.map(|p| format!("{}px", super::js_num(pixel_round(p)))),
        Some("FONT_SIZE_%") => pct.map(|p| format!("{}em", super::js_num(pixel_round(p / 100.0)))),
        _ => match (px, font_size) {
            (Some(p), Some(f)) if f != 0.0 => {
                Some(format!("{}em", super::js_num(pixel_round(p / f))))
            }
            _ => None,
        },
    }
}

/// Base text style → simplified object (fills handled by visuals extractor).
pub fn extract_text_style(n: &Value) -> Option<Value> {
    let style = n.get("style")?.as_object()?;
    let mut out = Map::new();
    if let Some(v) = style.get("fontFamily").and_then(|v| v.as_str()) {
        out.insert("fontFamily".to_string(), Value::String(v.to_string()));
    }
    if let Some(v) = style.get("fontStyle").and_then(|v| v.as_str())
        && !v.is_empty()
    {
        out.insert("fontStyle".to_string(), Value::String(v.to_string()));
    }
    if let Some(v) = style.get("fontWeight").and_then(|v| v.as_f64()) {
        // Verbatim value, integer rendering (JS semantics).
        out.insert("fontWeight".to_string(), super::num_raw(v));
    }
    if let Some(v) = style.get("fontSize").and_then(|v| v.as_f64()) {
        // Verbatim like upstream — no rounding (feeds content hashes).
        out.insert("fontSize".to_string(), super::num_raw(v));
    }
    let font_size = style.get("fontSize").and_then(|v| v.as_f64());
    if let Some(lh) = format_line_height(style, font_size) {
        out.insert("lineHeight".to_string(), Value::String(lh));
    }
    if let (Some(ls), Some(fs)) = (
        style.get("letterSpacing").and_then(|v| v.as_f64()),
        font_size,
    ) && ls != 0.0
        && fs != 0.0
    {
        out.insert(
            "letterSpacing".to_string(),
            Value::String(format!("{}em", super::js_num(em_round(ls / fs)))),
        );
    }
    for key in ["textCase", "textAlignHorizontal", "textAlignVertical"] {
        if let Some(v) = style.get(key).and_then(|v| v.as_str()) {
            out.insert(key.to_string(), Value::String(v.to_string()));
        }
    }
    if style.get("italic").and_then(|v| v.as_bool()) == Some(true) {
        out.insert("italic".to_string(), Value::Bool(true));
    }
    if let Some(td) = style.get("textDecoration").and_then(|v| v.as_str())
        && (td == "STRIKETHROUGH" || td == "UNDERLINE")
    {
        out.insert("textDecoration".to_string(), Value::String(td.to_string()));
    }
    if let Some(h) = style.get("hyperlink")
        && !h.is_null()
    {
        out.insert("hyperlink".to_string(), h.clone());
    }
    if let Some(f) = pick_non_zero_flags(style.get("opentypeFlags").and_then(|v| v.as_object())) {
        // Normalize whole numbers (JS semantics) for hash stability.
        let normalized: Map<String, Value> = f
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.as_f64().map(super::num_raw).unwrap_or_else(|| v.clone()),
                )
            })
            .collect();
        out.insert("opentypeFlags".to_string(), Value::Object(normalized));
    }
    for key in ["paragraphSpacing", "paragraphIndent", "listSpacing"] {
        if let Some(v) = style.get(key).and_then(|v| v.as_f64())
            && v > 0.0
        {
            // Verbatim like upstream — no rounding.
            out.insert(key.to_string(), super::num_raw(v));
        }
    }
    // Drop undefined-ish empties: remove nulls.
    out.retain(|_, v| !v.is_null());
    if out.is_empty() {
        None
    } else {
        Some(Value::Object(out))
    }
}

// ---------------------------------------------------------------------------
// Rich text
// ---------------------------------------------------------------------------

fn weight_as_u64(v: &Value) -> Option<u64> {
    if let Some(w) = v.as_u64() {
        return Some(w);
    }
    // Tolerate integral floats (e.g. 700.0): same value, JS renders `700`.
    v.as_f64()
        .filter(|w| w.fract() == 0.0 && (0.0..=1000.0).contains(w))
        .map(|w| w as u64)
}

pub struct FormattedText {
    pub text: Option<String>,
    pub bold_weight: Option<u64>,
}

const IGNORED_FIELDS: [&str; 5] = [
    "semanticWeight",
    "semanticItalic",
    "isOverrideOverTextStyle",
    "fontPostScriptName",
    "boundVariables",
];

#[derive(Clone, Default)]
struct Delta(Map<String, Value>);

pub fn build_formatted_text(
    node: &Value,
    register: &mut dyn FnMut(Value) -> String,
) -> FormattedText {
    let characters = node
        .get("characters")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if characters.is_empty() {
        return FormattedText {
            text: None,
            bold_weight: None,
        };
    }
    let overrides: Vec<u64> = node
        .get("characterStyleOverrides")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| super::uint(v).unwrap_or(0)).collect())
        .unwrap_or_default();
    let line_types: Vec<String> = node
        .get("lineTypes")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|v| v.as_str().unwrap_or("NONE").to_string())
                .collect()
        })
        .unwrap_or_default();
    let line_indents: Vec<usize> = node
        .get("lineIndentations")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|v| super::uint(v).unwrap_or(0) as usize)
                .collect()
        })
        .unwrap_or_default();

    let Some(base) = node.get("style").and_then(|v| v.as_object()) else {
        return FormattedText {
            text: Some(escape_markdown(characters)),
            bold_weight: None,
        };
    };

    let has_overrides = overrides.iter().any(|id| *id != 0);
    let has_list = line_types
        .iter()
        .any(|t| t == "ORDERED" || t == "UNORDERED");
    if !has_overrides && !has_list {
        return FormattedText {
            text: Some(escape_markdown(characters)),
            bold_weight: None,
        };
    }

    let codepoints: Vec<char> = characters.chars().collect();
    let table = node
        .get("styleOverrideTable")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let lines = split_lines(&codepoints, &overrides);

    let mut per_line: Vec<Vec<(String, Delta)>> = vec![];
    for line in &lines {
        per_line.push(compute_runs(&line.0, &line.1, &table, base));
    }

    let flat: Vec<(String, Delta)> = per_line.iter().flatten().cloned().collect();
    let bold_weight = detect_bold_weight(&flat, base);

    let mut counters: std::collections::HashMap<usize, usize> = Default::default();
    let mut rendered: Vec<String> = vec![];
    for (i, runs) in per_line.iter().enumerate() {
        let mut line_out = String::new();
        for (text, delta) in runs {
            let c = classify_run(delta, base, bold_weight);
            line_out.push_str(&render_run(text, &c, register));
        }
        let ltype = line_types.get(i).map(|s| s.as_str()).unwrap_or("NONE");
        let depth = line_indents.get(i).copied().unwrap_or(0);
        for k in counters.clone().keys() {
            if *k > depth {
                counters.remove(k);
            }
        }
        let indent = "  ".repeat(depth);
        match ltype {
            "ORDERED" => {
                let n = counters.get(&depth).copied().unwrap_or(0) + 1;
                counters.insert(depth, n);
                rendered.push(format!("{indent}{n}. {line_out}"));
            }
            "UNORDERED" => {
                counters.remove(&depth);
                rendered.push(format!("{indent}- {line_out}"));
            }
            _ => {
                counters.remove(&depth);
                rendered.push(line_out);
            }
        }
    }
    FormattedText {
        text: Some(rendered.join("\\n")),
        bold_weight,
    }
}

/// JS `Array.prototype.slice` semantics: out-of-range ends clamp instead of
/// failing. Figma omits trailing zero override entries, so the overrides
/// array is routinely shorter than the characters — a strict slice would
/// discard the whole line's overrides (killing all inline formatting).
fn slice_clamped(overrides: &[u64], start: usize, end: usize) -> Vec<u64> {
    let end = end.min(overrides.len());
    if start >= end {
        return vec![];
    }
    overrides[start..end].to_vec()
}

fn split_lines(chars: &[char], overrides: &[u64]) -> Vec<(Vec<char>, Vec<u64>)> {
    let mut lines = vec![];
    let mut start = 0;
    // Iterate with a sentinel past the end.
    let mut i = 0;
    while i <= chars.len() {
        let ch = if i < chars.len() {
            Some(chars[i])
        } else {
            None
        };
        if ch == Some('\n') || ch == Some('\u{2029}') || ch.is_none() {
            lines.push((chars[start..i].to_vec(), slice_clamped(overrides, start, i)));
            start = i + 1;
        }
        i += 1;
    }
    lines
}

fn compute_delta(id: u64, table: &Map<String, Value>, base: &Map<String, Value>) -> Delta {
    if id == 0 {
        return Delta(Map::new());
    }

    let Some(ov) = table.get(&id.to_string()).and_then(|v| v.as_object()) else {
        return Delta(Map::new());
    };
    let mut d = Map::new();
    for (k, v) in ov {
        if IGNORED_FIELDS.contains(&k.as_str()) || v.is_null() {
            continue;
        }
        let bv = base.get(k);
        if serde_json::to_string(bv.unwrap_or(&Value::Null)).ok() == serde_json::to_string(v).ok() {
            continue;
        }
        d.insert(k.clone(), v.clone());
    }
    Delta(d)
}

fn deltas_equal(a: &Delta, b: &Delta) -> bool {
    crate::simplify::stable_stringify(&Value::Object(a.0.clone()))
        == crate::simplify::stable_stringify(&Value::Object(b.0.clone()))
}

fn compute_runs(
    chars: &[char],
    overrides: &[u64],
    table: &Map<String, Value>,
    base: &Map<String, Value>,
) -> Vec<(String, Delta)> {
    if chars.is_empty() {
        return vec![];
    }
    let mut raw: Vec<(String, Delta)> = vec![];
    let mut start = 0;
    let mut i = 0;
    while i <= chars.len() {
        let cur: i64 = if i < chars.len() {
            overrides.get(i).copied().unwrap_or(0) as i64
        } else {
            -1
        };
        let start_id: i64 = if start < chars.len() {
            overrides.get(start).copied().unwrap_or(0) as i64
        } else {
            0
        };
        if (i == chars.len() || cur != start_id) && i > start {
            let text: String = chars[start..i].iter().collect();
            raw.push((text, compute_delta(start_id as u64, table, base)));
            start = i;
        }
        i += 1;
    }
    let mut runs: Vec<(String, Delta)> = vec![];
    for (t, d) in raw {
        if let Some(last) = runs.last_mut()
            && deltas_equal(&last.1, &d)
        {
            last.0.push_str(&t);
            continue;
        }
        runs.push((t, d));
    }
    runs
}

fn detect_bold_weight(runs: &[(String, Delta)], base: &Map<String, Value>) -> Option<u64> {
    let base_w = base
        .get("fontWeight")
        .and_then(weight_as_u64)
        .unwrap_or(400);
    let mut counts: std::collections::HashMap<u64, usize> = Default::default();
    for (text, d) in runs {
        if let Some(w) = d.0.get("fontWeight").and_then(weight_as_u64)
            && w > base_w
        {
            *counts.entry(w).or_insert(0) += text.chars().count();
        }
    }
    if counts.is_empty() {
        return None;
    }
    let mut best: Option<u64> = None;
    let mut best_n = 0usize;
    for (w, n) in counts {
        if n > best_n || (n == best_n && w > best.unwrap_or(0)) {
            best = Some(w);
            best_n = n;
        }
    }
    best
}

struct Classification {
    bold: bool,
    italic: bool,
    strike: bool,
    url: Option<String>,
    ref_delta: Option<Value>,
}

fn classify_run(
    delta: &Delta,
    base: &Map<String, Value>,
    bold_weight: Option<u64>,
) -> Classification {
    let mut c = Classification {
        bold: false,
        italic: false,
        strike: false,
        url: None,
        ref_delta: None,
    };
    let mut refd = Map::new();
    let base_w = base
        .get("fontWeight")
        .and_then(weight_as_u64)
        .unwrap_or(400);
    let base_italic = base
        .get("italic")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let eff_size = delta
        .0
        .get("fontSize")
        .and_then(|v| v.as_f64())
        .or_else(|| base.get("fontSize").and_then(|v| v.as_f64()))
        .unwrap_or(0.0);

    for (key, value) in &delta.0 {
        match key.as_str() {
            "fontWeight" => {
                let w = weight_as_u64(value).unwrap_or(base_w);
                if w > base_w {
                    c.bold = true;
                    if bold_weight != Some(w) && bold_weight.is_some() {
                        refd.insert("fontWeight".to_string(), Value::Number(w.into()));
                    }
                } else {
                    refd.insert("fontWeight".to_string(), Value::Number(w.into()));
                }
            }
            "italic" => {
                let it = value.as_bool().unwrap_or(false);
                if it && !base_italic {
                    c.italic = true;
                } else if !it && base_italic {
                    refd.insert("italic".to_string(), Value::Bool(false));
                }
            }
            "textDecoration" => {
                let td = value.as_str().unwrap_or("");
                let base_td = base
                    .get("textDecoration")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match td {
                    "STRIKETHROUGH" => {
                        c.strike = true;
                        if base_td == "UNDERLINE" {
                            refd.insert(
                                "textDecoration".to_string(),
                                Value::String("STRIKETHROUGH".to_string()),
                            );
                        }
                    }
                    "UNDERLINE" => {
                        refd.insert(
                            "textDecoration".to_string(),
                            Value::String("UNDERLINE".to_string()),
                        );
                    }
                    "NONE" if !base_td.is_empty() => {
                        refd.insert(
                            "textDecoration".to_string(),
                            Value::String("NONE".to_string()),
                        );
                    }
                    _ => {}
                }
            }
            "hyperlink" => {
                let t = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let url = value.get("url").and_then(|v| v.as_str()).unwrap_or("");
                if t == "URL" && !url.is_empty() {
                    c.url = Some(url.to_string());
                } else {
                    refd.insert("hyperlink".to_string(), value.clone());
                }
            }
            "fills" => {
                if let Some(paints) = value.as_array() {
                    let fills: Vec<Value> = paints
                        .iter()
                        .filter(|p| p.get("visible").and_then(|v| v.as_bool()).unwrap_or(true))
                        .map(|p| parse_paint(p, false))
                        .collect();
                    let mut rev = fills;
                    rev.reverse();
                    if !rev.is_empty() {
                        refd.insert("fills".to_string(), Value::Array(rev));
                    }
                }
            }
            "fontFamily"
            | "fontStyle"
            | "fontSize"
            | "textCase"
            | "textAlignHorizontal"
            | "textAlignVertical" => {
                refd.insert(key.clone(), value.clone());
            }
            "letterSpacing" => {
                let ls = value.as_f64().unwrap_or(0.0);
                if ls != 0.0 && eff_size != 0.0 {
                    refd.insert(
                        "letterSpacing".to_string(),
                        Value::String(format!("{}em", super::js_num(em_round(ls / eff_size)))),
                    );
                }
            }
            "lineHeightPx"
            | "lineHeightUnit"
            | "lineHeightPercent"
            | "lineHeightPercentFontSize" => {
                if refd.contains_key("lineHeight") {
                    continue;
                }
                let mut merged = base.clone();
                for k in [
                    "lineHeightPx",
                    "lineHeightUnit",
                    "lineHeightPercentFontSize",
                ] {
                    if let Some(v) = delta.0.get(k) {
                        merged.insert(k.to_string(), v.clone());
                    }
                }
                if let Some(f) = format_line_height(&merged, Some(eff_size)) {
                    refd.insert("lineHeight".to_string(), Value::String(f));
                }
            }
            "opentypeFlags" => {
                if let Some(f) = pick_non_zero_flags(value.as_object()) {
                    refd.insert("opentypeFlags".to_string(), Value::Object(f));
                }
            }
            "paragraphSpacing" | "paragraphIndent" | "listSpacing"
                if value.as_f64().unwrap_or(0.0) > 0.0 =>
            {
                refd.insert(key.clone(), value.clone());
            }
            _ => {}
        }
    }
    if !refd.is_empty() {
        c.ref_delta = Some(Value::Object(refd));
    }
    c
}

fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' | '*' | '_' | '~' | '[' | ']' | '(' | ')' | '{' | '}' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' | '\u{2029}' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out
}

fn split_edge_whitespace(text: &str) -> (String, String, String) {
    // Byte lengths of the trimmed edges are valid char boundaries because
    // trim_* remove whole characters.
    let leading_len = text.len() - text.trim_start().len();
    let core_end = text.trim_end().len();
    if core_end <= leading_len {
        // All whitespace (or empty): keep everything in `leading`, mirroring
        // the upstream lazy-regex match which yields an empty core.
        return (text.to_string(), String::new(), String::new());
    }
    (
        text[..leading_len].to_string(),
        text[leading_len..core_end].to_string(),
        text[core_end..].to_string(),
    )
}

fn escape_link_url(url: &str) -> String {
    let mut out = String::new();
    for ch in url.chars() {
        match ch {
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            c if c.is_whitespace() => out.push_str(
                &c.to_string()
                    .bytes()
                    .map(|b| format!("%{b:02X}"))
                    .collect::<String>(),
            ),
            c => out.push(c),
        }
    }
    out
}

fn render_run(raw: &str, c: &Classification, register: &mut dyn FnMut(Value) -> String) -> String {
    let has_md = c.bold || c.italic || c.strike || c.url.is_some();
    let (leading, core, trailing) = if has_md {
        split_edge_whitespace(raw)
    } else {
        (String::new(), raw.to_string(), String::new())
    };
    let mut inner = escape_markdown(&core);
    if c.italic {
        inner = format!("*{inner}*");
    }
    if c.bold {
        inner = format!("**{inner}**");
    }
    if c.strike {
        inner = format!("~~{inner}~~");
    }
    if let Some(url) = &c.url {
        inner = format!("[{inner}]({})", escape_link_url(url));
    }
    let mut output = format!(
        "{}{}{}",
        escape_markdown(&leading),
        inner,
        escape_markdown(&trailing)
    );
    if let Some(d) = &c.ref_delta {
        let id = register(d.clone());
        output = format!("{{{id}}}{output}{{/{id}}}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(rec: &mut Vec<Value>) -> impl FnMut(Value) -> String + '_ {
        let mut n = 0u64;
        move |d: Value| {
            n += 1;
            rec.push(d);
            format!("ts{n}")
        }
    }

    fn text_node() -> Value {
        serde_json::json!({
            "type": "TEXT",
            "characters": "edited body",
            "style": {"fontFamily": "Inter", "fontWeight": 400, "fontSize": 16.0},
            "characterStyleOverrides": [1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
            "styleOverrideTable": {"1": {"fontWeight": 700}}
        })
    }

    #[test]
    fn markdown_hugs_text_without_edge_whitespace() {
        let node = text_node();
        let mut rec = vec![];
        let out = build_formatted_text(&node, &mut reg(&mut rec));
        assert_eq!(out.text.as_deref(), Some("**edited** body"));
        assert_eq!(out.bold_weight, Some(700));
    }

    #[test]
    fn markdown_markers_skip_flanking_whitespace() {
        let mut node = text_node();
        node["characters"] = Value::String(" edited ".to_string());
        node["characterStyleOverrides"] =
            Value::Array((0..8).map(|_| Value::Number(1.into())).collect());
        let mut rec = vec![];
        let out = build_formatted_text(&node, &mut reg(&mut rec));
        assert_eq!(out.text.as_deref(), Some(" **edited** "));
    }

    #[test]
    fn repro_app_name_bold() {
        let raw: Value = serde_json::from_str(
            &std::fs::read_to_string(
                "/private/var/folders/lh/jyw7ntc1045f6pv1xs72x7q00000gp/T/opencode/raw-page.json",
            )
            .unwrap(),
        )
        .unwrap();
        let doc = &raw["nodes"]["11462:19979"]["document"];
        let mut target = None;
        fn find(n: &Value, out: &mut Option<Value>) {
            if n.get("type").and_then(|v| v.as_str()) == Some("TEXT")
                && n.get("characters")
                    .and_then(|v| v.as_str())
                    .map(|c| c.contains("By granting access"))
                    .unwrap_or(false)
                && out.is_none()
            {
                *out = Some(n.clone());
            }
            if let Some(kids) = n.get("children").and_then(|c| c.as_array()) {
                for c in kids {
                    find(c, out);
                }
            }
        }
        find(doc, &mut target);
        let node = target.expect("node found");
        let mut rec = vec![];
        let out = build_formatted_text(&node, &mut reg(&mut rec));
        // Must match upstream exactly (modulo the ts counter, which is
        // namespaced per extraction run): bold override on "[App Name]"
        // renders as markdown inside a style ref, with the trailing-omitted
        // overrides array correctly clamped per line.
        assert_eq!(
            out.text.as_deref(),
            Some("By granting access for {ts1}**\\[App Name\\]**{/ts1}, it will be able to:")
        );
        assert_eq!(out.bold_weight, Some(600));
    }

    #[test]
    fn split_edges() {
        assert_eq!(
            split_edge_whitespace("edited"),
            ("".to_string(), "edited".to_string(), "".to_string())
        );
        assert_eq!(
            split_edge_whitespace("  a b "),
            ("  ".to_string(), "a b".to_string(), " ".to_string())
        );
    }
}
