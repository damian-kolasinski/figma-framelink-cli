//! Simplify pipeline: Figma raw JSON → compact agent-ready design.
//! Faithful port of upstream `extractors/` + `services/get-figma-data.ts`,
//! operating on `serde_json::Value` (mirrors the TS dynamic field access).

pub mod component;
pub mod effects;
pub mod layout;
pub mod style;
pub mod text;

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use sha1::{Digest, Sha1};

use crate::figma::FigmaClient;
use crate::serialize::OutputFormat;

// ---------------------------------------------------------------------------
// Canonical JSON (stable key order) — mirrors `stableStringify`.
// ---------------------------------------------------------------------------

pub fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(m) => {
            let mut sorted = BTreeMap::new();
            for (k, v) in m {
                sorted.insert(k.clone(), canonical(v));
            }
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}

pub fn stable_stringify(value: &Value) -> String {
    serde_json::to_string(&canonical(value)).unwrap_or_else(|_| "null".to_string())
}

fn sha1_hex(s: &str) -> String {
    let mut h = Sha1::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

// Minimal hex encode without a dependency.
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}

pub fn pixel_round(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}

/// Format a float exactly like JavaScript's `Number.prototype.toString`
/// (which upstream uses both in hash inputs and in every template literal):
/// integers print without a decimal point (`1`, never `1.0`), `-0` prints as
/// `0`, and non-integers use shortest round-trip notation (ryu, which matches
/// JS for these magnitudes).
pub fn js_num(v: f64) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    if v.fract() == 0.0 && v.abs() < 1e21 {
        return format!("{}", v as i64);
    }
    serde_json::Number::from_f64(v)
        .map(|n| n.to_string())
        .unwrap_or_else(|| format!("{v:?}"))
}

/// JSON number with upstream parity: whole values serialize as integers
/// (`800`, not `800.0`), fractional values rounded to 2dp.
pub fn num(n: f64) -> Value {
    let r = pixel_round(n);
    if r.fract() == 0.0 && r.abs() < 9e15 {
        Value::Number((r as i64).into())
    } else {
        Value::Number(serde_json::Number::from_f64(r).unwrap())
    }
}

/// Integer field reader tolerant of float rendering (`2.0` for `2`, which
/// `Value::as_u64` rejects). Figma usually emits integers, but any integral
/// float must read identically — these feed grid anchors, override ids and
/// other structural decisions.
pub fn uint(v: &Value) -> Option<u64> {
    if let Some(w) = v.as_u64() {
        return Some(w);
    }
    v.as_f64()
        .filter(|w| w.fract() == 0.0 && *w >= 0.0 && *w < 1e15)
        .map(|w| w as u64)
}
/// Normalize a verbatim Figma float for output: whole values serialize as
/// integers (`60`, never `60.0` — matching JS `JSON.stringify`), fractional
/// values pass through untouched (NO rounding — unlike `num`). Required
/// anywhere a raw API number reaches simplified output or hash inputs.
pub fn num_raw(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 9e15 {
        Value::Number((v as i64).into())
    } else {
        Value::Number(serde_json::Number::from_f64(v).expect("finite number"))
    }
}

// ---------------------------------------------------------------------------
// Traversal state
// ---------------------------------------------------------------------------

pub struct Ctx<'a> {
    pub styles: &'a mut Map<String, Value>,
    pub style_cache: &'a mut HashMap<String, String>,
    pub inline_cache: &'a mut HashMap<String, String>,
    pub extra_styles: &'a Map<String, Value>,
    pub named_style_keys: &'a mut HashSet<String>,
    pub ts_counter: &'a mut u64,
    pub prop_defs: &'a mut Map<String, Value>,
    pub parent: Option<Value>,
    pub inside_component_def: bool,
    pub depth: u32,
    pub counter: &'a mut u64,
}

fn style_name_for(node: &Value, extra: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    let styles = node.get("styles")?.as_object()?;
    for k in keys {
        if let Some(id) = styles.get(*k).and_then(|v| v.as_str()) {
            if let Some(name) = extra
                .get(id)
                .and_then(|s| s.get("name"))
                .and_then(|n| n.as_str())
            {
                return Some(name.to_string());
            }
            // Style id present but metadata missing — still usable as key fallback.
            return Some(id.to_string());
        }
    }
    None
}

