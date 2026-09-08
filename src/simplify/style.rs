//! Style transformer — port of `transformers/style*.ts`:
//! colors, gradients, image/pattern paints, stroke assembly.

use serde_json::{Map, Value};

use super::pixel_round;

fn rgba_of(color: &Value) -> (f64, f64, f64, f64) {
    (
        color.get("r").and_then(|v| v.as_f64()).unwrap_or(0.0),
        color.get("g").and_then(|v| v.as_f64()).unwrap_or(0.0),
        color.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0),
        color.get("a").and_then(|v| v.as_f64()).unwrap_or(1.0),
    )
}

/// Convert Figma RGBA + paint opacity → (hex, effective alpha).
pub fn convert_color(color: &Value, opacity: f64) -> (String, f64) {
    let (r, g, b, a) = rgba_of(color);
    let ri = (r * 255.0).round() as u8;
    let gi = (g * 255.0).round() as u8;
    let bi = (b * 255.0).round() as u8;
    let alpha = ((opacity * a * 100.0).round()) / 100.0;
    (format!("#{:02X}{:02X}{:02X}", ri, gi, bi), alpha)
}

pub fn format_rgba(color: &Value, opacity: f64) -> String {
    let (r, g, b, a) = rgba_of(color);
    let ri = (r * 255.0).round() as i64;
    let gi = (g * 255.0).round() as i64;
    let bi = (b * 255.0).round() as i64;
    let alpha = ((opacity * a * 100.0).round()) / 100.0;
    format!("rgba({ri}, {gi}, {bi}, {})", super::js_num(alpha))
}

fn is_flattenable(paint: &Value) -> bool {
    if paint.get("type").and_then(|v| v.as_str()) != Some("SOLID") {
        return false;
    }
    matches!(
        paint.get("blendMode").and_then(|v| v.as_str()),
        None | Some("NORMAL") | Some("PASS_THROUGH")
    )
}

/// Collapse an all-solid stack into the single visible color (bottom→top
/// source-over). None when gradients/images/patterns or exotic blends appear.
pub fn flatten_solid_fills(paints: &[Value]) -> Option<String> {
    if paints.is_empty() || !paints.iter().all(is_flattenable) {
        return None;
    }
    let to_straight = |p: &Value| -> (f64, f64, f64, f64) {
        let c = p.get("color").cloned().unwrap_or(Value::Null);
        let (r, g, b, a) = rgba_of(&c);
        let op = p.get("opacity").and_then(|v| v.as_f64()).unwrap_or(1.0);
        (r, g, b, a * op)
    };
    let over = |top: (f64, f64, f64, f64), bot: (f64, f64, f64, f64)| -> (f64, f64, f64, f64) {
        let (tr, tg, tb, ta) = top;
        let (br, bg, bb, ba) = bot;
        let a = ta + ba * (1.0 - ta);
        if a == 0.0 {
            return (0.0, 0.0, 0.0, 0.0);
        }
        let blend = |ct: f64, cb: f64| (ct * ta + cb * ba * (1.0 - ta)) / a;
        (blend(tr, br), blend(tg, bg), blend(tb, bb), a)
    };
    let mut acc = to_straight(&paints[0]);
    for p in &paints[1..] {
        acc = over(to_straight(p), acc);
    }
    let color = serde_json::json!({"r": acc.0, "g": acc.1, "b": acc.2, "a": acc.3});
    let (hex, opacity) = convert_color(&color, 1.0);
    if opacity == 1.0 {
        Some(hex)
    } else {
        Some(format_rgba(&color, 1.0))
    }
}

// ---------------------------------------------------------------------------
// Image fills
// ---------------------------------------------------------------------------

fn transform_hash(t: &Value) -> String {
    // Exact port of upstream `generateTransformHash`: accumulate a signed
    // 32-bit checksum over the JS-string of each matrix number in row-major
    // order, then take 6 hex chars of the absolute value. Hashing the JSON
    // serialization instead (brackets, commas, Rust float rendering) yields
    // different suffixes — and those suffixes feed back into style/element
    // content hashes, so any divergence cascades through the whole output.
    let mut acc: i32 = 0;
    if let Some(rows) = t.as_array() {
        for row in rows {
            if let Some(cols) = row.as_array() {
                for val in cols {
                    if let Some(n) = val.as_f64() {
                        for ch in super::js_num(n).chars() {
                            acc = acc.wrapping_mul(31).wrapping_add(ch as i32);
                        }
                    }
                }
            }
        }
    }
    format!("{:x}", acc.unsigned_abs())
        .chars()
        .take(6)
        .collect()
}

