//! Layout transformer — port of `transformers/layout*.ts`.
//! Converts Figma auto-layout / sizing / positioning fields into the compact
//! flex-like schema agents consume (mode/justifyContent/alignItems/...).

use serde_json::{Map, Value};

use super::pixel_round;

fn is_frame(n: &Value) -> bool {
    n.get("clipsContent").and_then(|v| v.as_bool()).is_some()
}

fn bbox(n: &Value) -> Option<(f64, f64, f64, f64)> {
    let b = n.get("absoluteBoundingBox")?;
    Some((
        b.get("x")?.as_f64()?,
        b.get("y")?.as_f64()?,
        b.get("width")?.as_f64()?,
        b.get("height")?.as_f64()?,
    ))
}

fn layout_mode_schema(mode: Option<&str>) -> &'static str {
    match mode {
        Some("HORIZONTAL") => "row",
        Some("VERTICAL") => "column",
        Some("GRID") => "grid",
        _ => "none",
    }
}

fn convert_sizing(s: Option<&str>) -> Option<&'static str> {
    match s {
        Some("FIXED") => Some("fixed"),
        Some("FILL") => Some("fill"),
        Some("HUG") => Some("hug"),
        _ => None,
    }
}

fn convert_self_align(a: Option<&str>) -> Option<&'static str> {
    match a {
        Some("MAX") => Some("flex-end"),
        Some("CENTER") => Some("center"),
        Some("STRETCH") => Some("stretch"),
        _ => None,
    }
}

fn convert_justify(a: Option<&str>) -> Option<&'static str> {
    match a {
        Some("MAX") => Some("flex-end"),
        Some("CENTER") => Some("center"),
        Some("SPACE_BETWEEN") => Some("space-between"),
        _ => None,
    }
}

fn parent_axis_row_col(parent: Option<&Value>) -> Option<&'static str> {
    let p = parent?;
    if !is_frame(p) {
        return None;
    }
    match p.get("layoutMode").and_then(|v| v.as_str()) {
        Some("HORIZONTAL") => Some("row"),
        Some("VERTICAL") => Some("column"),
        _ => None,
    }
}

fn is_in_auto_layout_flow(n: &Value, parent: Option<&Value>) -> bool {
    let Some(p) = parent else { return false };
    let auto = p
        .get("layoutMode")
        .and_then(|m| m.as_str())
        .map(|m| m == "HORIZONTAL" || m == "VERTICAL" || m == "GRID")
        .unwrap_or(false);
    if !auto || bbox(n).is_none() {
        return false;
    }
    n.get("layoutPositioning").and_then(|v| v.as_str()) != Some("ABSOLUTE")
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

fn gap_shorthand(row: Option<f64>, col: Option<f64>) -> Option<String> {
    match (row, col) {
        (None, None) => None,
        (Some(r), Some(c)) => {
            if r == 0.0 && c == 0.0 {
                return None;
            }
            if r == c {
                Some(format!("{}px", super::js_num(r)))
            } else {
                Some(format!("{}px {}px", super::js_num(r), super::js_num(c)))
            }
        }
        (Some(r), None) | (None, Some(r)) => {
            if r == 0.0 {
                // Single zero is the CSS default — omit (upstream convention).
                // As half of a two-value shorthand it would be kept, but a
                // lone axis gap of 0 carries no signal.
                let _ = c_is_none(col);
                None
            } else {
                Some(format!("{}px", super::js_num(r)))
            }
        }
    }
}

fn c_is_none(_: Option<f64>) -> bool {
    true
}

fn convert_align_items(n: &Value, mode: &str) -> Option<&'static str> {
    // Stretch shortcut: every in-flow child fills the cross axis.
    if let Some(kids) = n.get("children").and_then(|c| c.as_array())
        && !kids.is_empty()
    {
        let cross = if mode == "row" {
            "layoutSizingVertical"
        } else {
            "layoutSizingHorizontal"
        };
        let all_stretch = kids.iter().all(|c| {
            c.get("layoutPositioning").and_then(|v| v.as_str()) == Some("ABSOLUTE")
                || c.get(cross).and_then(|v| v.as_str()) == Some("FILL")
        });
        if all_stretch {
            return Some("stretch");
        }
    }
    match n.get("counterAxisAlignItems").and_then(|v| v.as_str()) {
        Some("MAX") => Some("flex-end"),
        Some("CENTER") => Some("center"),
        Some("BASELINE") => Some("baseline"),
        _ => None,
    }
}