/// Register a style value: prefer the Figma named style, else a
/// content-addressed `prefix_<sha1>` id (deduped within the run).
pub fn register_style(
    ctx: &mut Ctx,
    node: &Value,
    value: Value,
    style_keys: &[&str],
    prefix: &str,
) -> String {
    if let Some(name) = style_name_for(node, ctx.extra_styles, style_keys) {
        // Same name + same value collapses; same name + different value
        // disambiguates with the style id (mirrors upstream resolveStyleKey).
        let key = match ctx.styles.get(&name) {
            None => name.clone(),
            Some(existing) if stable_stringify(existing) == stable_stringify(&value) => {
                name.clone()
            }
            Some(_) => {
                let id = node
                    .get("styles")
                    .and_then(|s| s.as_object())
                    .and_then(|m| {
                        style_keys
                            .iter()
                            .filter_map(|k| m.get(*k).and_then(|v| v.as_str()))
                            .next()
                    })
                    .unwrap_or("?");
                format!("{name} ({id})")
            }
        };
        ctx.styles.insert(key.clone(), value);
        ctx.named_style_keys.insert(key.clone());
        return key;
    }
    find_or_create_var(ctx.styles, ctx.style_cache, value, prefix)
}

pub fn find_or_create_var(
    styles: &mut Map<String, Value>,
    cache: &mut HashMap<String, String>,
    value: Value,
    prefix: &str,
) -> String {
    let key = stable_stringify(&value);
    if let Some(id) = cache.get(&key) {
        return id.clone();
    }
    let full = sha1_hex(&key);
    let mut len = 8usize;
    let var_id = loop {
        let id = format!("{prefix}_{}", &full[..len.min(full.len())]);
        if styles.get(&id).is_none() {
            break id;
        }
        len += 4;
        if len >= full.len() {
            break format!("{prefix}_{full}");
        }
    };
    styles.insert(var_id.clone(), value);
    cache.insert(key, var_id.clone());
    var_id
}

/// Inline text-style override deltas get short sequential `tsN` ids.
pub fn register_inline_style(ctx: &mut Ctx, delta: Value) -> String {
    let key = stable_stringify(&delta);
    if let Some(id) = ctx.inline_cache.get(&key) {
        return id.clone();
    }
    *ctx.ts_counter += 1;
    let id = format!("ts{}", *ctx.ts_counter);
    ctx.styles.insert(id.clone(), delta);
    ctx.inline_cache.insert(key, id.clone());
    id
}

// ---------------------------------------------------------------------------
// Built-in extractors (mirror upstream built-in.ts)
// ---------------------------------------------------------------------------

fn layout_extractor(node: &Value, result: &mut Map<String, Value>, ctx: &mut Ctx) {
    let layout = layout::build_simplified_layout(node, ctx.parent.as_ref());
    if layout.len() > 1 {
        let id = find_or_create_var(ctx.styles, ctx.style_cache, Value::Object(layout), "layout");
        result.insert("layout".to_string(), Value::String(id));
    }
}

fn text_extractor(node: &Value, result: &mut Map<String, Value>, ctx: &mut Ctx) {
    if text::is_text_node(node) {
        let register = &mut *ctx as *mut Ctx;
        // SAFETY: only used synchronously within this call; the closure
        // inserts into ctx.styles/inline_cache through the raw pointer.
        // Encapsulated to satisfy the FnMut bound without borrow conflicts.
        let mut reg = |delta: Value| -> String {
            let c = unsafe { &mut *register };
            register_inline_style(c, delta)
        };
        let rich = text::build_formatted_text(node, &mut reg);
        if let Some(t) = rich.text
            && !t.is_empty()
        {
            result.insert("text".to_string(), Value::String(t));
        }
        if let Some(w) = rich.bold_weight {
            result.insert("boldWeight".to_string(), Value::Number(w.into()));
        }
    }
    if text::has_text_style(node)
        && let Some(style) = text::extract_text_style(node)
    {
        let id = register_style(ctx, node, style, &["text", "typography"], "style");
        result.insert("textStyle".to_string(), Value::String(id));
    }
}