/// Recursively normalize whole-valued floats to ints (JS semantics) for
/// verbatim API payloads that reach simplified output (cropTransform, flags).
pub fn normalize_numbers(v: &Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().map(super::num_raw).unwrap_or_else(|| v.clone()),
        Value::Array(a) => Value::Array(a.iter().map(normalize_numbers).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, val)| (k.clone(), normalize_numbers(val)))
                .collect(),
        ),
        _ => v.clone(),
    }
}

fn translate_scale_mode(
    scale_mode: &str,
    has_children: bool,
    scaling_factor: Option<f64>,
) -> (Map<String, Value>, Map<String, Value>) {
    let mut css = Map::new();
    let mut proc = Map::new();
    proc.insert("needsCropping".to_string(), Value::Bool(false));
    proc.insert("requiresImageDimensions".to_string(), Value::Bool(false));
    match scale_mode {
        "FILL" => {
            if has_children {
                css.insert(
                    "backgroundSize".to_string(),
                    Value::String("cover".to_string()),
                );
                css.insert(
                    "backgroundRepeat".to_string(),
                    Value::String("no-repeat".to_string()),
                );
                css.insert("isBackground".to_string(), Value::Bool(true));
            } else {
                css.insert("objectFit".to_string(), Value::String("cover".to_string()));
                css.insert("isBackground".to_string(), Value::Bool(false));
            }
        }
        "FIT" => {
            if has_children {
                css.insert(
                    "backgroundSize".to_string(),
                    Value::String("contain".to_string()),
                );
                css.insert(
                    "backgroundRepeat".to_string(),
                    Value::String("no-repeat".to_string()),
                );
                css.insert("isBackground".to_string(), Value::Bool(true));
            } else {
                css.insert(
                    "objectFit".to_string(),
                    Value::String("contain".to_string()),
                );
                css.insert("isBackground".to_string(), Value::Bool(false));
            }
        }
        "TILE" => {
            css.insert(
                "backgroundRepeat".to_string(),
                Value::String("repeat".to_string()),
            );
            css.insert(
                "backgroundSize".to_string(),
                Value::String(match scaling_factor {
                    Some(f) => format!(
                        "calc(var(--original-width) * {}) calc(var(--original-height) * {})",
                        super::js_num(f),
                        super::js_num(f)
                    ),
                    None => "auto".to_string(),
                }),
            );
            css.insert("isBackground".to_string(), Value::Bool(true));
            proc.insert("requiresImageDimensions".to_string(), Value::Bool(true));
        }
        // Figma calls crop "STRETCH" in the API.
        _ => {
            if has_children {
                css.insert(
                    "backgroundSize".to_string(),
                    Value::String("100% 100%".to_string()),
                );
                css.insert(
                    "backgroundRepeat".to_string(),
                    Value::String("no-repeat".to_string()),
                );
                css.insert("isBackground".to_string(), Value::Bool(true));
            } else {
                css.insert("objectFit".to_string(), Value::String("fill".to_string()));
                css.insert("isBackground".to_string(), Value::Bool(false));
            }
        }
    }
    (css, proc)
}