fn build_flex_gap(n: &Value, mode: &str) -> Option<String> {
    let primary_gap =
        if n.get("primaryAxisAlignItems").and_then(|v| v.as_str()) == Some("SPACE_BETWEEN") {
            None
        } else {
            n.get("itemSpacing").and_then(|v| v.as_f64())
        };
    let counter_gap = if n.get("layoutWrap").and_then(|v| v.as_str()) != Some("WRAP")
        || n.get("counterAxisAlignContent").and_then(|v| v.as_str()) != Some("SPACE_BETWEEN")
    {
        // Upstream: counter gap suppressed unless WRAP without SPACE_BETWEEN... recheck:
        // `layoutWrap !== "WRAP" || counterAxisAlignContent === "SPACE_BETWEEN" ? undefined : counterAxisSpacing`
        if n.get("layoutWrap").and_then(|v| v.as_str()) != Some("WRAP")
            || n.get("counterAxisAlignContent").and_then(|v| v.as_str()) == Some("SPACE_BETWEEN")
        {
            None
        } else {
            n.get("counterAxisSpacing").and_then(|v| v.as_f64())
        }
    } else {
        n.get("counterAxisSpacing").and_then(|v| v.as_f64())
    };
    let (row_gap, col_gap) = if mode == "row" {
        (counter_gap, primary_gap)
    } else {
        (primary_gap, counter_gap)
    };
    gap_shorthand(row_gap, col_gap)
}