fn visuals_extractor(node: &Value, result: &mut Map<String, Value>, ctx: &mut Ctx) {
    let has_children = node
        .get("children")
        .and_then(|c| c.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);

    if let Some(fills) = node.get("fills").and_then(|f| f.as_array())
        && !fills.is_empty()
    {
        // Mirror upstream: when every paint is invisible the visible subset is
        // empty, but the (empty) fills array is still registered — it may
        // dedupe into a shared `fill_*` var rather than being dropped.
        let visible: Vec<&Value> = fills.iter().filter(|f| is_visible(f)).collect();
        let owned: Vec<Value> = visible.into_iter().cloned().collect();
        let simplified: Value = match style::flatten_solid_fills(&owned) {
            Some(single) => Value::Array(vec![Value::String(single)]),
            None => {
                let mut arr: Vec<Value> = owned
                    .iter()
                    .map(|f| style::parse_paint(f, has_children))
                    .collect();
                arr.reverse(); // CSS top-first order
                Value::Array(arr)
            }
        };
        let id = register_style(ctx, node, simplified, &["fill", "fills"], "fill");
        result.insert("fills".to_string(), Value::String(id));
    }

    let strokes = style::build_simplified_strokes(node, has_children);
    if !strokes.colors.is_empty() {
        let id = register_style(
            ctx,
            node,
            Value::Array(strokes.colors),
            &["stroke", "strokes"],
            "fill",
        );
        result.insert("strokes".to_string(), Value::String(id));
        if let Some(w) = strokes.stroke_weight {
            result.insert("strokeWeight".to_string(), Value::String(w));
        }
        if let Some(d) = strokes.stroke_dashes {
            result.insert("strokeDashes".to_string(), Value::Array(d));
        }
        if let Some(w) = strokes.stroke_weights {
            result.insert("strokeWeights".to_string(), Value::String(w));
        }
        if let Some(a) = strokes.stroke_align {
            result.insert("strokeAlign".to_string(), Value::String(a));
        }
    }

    let fx = effects::build_simplified_effects(node);
    if !fx.is_empty() {
        let id = register_style(
            ctx,
            node,
            Value::Object(fx),
            &["effect", "effects"],
            "effect",
        );
        result.insert("effects".to_string(), Value::String(id));
    }

    if let Some(o) = node.get("opacity").and_then(|v| v.as_f64())
        && o != 1.0
    {
        // Verbatim like upstream (`result.opacity = node.opacity`) — no
        // rounding, but whole values render as integers (JS semantics).
        result.insert("opacity".to_string(), num_raw(o));
    }

    if let Some(r) = node.get("cornerRadius").and_then(|v| v.as_f64()) {
        result.insert(
            "borderRadius".to_string(),
            Value::String(format!("{}px", js_num(r))),
        );
    } else if let Some(radii) = node.get("rectangleCornerRadii").and_then(|v| v.as_array())
        && radii.len() == 4
        && radii.iter().all(|v| v.is_number())
    {
        let parts: Vec<String> = radii
            .iter()
            .map(|v| format!("{}px", js_num(v.as_f64().unwrap_or(0.0))))
            .collect();
        result.insert("borderRadius".to_string(), Value::String(parts.join(" ")));
    }
}

fn component_extractor(node: &Value, result: &mut Map<String, Value>, ctx: &mut Ctx) {
    let node_type = node.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if node_type == "INSTANCE" {
        if let Some(id) = node.get("componentId").and_then(|v| v.as_str()) {
            result.insert("componentId".to_string(), Value::String(id.to_string()));
        }
        if let Some(props) = node.get("componentProperties").and_then(|v| v.as_object()) {
            let simple = component::simplify_component_properties(props);
            if !simple.is_empty() {
                result.insert("componentProperties".to_string(), Value::Object(simple));
            }
        }
    }
    if let Some(refs) = node
        .get("componentPropertyReferences")
        .and_then(|v| v.as_object())
    {
        let simple = component::simplify_property_references(refs);
        if !simple.is_empty() {
            result.insert(
                "componentPropertyReferences".to_string(),
                Value::Object(simple),
            );
        }
    }
    if (node_type == "COMPONENT" || node_type == "COMPONENT_SET")
        && let Some(defs) = node
            .get("componentPropertyDefinitions")
            .and_then(|v| v.as_object())
    {
        let simple = component::simplify_property_definitions(defs);
        if !simple.is_empty()
            && let Some(id) = node.get("id").and_then(|v| v.as_str())
        {
            ctx.prop_defs.insert(id.to_string(), Value::Object(simple));
        }
    }
}

fn is_visible(v: &Value) -> bool {
    v.get("visible").and_then(|b| b.as_bool()).unwrap_or(true)
}