/// Convert one Figma paint to a simplified fill (string color or object).
pub fn parse_paint(raw: &Value, has_children: bool) -> Value {
    match raw.get("type").and_then(|v| v.as_str()) {
        Some("IMAGE") => {
            let mut obj = Map::new();
            obj.insert("type".to_string(), Value::String("IMAGE".to_string()));
            if let Some(r) = raw.get("imageRef").and_then(|v| v.as_str()) {
                obj.insert("imageRef".to_string(), Value::String(r.to_string()));
            }
            if let Some(r) = raw.get("gifRef").and_then(|v| v.as_str()) {
                obj.insert("gifRef".to_string(), Value::String(r.to_string()));
            }
            let scale_mode = raw
                .get("scaleMode")
                .and_then(|v| v.as_str())
                .unwrap_or("FILL");
            obj.insert(
                "scaleMode".to_string(),
                Value::String(scale_mode.to_string()),
            );
            if let Some(f) = raw.get("scalingFactor").and_then(|v| v.as_f64()) {
                // Whole values render as integers (JS semantics).
                obj.insert("scalingFactor".to_string(), super::num_raw(f));
            }
            let (css, mut proc) = translate_scale_mode(
                scale_mode,
                has_children,
                raw.get("scalingFactor").and_then(|v| v.as_f64()),
            );
            for (k, v) in css {
                obj.insert(k, v);
            }
            if let Some(t) = raw.get("imageTransform") {
                proc.insert("needsCropping".to_string(), Value::Bool(true));
                // Normalize whole numbers to ints (JS semantics) so the
                // transform — which feeds content hashes — matches upstream.
                proc.insert("cropTransform".to_string(), normalize_numbers(t));
                proc.insert(
                    "filenameSuffix".to_string(),
                    Value::String(transform_hash(t)),
                );
                if proc.get("requiresImageDimensions").is_none() {
                    proc.insert("requiresImageDimensions".to_string(), Value::Bool(false));
                }
            }
            obj.insert("imageDownloadArguments".to_string(), Value::Object(proc));
            Value::Object(obj)
        }
        Some("PATTERN") => parse_pattern_paint(raw),
        Some(t) if t.starts_with("GRADIENT") => {
            let mut obj = Map::new();
            obj.insert("type".to_string(), Value::String(t.to_string()));
            obj.insert(
                "gradient".to_string(),
                Value::String(convert_gradient_to_css(raw)),
            );
            Value::Object(obj)
        }
        _ => {
            let color = raw.get("color").cloned().unwrap_or(Value::Null);
            let opacity = raw.get("opacity").and_then(|v| v.as_f64()).unwrap_or(1.0);
            let (hex, a) = convert_color(&color, opacity);
            if a == 1.0 {
                Value::String(hex)
            } else {
                Value::String(format_rgba(&color, opacity))
            }
        }
    }
}

fn parse_pattern_paint(raw: &Value) -> Value {
    let h = match raw.get("horizontalAlignment").and_then(|v| v.as_str()) {
        Some("CENTER") => "center",
        Some("END") => "right",
        _ => "left",
    };
    let v = match raw.get("verticalAlignment").and_then(|v| v.as_str()) {
        Some("CENTER") => "center",
        Some("END") => "bottom",
        _ => "top",
    };
    let scale = raw
        .get("scalingFactor")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    serde_json::json!({
        "type": "PATTERN",
        "patternSource": {
            "type": "IMAGE-PNG",
            "nodeId": raw.get("sourceNodeId").and_then(|v| v.as_str()).unwrap_or(""),
        },
        "backgroundRepeat": "repeat",
        "backgroundSize": format!("{}%", super::js_num((scale * 100.0).round())),
        "backgroundPosition": format!("{h} {v}"),
    })
}

// ---------------------------------------------------------------------------
// Gradients → CSS (port of style/gradient.ts)
// ---------------------------------------------------------------------------