/// Public entry: frame-level values + per-node layout values merged.
pub fn build_simplified_layout(n: &Value, parent: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let mode = if is_frame(n) {
        layout_mode_schema(n.get("layoutMode").and_then(|v| v.as_str()))
    } else {
        "none"
    };

    if is_frame(n) {
        out.insert("mode".to_string(), Value::String(mode.to_string()));
        let mut overflow: Vec<Value> = vec![];
        if let Some(dir) = n.get("overflowDirection").and_then(|v| v.as_str()) {
            if dir.contains("HORIZONTAL") {
                overflow.push(Value::String("x".to_string()));
            }
            if dir.contains("VERTICAL") {
                overflow.push(Value::String("y".to_string()));
            }
        }
        if !overflow.is_empty() {
            out.insert("overflowScroll".to_string(), Value::Array(overflow));
        }
        if mode == "none" {
            // Still fall through to per-node values below? Upstream returns
            // early with just {mode:none} merged with layout values. Keep both.
        } else {
            if let Some(a) = convert_self_align(n.get("layoutAlign").and_then(|v| v.as_str())) {
                out.insert("alignSelf".to_string(), Value::String(a.to_string()));
            }
            let (pt, pr, pb, pl) = (
                n.get("paddingTop").and_then(|v| v.as_f64()).unwrap_or(0.0),
                n.get("paddingRight")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                n.get("paddingBottom")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                n.get("paddingLeft").and_then(|v| v.as_f64()).unwrap_or(0.0),
            );
            let has_padding = pt != 0.0 || pr != 0.0 || pb != 0.0 || pl != 0.0;
            if has_padding && let Some(sh) = css_shorthand(pt, pr, pb, pl) {
                out.insert("padding".to_string(), Value::String(sh));
            }
            if mode == "grid" {
                if let Some(cols) = n.get("gridColumnsSizing").and_then(|v| v.as_str()) {
                    let t = cols.trim();
                    if !t.is_empty() {
                        out.insert(
                            "gridTemplateColumns".to_string(),
                            Value::String(t.to_string()),
                        );
                    }
                }
                if let Some(rows) = n.get("gridRowsSizing").and_then(|v| v.as_str()) {
                    let t = rows.trim();
                    if !t.is_empty() {
                        out.insert("gridTemplateRows".to_string(), Value::String(t.to_string()));
                    }
                }
                if let Some(g) = gap_shorthand(
                    n.get("gridRowGap").and_then(|v| v.as_f64()),
                    n.get("gridColumnGap").and_then(|v| v.as_f64()),
                ) {
                    out.insert("gap".to_string(), Value::String(g));
                }
            } else {
                if let Some(j) =
                    convert_justify(n.get("primaryAxisAlignItems").and_then(|v| v.as_str()))
                {
                    out.insert("justifyContent".to_string(), Value::String(j.to_string()));
                }
                if let Some(a) = convert_align_items(n, mode) {
                    out.insert("alignItems".to_string(), Value::String(a.to_string()));
                }
                if n.get("layoutWrap").and_then(|v| v.as_str()) == Some("WRAP") {
                    out.insert("wrap".to_string(), Value::Bool(true));
                }
                if let Some(g) = build_flex_gap(n, mode) {
                    out.insert("gap".to_string(), Value::String(g));
                }
            }
        }
    } else {
        out.insert("mode".to_string(), Value::String("none".to_string()));
    }

    // Per-node values (need absoluteBoundingBox).
    if bbox(n).is_none() {
        return out;
    }
    let is_root = parent.is_none();

    let h = convert_sizing(n.get("layoutSizingHorizontal").and_then(|v| v.as_str()));
    let v = convert_sizing(n.get("layoutSizingVertical").and_then(|v| v.as_str()));
    let mut sizing = Map::new();
    if let Some(s) = h {
        sizing.insert("horizontal".to_string(), Value::String(s.to_string()));
    }
    if let Some(s) = v {
        sizing.insert("vertical".to_string(), Value::String(s.to_string()));
    }
    // Root FIXED → contextual + designed size reference.
    if is_root && let Some((_, _, w, hgt)) = bbox(n) {
        if sizing.get("horizontal").and_then(|v| v.as_str()) == Some("fixed") {
            sizing.insert(
                "horizontal".to_string(),
                Value::String("contextual".to_string()),
            );
            out.insert(
                "designedWidth".to_string(),
                Value::String(format!("{}px", super::js_num(pixel_round(w)))),
            );
        }
        if sizing.get("vertical").and_then(|v| v.as_str()) == Some("fixed") {
            sizing.insert(
                "vertical".to_string(),
                Value::String("contextual".to_string()),
            );
            out.insert(
                "designedHeight".to_string(),
                Value::String(format!("{}px", super::js_num(pixel_round(hgt)))),
            );
        }
    }
    // Upstream always sets `sizing` on layout nodes — even when neither axis
    // converts (emitted as `sizing: {}`). The empty object counts toward the
    // layout-extractor's key threshold, so it must not be skipped.
    out.insert("sizing".to_string(), Value::Object(sizing));

    if let Some(p) = parent {
        if (is_frame(p) || bbox(p).is_some()) && !is_in_auto_layout_flow(n, parent) {
            if n.get("layoutPositioning").and_then(|v| v.as_str()) == Some("ABSOLUTE") {
                out.insert(
                    "position".to_string(),
                    Value::String("absolute".to_string()),
                );
            }
            if let (Some((nx, ny, _, _)), Some((px, py, _, _))) = (bbox(n), bbox(p)) {
                let mut loc = Map::new();
                loc.insert("x".to_string(), super::num(nx - px));
                loc.insert("y".to_string(), super::num(ny - py));
                out.insert("locationRelativeToParent".to_string(), Value::Object(loc));
            }
        }
        // Grid-child positioning.
        let parent_is_grid = parent
            .and_then(|p| p.get("layoutMode"))
            .and_then(|m| m.as_str())
            == Some("GRID");
        if parent_is_grid && n.get("layoutPositioning").and_then(|v| v.as_str()) != Some("ABSOLUTE")
        {
            let packed = parent
                .and_then(|p| p.get("children"))
                .and_then(|c| c.as_array())
                .map(|kids| kids.to_vec())
                .map(|kids| is_packed_grid(&kids))
                .unwrap_or(true);
            for (k, val) in build_grid_child_positioning(n, parent.unwrap(), packed) {
                out.insert(k, val);
            }
        }
    }

    // Dimensions for non-root.
    if !is_root && let Some((_, _, w, hgt)) = bbox(n) {
        let axis = resolve_child_axis(n, parent, mode);
        let (sh, sv) = child_stretch(n, axis);
        let mut dims = Map::new();
        if !sh
            && should_emit(
                n.get("layoutSizingHorizontal").and_then(|v| v.as_str()),
                axis,
            )
        {
            dims.insert("width".to_string(), super::num(w));
        }
        if !sv && should_emit(n.get("layoutSizingVertical").and_then(|v| v.as_str()), axis) {
            dims.insert("height".to_string(), super::num(hgt));
        }
        if axis == Axis::Column
            && n.get("preserveRatio").and_then(|v| v.as_bool()) == Some(true)
            && hgt != 0.0
        {
            dims.insert("aspectRatio".to_string(), super::num(w / hgt));
        }
        if !dims.is_empty() {
            out.insert("dimensions".to_string(), Value::Object(dims));
        }
    }

    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    Row,
    Column,
    Grid,
    None,
}