fn is_node_visible(node: &Value, inside_component_def: bool) -> bool {
    if is_visible(node) {
        return true;
    }
    // Rescue hidden nodes driven by a boolean prop inside component defs.
    if inside_component_def
        && let Some(refs) = node
            .get("componentPropertyReferences")
            .and_then(|v| v.as_object())
        && refs.contains_key("visible")
    {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Walker (mirror node-walker.ts)
// ---------------------------------------------------------------------------

fn process_node(
    node: &Value,
    ctx: &mut Ctx,
    max_depth: Option<u32>,
    after_children: bool,
) -> Option<Map<String, Value>> {
    if !is_node_visible(node, ctx.inside_component_def) {
        return None;
    }
    *ctx.counter += 1;

    let id = node
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let name = node
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let raw_type = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let node_type = if raw_type == "VECTOR" {
        "IMAGE-SVG"
    } else {
        raw_type
    };

    let mut result = Map::new();
    result.insert("id".to_string(), Value::String(id));
    result.insert("name".to_string(), Value::String(name));
    result.insert("type".to_string(), Value::String(node_type.to_string()));

    layout_extractor(node, &mut result, ctx);
    text_extractor(node, &mut result, ctx);
    visuals_extractor(node, &mut result, ctx);
    component_extractor(node, &mut result, ctx);

    // Children.
    let at_limit = max_depth.map(|m| ctx.depth >= m).unwrap_or(false);
    if !at_limit
        && let Some(children) = node.get("children").and_then(|c| c.as_array())
        && !children.is_empty()
    {
        let node_type_s = node_type.to_string();
        let inside = match node_type_s.as_str() {
            "COMPONENT" | "COMPONENT_SET" => true,
            "INSTANCE" => false,
            _ => ctx.inside_component_def,
        };
        // Grid containers emit children in anchor order (CSS auto-placement).
        let order: Vec<usize> =
            layout::compute_grid_child_order(node).unwrap_or_else(|| (0..children.len()).collect());
        // Temporarily swap parent/depth/inside flags via child ctx borrow.
        let (styles, style_cache, inline_cache, extra, named, ts, defs, counter) = (
            &mut *ctx.styles as *mut Map<String, Value>,
            &mut *ctx.style_cache as *mut HashMap<String, String>,
            &mut *ctx.inline_cache as *mut HashMap<String, String>,
            ctx.extra_styles as *const Map<String, Value>,
            &mut *ctx.named_style_keys as *mut HashSet<String>,
            &mut *ctx.ts_counter as *mut u64,
            &mut *ctx.prop_defs as *mut Map<String, Value>,
            &mut *ctx.counter as *mut u64,
        );
        let mut kids: Vec<Value> = Vec::new();
        for idx in order {
            let Some(child) = children.get(idx) else {
                continue;
            };
            // SAFETY: child processing is synchronous; reborrows end before return.
            let mut child_ctx = unsafe {
                Ctx {
                    styles: &mut *styles,
                    style_cache: &mut *style_cache,
                    inline_cache: &mut *inline_cache,
                    extra_styles: &*extra,
                    named_style_keys: &mut *named,
                    ts_counter: &mut *ts,
                    prop_defs: &mut *defs,
                    parent: Some(node.clone()),
                    inside_component_def: inside,
                    depth: ctx.depth + 1,
                    counter: &mut *counter,
                }
            };
            if !is_node_visible(child, child_ctx.inside_component_def) {
                continue;
            }
            if let Some(c) = process_node(child, &mut child_ctx, max_depth, after_children) {
                kids.push(Value::Object(c));
            }
        }
        if !kids.is_empty() {
            let mut result_v = Value::Object(result);
            let mut kids_v: Vec<Value> = kids;
            if after_children {
                kids_v = collapse_svg_containers(node, &mut result_v, kids_v);
            }
            result = result_v.as_object().unwrap().clone();
            if !kids_v.is_empty() {
                result.insert("children".to_string(), Value::Array(kids_v));
            }
        }
    } else if after_children {
        // Leaf containers still get the collapse check (children == [] case
        // never collapses since `every` on empty is true but count check...).
        let mut result_v = Value::Object(result);
        let out = collapse_svg_containers(node, &mut result_v, vec![]);
        result = result_v.as_object().unwrap().clone();
        if !out.is_empty() {
            result.insert("children".to_string(), Value::Array(out));
        }
    }

    Some(result)
}

fn has_image_fill(node: &Value) -> bool {
    node.get("fills")
        .and_then(|f| f.as_array())
        .map(|a| {
            a.iter()
                .any(|p| p.get("type").and_then(|t| t.as_str()) == Some("IMAGE"))
        })
        .unwrap_or(false)
}

/// Collapse vector-only containers into a single IMAGE-SVG (mirrors upstream).
fn collapse_svg_containers(node: &Value, result: &mut Value, children: Vec<Value>) -> Vec<Value> {
    const COLLAPSIBLE: [&str; 4] = ["FRAME", "GROUP", "INSTANCE", "BOOLEAN_OPERATION"];
    let t = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if !COLLAPSIBLE.contains(&t) {
        return children;
    }
    if children.is_empty() {
        return children;
    }
    let eligible = [
        "IMAGE-SVG",
        "BOOLEAN_OPERATION",
        "STAR",
        "LINE",
        "ELLIPSE",
        "REGULAR_POLYGON",
        "RECTANGLE",
    ];
    let all_eligible = children.iter().all(|c| {
        c.get("type")
            .and_then(|v| v.as_str())
            .map(|ct| eligible.contains(&ct))
            .unwrap_or(false)
    });
    if !all_eligible {
        return children;
    }
    if has_image_fill(node) {
        return children;
    }
    if let Some(kids) = node.get("children").and_then(|c| c.as_array())
        && kids.iter().any(has_image_fill)
    {
        return children;
    }
    // Auto-layout carve-out: preserve authored structure under 10 children.
    let auto = node
        .get("layoutMode")
        .and_then(|m| m.as_str())
        .map(|m| m == "HORIZONTAL" || m == "VERTICAL" || m == "GRID")
        .unwrap_or(false);
    if auto && children.len() < 10 {
        return children;
    }
    if let Some(obj) = result.as_object_mut() {
        obj.insert("type".to_string(), Value::String("IMAGE-SVG".to_string()));
    }
    vec![]
}

// ---------------------------------------------------------------------------
// Design assembly + finalize (mirrors design-extractor.ts + finalize.ts)
// ---------------------------------------------------------------------------

type ParsedResponse = (
    String,
    Vec<Value>,
    Map<String, Value>,
    Map<String, Value>,
    Map<String, Value>,
);

fn parse_api_response(body: &Value) -> Result<ParsedResponse> {
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled")
        .to_string();
    let mut components = Map::new();
    let mut component_sets = Map::new();
    let mut extra_styles = Map::new();

    if let Some(nodes) = body.get("nodes").and_then(|n| n.as_object()) {
        let (node_id, node_data) = nodes.iter().next().context("empty nodes response")?;
        if node_data.is_null() {
            anyhow::bail!(
                "node {node_id} was not found in the Figma file. Only /design/ and /file/ URLs are supported; branches need their own fileKey."
            );
        }
        if let Some(c) = node_data.get("components").and_then(|v| v.as_object()) {
            components.extend(c.clone());
        }
        if let Some(c) = node_data.get("componentSets").and_then(|v| v.as_object()) {
            component_sets.extend(c.clone());
        }
        if let Some(s) = node_data.get("styles").and_then(|v| v.as_object()) {
            extra_styles.extend(s.clone());
        }
        let doc = node_data
            .get("document")
            .cloned()
            .context("nodes response missing document")?;
        Ok((name, vec![doc], components, component_sets, extra_styles))
    } else {
        if let Some(c) = body.get("components").and_then(|v| v.as_object()) {
            components.extend(c.clone());
        }
        if let Some(c) = body.get("componentSets").and_then(|v| v.as_object()) {
            component_sets.extend(c.clone());
        }
        if let Some(s) = body.get("styles").and_then(|v| v.as_object()) {
            extra_styles.extend(s.clone());
        }
        let kids = body
            .pointer("/document/children")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok((name, kids, components, component_sets, extra_styles))
    }
}

const STYLE_REF_FIELDS: [&str; 5] = ["layout", "fills", "strokes", "effects", "textStyle"];

fn is_inline_ts_key(k: &str) -> bool {
    k.len() > 2 && k.starts_with("ts") && k[2..].chars().all(|c| c.is_ascii_digit())
}

fn count_style_refs(nodes: &[Value]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    fn walk(ns: &[Value], counts: &mut HashMap<String, usize>) {
        for n in ns {
            if let Some(o) = n.as_object() {
                for f in STYLE_REF_FIELDS {
                    if let Some(Value::String(id)) = o.get(f) {
                        *counts.entry(id.clone()).or_insert(0) += 1;
                    }
                }
                if let Some(kids) = o.get("children").and_then(|c| c.as_array()) {
                    walk(kids, counts);
                }
            }
        }
    }
    walk(nodes, &mut counts);
    counts
}

fn inline_single_use_styles(
    nodes: &mut [Value],
    styles: &mut Map<String, Value>,
    named: &HashSet<String>,
    counts: &HashMap<String, usize>,
) {
    let mut inline_keys = HashSet::new();
    let mut drop_keys = HashSet::new();
    for key in styles.keys() {
        if is_inline_ts_key(key) {
            continue;
        }
        if named.contains(key) {
            if counts.get(key).copied().unwrap_or(0) == 0 {
                drop_keys.insert(key.clone());
            }
            continue;
        }
        if counts.get(key).copied().unwrap_or(0) >= 2 {
            continue;
        }
        inline_keys.insert(key.clone());
    }
    fn walk(ns: &mut [Value], styles: &Map<String, Value>, inline_keys: &HashSet<String>) {
        for n in ns.iter_mut() {
            if let Some(o) = n.as_object_mut() {
                for f in STYLE_REF_FIELDS {
                    if let Some(Value::String(id)) = o.get(f).cloned()
                        && inline_keys.contains(&id)
                        && let Some(v) = styles.get(&id)
                    {
                        o.insert(f.to_string(), v.clone());
                    }
                }
                if let Some(kids) = o.get_mut("children").and_then(|c| c.as_array_mut()) {
                    walk(kids, styles, inline_keys);
                }
            }
        }
    }
    walk(nodes, styles, &inline_keys);
    styles.retain(|k, _| !inline_keys.contains(k) && !drop_keys.contains(k));
}

fn body_of(node: &Map<String, Value>) -> Map<String, Value> {
    node.iter()
        .filter(|(k, _)| *k != "id" && *k != "name" && *k != "children")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

type BodyEntry = (Map<String, Value>, String, usize);
/// Insertion-ordered (walk order) dedup table. A plain HashMap would make
/// element output order nondeterministic across processes (RandomState).
type BodyTable = indexmap::IndexMap<String, BodyEntry>;

fn deduplicate_elements(nodes: &mut [Value]) -> (Map<String, Value>, HashMap<String, usize>) {
    // Collect body hashes.
    let mut bodies: BodyTable = BodyTable::new();
    fn collect(ns: &[Value], bodies: &mut BodyTable) {
        for n in ns {
            if let Some(o) = n.as_object() {
                let body = body_of(o);
                if body.len() > 1 {
                    let s = stable_stringify(&Value::Object(body.clone()));
                    let id = element_id(&s, bodies);
                    bodies
                        .entry(id)
                        .and_modify(|e| e.2 += 1)
                        .or_insert((body, s, 1));
                }
                if let Some(kids) = o.get("children").and_then(|c| c.as_array()) {
                    collect(kids, bodies);
                }
            }
        }
    }
    collect(nodes, &mut bodies);

    let mut elements = Map::new();
    let mut counts = HashMap::new();
    for (id, (body, _, count)) in &bodies {
        if *count >= 2 {
            elements.insert(id.clone(), Value::Object(body.clone()));
            counts.insert(id.clone(), *count);
        }
    }
    // Rebuild with template refs. Hashes are recomputed on this second walk
    // (pointer-keyed maps would be fragile across the in-place mutation).
    apply_template_refs(nodes, &elements);
    (elements, counts)
}

/// Depth-first (children before parents): replace bodies that repeat 2+ times
/// with compact `{id, name, template, children?}` references.
fn apply_template_refs(ns: &mut [Value], elements: &Map<String, Value>) {
    for n in ns.iter_mut() {
        if let Some(o) = n.as_object_mut()
            && let Some(kids) = o.get_mut("children").and_then(|c| c.as_array_mut())
        {
            apply_template_refs(kids, elements);
        }
    }
    for n in ns.iter_mut() {
        if let Some(o) = n.as_object_mut() {
            let body = body_of(o);
            if body.len() > 1
                && let Some(id) = match_element(&stable_stringify(&Value::Object(body)), elements)
            {
                let mut reference = Map::new();
                if let Some(v) = o.get("id") {
                    reference.insert("id".to_string(), v.clone());
                }
                if let Some(v) = o.get("name") {
                    reference.insert("name".to_string(), v.clone());
                }
                reference.insert("template".to_string(), Value::String(id));
                if let Some(kids) = o.get("children").cloned() {
                    reference.insert("children".to_string(), kids);
                }
                *o = reference;
            }
        }
    }
}

/// Find the deduplicated element id for a canonical body string, honoring the
/// truncated-hash collision guard (ids lengthen on clash).
fn match_element(canonical_body: &str, elements: &Map<String, Value>) -> Option<String> {
    let full = sha1_hex(canonical_body);
    let mut len = 8usize;
    while len <= full.len() {
        let id = format!("EL-{}", &full[..len.min(full.len())]);
        if elements.contains_key(&id) {
            return Some(id);
        }
        len += 4;
    }
    None
}

fn element_id(s: &str, bodies: &BodyTable) -> String {
    let full = sha1_hex(s);
    let mut len = 8usize;
    while len < full.len() {
        let id = format!("EL-{}", &full[..len]);
        match bodies.get(&id) {
            None => return id,
            Some((_, existing, _)) if existing == s => return id,
            _ => len += 4,
        }
    }
    format!("EL-{full}")
}

fn inline_exclusive_styles(
    elements: &mut Map<String, Value>,
    instance_counts: &HashMap<String, usize>,
    styles: &mut Map<String, Value>,
    counts: &HashMap<String, usize>,
    named: &HashSet<String>,
) {
    for (hash, body) in elements.iter_mut() {
        let Some(instance_count) = instance_counts.get(hash) else {
            continue;
        };
        let Some(obj) = body.as_object_mut() else {
            continue;
        };
        for f in STYLE_REF_FIELDS {
            if let Some(Value::String(r)) = obj.get(f).cloned() {
                if named.contains(&r) || is_inline_ts_key(&r) {
                    continue;
                }
                if !styles.contains_key(&r) {
                    continue;
                }
                if counts.get(&r).copied() == Some(*instance_count)
                    && let Some(v) = styles.get(&r).cloned()
                {
                    obj.insert(f.to_string(), v);
                    // shift_remove, not remove: Map::remove is swap-remove and
                    // would scramble the surviving styles' first-use order.
                    styles.shift_remove(&r);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point (mirrors services/get-figma-data.ts)
// ---------------------------------------------------------------------------

/// Fetch raw Figma JSON, simplify it, and serialize to `format`.
pub async fn get_figma_data(
    client: &FigmaClient,
    file_key: &str,
    node_id: Option<&str>,
    depth: Option<u32>,
    format: OutputFormat,
) -> Result<String> {
    let body = match node_id {
        Some(n) => client.get_raw_node(file_key, n, depth).await?,
        None => client.get_raw_file(file_key, depth).await?,
    };
    let design = simplify_raw(&body, depth)?;
    Ok(crate::serialize::serialize_design(&design, format))
}

/// Simplify an already-fetched raw Figma response (file or nodes).
pub fn simplify_raw(body: &Value, max_depth: Option<u32>) -> Result<SimplifiedDesign> {
    let (name, raw_nodes, components, component_sets, extra_styles) = parse_api_response(body)?;

    let mut styles: Map<String, Value> = Map::new();
    let mut style_cache: HashMap<String, String> = HashMap::new();
    let mut inline_cache: HashMap<String, String> = HashMap::new();
    let mut named: HashSet<String> = HashSet::new();
    let mut ts_counter: u64 = 0;
    let mut prop_defs: Map<String, Value> = Map::new();
    let mut counter: u64 = 0;

    let mut nodes: Vec<Value> = Vec::new();
    for node in &raw_nodes {
        let mut ctx = Ctx {
            styles: &mut styles,
            style_cache: &mut style_cache,
            inline_cache: &mut inline_cache,
            extra_styles: &extra_styles,
            named_style_keys: &mut named,
            ts_counter: &mut ts_counter,
            prop_defs: &mut prop_defs,
            parent: None,
            inside_component_def: false,
            depth: 0,
            counter: &mut counter,
        };
        if let Some(n) = process_node(node, &mut ctx, max_depth, true) {
            nodes.push(Value::Object(n));
        }
    }

    // Finalize: count-gated style hoisting + element dedup.
    let counts = count_style_refs(&nodes);
    inline_single_use_styles(&mut nodes, &mut styles, &named, &counts);
    let (mut elements, instance_counts) = deduplicate_elements(&mut nodes);
    inline_exclusive_styles(
        &mut elements,
        &instance_counts,
        &mut styles,
        &counts,
        &named,
    );

    Ok(SimplifiedDesign {
        name,
        nodes,
        components: component::simplify_components(&components, &prop_defs),
        component_sets: component::simplify_component_sets(&component_sets, &prop_defs),
        global_vars: styles,
        elements,
    })
}

#[derive(Debug, Clone)]
pub struct SimplifiedDesign {
    pub name: String,
    pub nodes: Vec<Value>,
    pub components: Map<String, Value>,
    pub component_sets: Map<String, Value>,
    pub global_vars: Map<String, Value>,
    pub elements: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_file() -> Value {
        serde_json::json!({
            "name": "Test File",
            "document": {
                "children": [
                    {
                        "id": "1:1",
                        "name": "Page 1",
                        "type": "CANVAS",
                        "children": [
                            {
                                "id": "1:2",
                                "name": "Hero",
                                "type": "FRAME",
                                "visible": true,
                                "clipsContent": true,
                                "layoutMode": "VERTICAL",
                                "primaryAxisAlignItems": "MIN",
                                "counterAxisAlignItems": "CENTER",
                                "paddingTop": 16.0,
                                "paddingRight": 16.0,
                                "paddingBottom": 16.0,
                                "paddingLeft": 16.0,
                                "itemSpacing": 8.0,
                                "layoutSizingHorizontal": "FIXED",
                                "layoutSizingVertical": "HUG",
                                "absoluteBoundingBox": {"x": 0.0, "y": 0.0, "width": 800.0, "height": 600.0},
                                "fills": [
                                    {"type": "SOLID", "visible": true, "color": {"r": 1.0, "g": 1.0, "b": 1.0, "a": 1.0}}
                                ],
                                "children": [
                                    {
                                        "id": "1:3",
                                        "name": "Hello world",
                                        "type": "TEXT",
                                        "visible": true,
                                        "characters": "Hello world",
                                        "style": {
                                            "fontFamily": "Inter",
                                            "fontWeight": 700,
                                            "fontSize": 24.0,
                                            "lineHeightPx": 32.0,
                                            "lineHeightUnit": "PIXELS",
                                            "textAlignHorizontal": "LEFT"
                                        },
                                        "fills": [
                                            {"type": "SOLID", "visible": true, "color": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}
                                        ],
                                        "absoluteBoundingBox": {"x": 16.0, "y": 16.0, "width": 200.0, "height": 32.0},
                                        "layoutSizingHorizontal": "FIXED",
                                        "layoutSizingVertical": "FIXED",
                                        "layoutAlign": "STRETCH"
                                    },
                                    {
                                        "id": "1:4",
                                        "name": "Rectangle 12",
                                        "type": "RECTANGLE",
                                        "visible": true,
                                        "fills": [
                                            {"type": "SOLID", "visible": true, "color": {"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0}, "opacity": 0.5}
                                        ],
                                        "cornerRadius": 8.0,
                                        "absoluteBoundingBox": {"x": 16.0, "y": 56.0, "width": 100.0, "height": 50.0},
                                        "layoutSizingHorizontal": "FIXED",
                                        "layoutSizingVertical": "FIXED"
                                    }
                                ]
                            }
                        ]
                    }
                ]
            },
            "components": {},
            "componentSets": {},
            "styles": {}
        })
    }

    #[test]
    fn pipeline_produces_tree_yaml_json() {
        let design = simplify_raw(&fixture_file(), None).unwrap();
        assert_eq!(design.name, "Test File");
        assert_eq!(design.nodes.len(), 1);

        let tree = crate::serialize::serialize_design(&design, OutputFormat::Tree);
        assert!(tree.contains("NAME: \"Test File\""), "tree:\n{tree}");
        assert!(tree.contains("NODES:"), "tree:\n{tree}");
        assert!(tree.contains("Hello world"), "tree:\n{tree}");
        // Auto-generated "Rectangle 12" name must be stripped.
        assert!(!tree.contains("Rectangle 12"), "tree:\n{tree}");
        // TEXT layer names are noise and must be stripped.
        assert!(!tree.contains("\"Hello world\" #"), "tree:\n{tree}");
        // Semi-transparent red fill → rgba string.
        assert!(tree.contains("rgba(255, 0, 0, 0.5)"), "tree:\n{tree}");

        let json = crate::serialize::serialize_design(&design, OutputFormat::Json);
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.pointer("/metadata/name").and_then(|v| v.as_str()),
            Some("Test File")
        );

        let yaml = crate::serialize::serialize_design(&design, OutputFormat::Yaml);
        let parsed_y: Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            parsed_y.pointer("/metadata/name").and_then(|v| v.as_str()),
            Some("Test File")
        );
    }

    #[test]
    fn identical_siblings_dedupe_into_elements() {
        let mut file = fixture_file();
        // Duplicate the rectangle node so two identical bodies exist.
        let rect = file
            .pointer("/document/children/0/children/0/children/1")
            .unwrap()
            .clone();
        let mut rect2 = rect.clone();
        rect2["id"] = Value::String("1:5".to_string());
        file.pointer_mut("/document/children/0/children/0/children")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(rect2);
        let design = simplify_raw(&file, None).unwrap();
        assert!(!design.elements.is_empty(), "expected element dedup");
        let tree = crate::serialize::serialize_design(&design, OutputFormat::Tree);
        assert!(tree.contains("template=EL-"), "tree:\n{tree}");
    }

    #[test]
    fn depth_limits_traversal() {
        let design = simplify_raw(&fixture_file(), Some(1)).unwrap();
        let tree = crate::serialize::serialize_design(&design, OutputFormat::Tree);
        // Depth 1: canvas + frame, but no text/rect leaves.
        assert!(!tree.contains("Hello world"), "tree:\n{tree}");
    }
}