fn format_stops(stops: &[Value], paint_opacity: f64) -> String {
    stops
        .iter()
        .map(|s| {
            let pos = s.get("position").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let color = s.get("color").cloned().unwrap_or(Value::Null);
            format!(
                "{} {}%",
                format_rgba(&color, paint_opacity),
                rn(pos * 100.0)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Rounded gradient-geometry number rendered JS-style (`-0` -> `0`).
fn rn(v: f64) -> String {
    super::js_num(v.round())
}

fn line_intersections(start: (f64, f64), end: (f64, f64)) -> Vec<f64> {
    let (sx, sy) = start;
    let (dx, dy) = (end.0 - sx, end.1 - sy);
    if dx.abs() < 1e-10 && dy.abs() < 1e-10 {
        return vec![];
    }
    let mut ts = vec![];
    if dy.abs() > 1e-10 {
        for edge_y in [0.0, 1.0] {
            let t = (edge_y - sy) / dy;
            let x = sx + t * dx;
            if (0.0..=1.0).contains(&x) {
                ts.push(t);
            }
        }
    }
    if dx.abs() > 1e-10 {
        for edge_x in [0.0, 1.0] {
            let t = (edge_x - sx) / dx;
            let y = sy + t * dy;
            if (0.0..=1.0).contains(&y) {
                ts.push(t);
            }
        }
    }
    let mut uniq: Vec<f64> = vec![];
    for t in ts {
        let r = (t * 1e6).round() / 1e6;
        if !uniq.contains(&r) {
            uniq.push(r);
        }
    }
    uniq.sort_by(|a, b| a.partial_cmp(b).unwrap());
    uniq
}

fn handles_of(paint: &Value) -> Option<Vec<(f64, f64)>> {
    let h = paint.get("gradientHandlePositions")?.as_array()?;
    let pts: Vec<(f64, f64)> = h
        .iter()
        .filter_map(|p| Some((p.get("x")?.as_f64()?, p.get("y")?.as_f64()?)))
        .collect();
    if pts.len() >= 2 { Some(pts) } else { None }
}

fn sorted_stops(paint: &Value) -> Vec<Value> {
    let mut stops: Vec<Value> = paint
        .get("gradientStops")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    stops.sort_by(|a, b| {
        a.get("position")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .partial_cmp(&b.get("position").and_then(|v| v.as_f64()).unwrap_or(0.0))
            .unwrap()
    });
    stops
}

pub fn convert_gradient_to_css(paint: &Value) -> String {
    let paint_opacity = paint.get("opacity").and_then(|v| v.as_f64()).unwrap_or(1.0);
    let stops = sorted_stops(paint);
    let gtype = paint
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("GRADIENT_LINEAR");
    let Some(handles) = handles_of(paint) else {
        let wrap = |g: &str, s: &str| match gtype {
            "GRADIENT_ANGULAR" => format!("conic-gradient({g}, {s})"),
            "GRADIENT_RADIAL" | "GRADIENT_DIAMOND" => format!("radial-gradient({g}, {s})"),
            _ => format!("linear-gradient({g}, {s})"),
        };
        return wrap("0deg", &format_stops(&stops, paint_opacity));
    };
    match gtype {
        "GRADIENT_RADIAL" => {
            let (cx, cy) = handles[0];
            format!(
                "radial-gradient(circle at {}% {}%, {})",
                rn(cx * 100.0),
                rn(cy * 100.0),
                format_stops(&stops, paint_opacity)
            )
        }
        "GRADIENT_ANGULAR" => {
            let (cx, cy) = handles[0];
            let (ax, ay) = handles[1];
            let angle = rn((ay - cy).atan2(ax - cx) * 180.0 / std::f64::consts::PI + 90.0);
            format!(
                "conic-gradient(from {angle}deg at {}% {}%, {})",
                rn(cx * 100.0),
                rn(cy * 100.0),
                format_stops(&stops, paint_opacity)
            )
        }
        "GRADIENT_DIAMOND" => {
            let (cx, cy) = handles[0];
            format!(
                "radial-gradient(ellipse at {}% {}%, {})",
                rn(cx * 100.0),
                rn(cy * 100.0),
                format_stops(&stops, paint_opacity)
            )
        }
        _ => {
            let (sx, sy) = handles[0];
            let (ex, ey) = handles[1];
            let (dx, dy) = (ex - sx, ey - sy);
            let len = (dx * dx + dy * dy).sqrt();
            if len == 0.0 {
                return format!(
                    "linear-gradient(0deg, {})",
                    format_stops(&stops, paint_opacity)
                );
            }
            let angle = rn(dy.atan2(dx) * 180.0 / std::f64::consts::PI + 90.0);
            let inter = line_intersections((sx, sy), (ex, ey));
            if inter.len() >= 2 {
                let (lo, hi) = (inter[0].min(inter[1]), inter[0].max(inter[1]));
                let mapped: Vec<String> = stops
                    .iter()
                    .map(|s| {
                        let pos = s.get("position").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let color = s.get("color").cloned().unwrap_or(Value::Null);
                        let ext = if hi != lo {
                            (pos - lo) / (hi - lo)
                        } else {
                            pos
                        };
                        let c = ext.clamp(0.0, 1.0);
                        format!("{} {}%", format_rgba(&color, paint_opacity), rn(c * 100.0))
                    })
                    .collect();
                format!("linear-gradient({angle}deg, {})", mapped.join(", "))
            } else {
                format!(
                    "linear-gradient({angle}deg, {})",
                    format_stops(&stops, paint_opacity)
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Strokes
// ---------------------------------------------------------------------------

pub struct SimplifiedStrokes {
    pub colors: Vec<Value>,
    pub stroke_weight: Option<String>,
    pub stroke_dashes: Option<Vec<Value>>,
    pub stroke_weights: Option<String>,
    pub stroke_align: Option<String>,
}

pub fn build_simplified_strokes(n: &Value, has_children: bool) -> SimplifiedStrokes {
    let mut colors = vec![];
    if let Some(strokes) = n.get("strokes").and_then(|v| v.as_array()) {
        let mut v: Vec<Value> = strokes
            .iter()
            .filter(|s| s.get("visible").and_then(|b| b.as_bool()).unwrap_or(true))
            .map(|s| parse_paint(s, has_children))
            .collect();
        v.reverse();
        colors = v;
    }
    let mut out = SimplifiedStrokes {
        colors,
        stroke_weight: None,
        stroke_dashes: None,
        stroke_weights: None,
        stroke_align: None,
    };
    if let Some(w) = n.get("strokeWeight").and_then(|v| v.as_f64())
        && w > 0.0
    {
        out.stroke_weight = Some(format!("{}px", super::js_num(w)));
    }
    if let Some(d) = n.get("strokeDashes").and_then(|v| v.as_array())
        && !d.is_empty()
    {
        // Preserve raw numbers with JS rendering (Figma may emit 10.0 for 10;
        // as_u64 would drop those). Whole values render as integers.
        let vals: Vec<Value> = d
            .iter()
            .filter_map(|v| v.as_f64())
            .map(super::num_raw)
            .collect();
        if !vals.is_empty() {
            out.stroke_dashes = Some(vals);
        }
    }
    if let Some(a) = n.get("strokeAlign").and_then(|v| v.as_str())
        && (a == "OUTSIDE" || a == "CENTER")
    {
        out.stroke_align = Some(a.to_string());
    }
    if let Some(w) = n.get("individualStrokeWeights").and_then(|v| v.as_object()) {
        let top = w.get("top").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let right = w.get("right").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let bottom = w.get("bottom").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let left = w.get("left").and_then(|v| v.as_f64()).unwrap_or(0.0);
        out.stroke_weight = css_shorthand(top, right, bottom, left);
    }
    out
}

fn css_shorthand(top: f64, right: f64, bottom: f64, left: f64) -> Option<String> {
    if top == 0.0 && right == 0.0 && bottom == 0.0 && left == 0.0 {
        return None;
    }
    let px = super::js_num;
    if top == right && right == bottom && bottom == left {
        return Some(format!("{}px", px(top)));
    }
    if right == left {
        if top == bottom {
            return Some(format!("{}px {}px", px(top), px(right)));
        }
        return Some(format!("{}px {}px {}px", px(top), px(right), px(bottom)));
    }
    Some(format!(
        "{}px {}px {}px {}px",
        px(top),
        px(right),
        px(bottom),
        px(left)
    ))
}

#[allow(unused)]
pub fn round2(n: f64) -> f64 {
    pixel_round(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_hash_matches_upstream() {
        // Suffix observed in upstream output for this matrix.
        let t = serde_json::json!([
            [1.0, 0.0, 0.0],
            [0.0, 0.9960304498672485, 0.0019847655203193426]
        ]);
        assert_eq!(transform_hash(&t), "2ed677");
    }

    #[test]
    fn js_numbers_render_like_js() {
        assert_eq!(super::super::js_num(1.0), "1");
        assert_eq!(super::super::js_num(-0.0), "0");
        assert_eq!(super::super::js_num(0.5), "0.5");
        assert_eq!(
            super::super::js_num(0.9960304498672485),
            "0.9960304498672485"
        );
    }
}