fn resolve_child_axis(n: &Value, parent: Option<&Value>, own_mode: &str) -> Axis {
    if parent
        .and_then(|p| p.get("layoutMode"))
        .and_then(|m| m.as_str())
        == Some("GRID")
    {
        return Axis::Grid;
    }
    if is_in_auto_layout_flow(n, parent) {
        match parent_axis_row_col(parent) {
            Some("row") => return Axis::Row,
            Some("column") => return Axis::Column,
            _ => {}
        }
    }
    match own_mode {
        "row" => Axis::Row,
        "column" => Axis::Column,
        _ => Axis::None,
    }
}

fn child_stretch(n: &Value, axis: Axis) -> (bool, bool) {
    let grow = n.get("layoutGrow").and_then(|v| v.as_f64()).unwrap_or(0.0) != 0.0
        || n.get("layoutGrow").and_then(super::uint).unwrap_or(0) != 0;
    let align_stretch = n.get("layoutAlign").and_then(|v| v.as_str()) == Some("STRETCH");
    match axis {
        Axis::Grid => (
            n.get("layoutSizingHorizontal").and_then(|v| v.as_str()) == Some("FILL"),
            n.get("layoutSizingVertical").and_then(|v| v.as_str()) == Some("FILL"),
        ),
        Axis::Row => (grow, align_stretch),
        Axis::Column => (align_stretch, grow),
        Axis::None => (false, false),
    }
}

fn should_emit(sizing: Option<&str>, axis: Axis) -> bool {
    match axis {
        Axis::Row | Axis::Column => sizing == Some("FIXED"),
        _ => sizing.is_none() || sizing == Some("FIXED"),
    }
}

// ---------------------------------------------------------------------------
// Grid helpers (port of layout/grid.ts)
// ---------------------------------------------------------------------------

fn is_absolute_child(c: &Value) -> bool {
    c.get("layoutPositioning").and_then(|v| v.as_str()) == Some("ABSOLUTE") && bbox(c).is_some()
        || c.get("layoutPositioning").and_then(|v| v.as_str()) == Some("ABSOLUTE")
}

/// Anchor order for grid children (CSS auto-placement). None = already ordered.
pub fn compute_grid_child_order(parent: &Value) -> Option<Vec<usize>> {
    if parent.get("layoutMode").and_then(|v| v.as_str()) != Some("GRID") {
        return None;
    }
    let kids = parent.get("children")?.as_array()?;
    if kids.len() < 2 {
        return None;
    }
    let mut in_flow: Vec<usize> = (0..kids.len())
        .filter(|i| !is_absolute_child(&kids[*i]))
        .collect();
    in_flow.sort_by(|a, b| {
        let ra = kids[*a]
            .get("gridRowAnchorIndex")
            .and_then(super::uint)
            .unwrap_or(0);
        let rb = kids[*b]
            .get("gridRowAnchorIndex")
            .and_then(super::uint)
            .unwrap_or(0);
        ra.cmp(&rb).then_with(|| {
            let ca = kids[*a]
                .get("gridColumnAnchorIndex")
                .and_then(super::uint)
                .unwrap_or(0);
            let cb = kids[*b]
                .get("gridColumnAnchorIndex")
                .and_then(super::uint)
                .unwrap_or(0);
            ca.cmp(&cb).then_with(|| a.cmp(b))
        })
    });
    let mut result = Vec::with_capacity(kids.len());
    let mut cursor = 0;
    for (i, kid) in kids.iter().enumerate() {
        if is_absolute_child(kid) {
            result.push(i);
        } else {
            result.push(in_flow[cursor]);
            cursor += 1;
        }
    }
    if result.iter().enumerate().all(|(i, v)| *v == i) {
        None
    } else {
        Some(result)
    }
}

fn is_packed_grid(children: &[Value]) -> bool {
    let mut occupied = std::collections::HashSet::new();
    for c in children {
        if c.get("layoutPositioning").and_then(|v| v.as_str()) == Some("ABSOLUTE") {
            continue;
        }
        if bbox(c).is_none()
            && c.get("gridRowAnchorIndex").is_none()
            && c.get("gridColumnAnchorIndex").is_none()
        {
            continue;
        }
        let col = c
            .get("gridColumnAnchorIndex")
            .and_then(super::uint)
            .unwrap_or(0);
        let row = c
            .get("gridRowAnchorIndex")
            .and_then(super::uint)
            .unwrap_or(0);
        let cs = c
            .get("gridColumnSpan")
            .and_then(super::uint)
            .unwrap_or(1)
            .max(1);
        let rs = c
            .get("gridRowSpan")
            .and_then(super::uint)
            .unwrap_or(1)
            .max(1);
        for r in row..row + rs {
            for cc in col..col + cs {
                occupied.insert((r, cc));
            }
        }
    }
    if occupied.is_empty() {
        return true;
    }
    let max_r = occupied.iter().map(|(r, _)| *r).max().unwrap_or(0);
    let max_c = occupied.iter().map(|(_, c)| *c).max().unwrap_or(0);
    occupied.len() == ((max_r + 1) * (max_c + 1)) as usize
}

fn grid_children_overlap(parent: &Value) -> bool {
    let Some(kids) = parent.get("children").and_then(|c| c.as_array()) else {
        return false;
    };
    let boxes: Vec<(f64, f64, f64, f64)> = kids
        .iter()
        .filter(|c| c.get("layoutPositioning").and_then(|v| v.as_str()) != Some("ABSOLUTE"))
        .filter_map(bbox)
        .collect();
    for i in 0..boxes.len() {
        for j in (i + 1)..boxes.len() {
            let (ax, ay, aw, ah) = boxes[i];
            let (bx, by, bw, bh) = boxes[j];
            if ax < bx + bw && ax + aw > bx && ay < by + bh && ay + ah > by {
                return true;
            }
        }
    }
    false
}

fn convert_grid_align(a: Option<&str>) -> Option<&'static str> {
    match a {
        Some("MIN") => Some("start"),
        Some("MAX") => Some("end"),
        Some("CENTER") => Some("center"),
        _ => None,
    }
}

fn build_grid_child_positioning(n: &Value, parent: &Value, packed: bool) -> Vec<(String, Value)> {
    let mut out = vec![];
    let col_span = n
        .get("gridColumnSpan")
        .and_then(super::uint)
        .unwrap_or(1)
        .max(1);
    let row_span = n
        .get("gridRowSpan")
        .and_then(super::uint)
        .unwrap_or(1)
        .max(1);
    if !packed {
        let col = n
            .get("gridColumnAnchorIndex")
            .and_then(super::uint)
            .unwrap_or(0)
            + 1;
        let row = n
            .get("gridRowAnchorIndex")
            .and_then(super::uint)
            .unwrap_or(0)
            + 1;
        out.push((
            "gridColumn".to_string(),
            Value::String(if col_span > 1 {
                format!("{col} / span {col_span}")
            } else {
                format!("{col}")
            }),
        ));
        out.push((
            "gridRow".to_string(),
            Value::String(if row_span > 1 {
                format!("{row} / span {row_span}")
            } else {
                format!("{row}")
            }),
        ));
    } else {
        if col_span > 1 {
            out.push((
                "gridColumn".to_string(),
                Value::String(format!("span {col_span}")),
            ));
        }
        if row_span > 1 {
            out.push((
                "gridRow".to_string(),
                Value::String(format!("span {row_span}")),
            ));
        }
    }
    if let Some(a) = convert_grid_align(n.get("gridChildHorizontalAlign").and_then(|v| v.as_str()))
        && n.get("gridChildHorizontalAlign").and_then(|v| v.as_str()) != Some("AUTO")
    {
        out.push(("justifySelf".to_string(), Value::String(a.to_string())));
    }
    if let Some(a) = convert_grid_align(n.get("gridChildVerticalAlign").and_then(|v| v.as_str()))
        && n.get("gridChildVerticalAlign").and_then(|v| v.as_str()) != Some("AUTO")
    {
        out.push(("alignSelf".to_string(), Value::String(a.to_string())));
    }
    // zIndex when reorder moved this child and siblings overlap.
    if let Some(order) = compute_grid_child_order(parent)
        && grid_children_overlap(parent)
    {
        let kids = parent
            .get("children")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        let orig = kids.iter().position(|k| {
            k.get("id").and_then(|v| v.as_str()) == n.get("id").and_then(|v| v.as_str())
        });
        if let Some(o) = orig {
            let new_idx = order.iter().position(|v| *v == o);
            if new_idx != Some(o) {
                out.push(("zIndex".to_string(), Value::Number((o as u64).into())));
            }
        }
    }
    out
}
