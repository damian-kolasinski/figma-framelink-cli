//! Image downloads — port of `services/download-figma-images.ts` +
//! `services/figma.ts#downloadImages` + `utils/image-processing.ts`.
//!
//! Flow: dedupe requested nodes (imageRef fills collapse; gifRef + rendered
//! nodes stay unique; filenameSuffix disambiguates crops) → resolve Figma
//! URLs (fills vs PNG/SVG renders) → download bytes → write files →
//! post-process (crop rasters via transform matrix; SVG dims from markup;
//! CSS vars for TILE) → agent-readable summary.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::figma::FigmaClient;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DownloadNode {
    #[serde(rename = "nodeId")]
    pub node_id: String,
    #[serde(rename = "imageRef", default, skip_serializing_if = "Option::is_none")]
    pub image_ref: Option<String>,
    #[serde(rename = "gifRef", default, skip_serializing_if = "Option::is_none")]
    pub gif_ref: Option<String>,
    #[serde(rename = "fileName")]
    pub file_name: String,
    #[serde(rename = "needsCropping", default)]
    pub needs_cropping: bool,
    #[serde(
        rename = "cropTransform",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub crop_transform: Option<Vec<Vec<f64>>>,
    #[serde(rename = "requiresImageDimensions", default)]
    pub requires_image_dimensions: bool,
    #[serde(
        rename = "filenameSuffix",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub filename_suffix: Option<String>,
}

struct Item {
    file_name: String,
    needs_cropping: bool,
    crop_transform: Option<Vec<Vec<f64>>>,
    requires_dims: bool,
    image_ref: Option<String>,
    gif_ref: Option<String>,
    node_id: Option<String>,
}

pub struct DownloadResult {
    pub file_path: PathBuf,
    pub final_dims: (u32, u32),
    pub was_cropped: bool,
    pub css_vars: Option<String>,
    pub requested_names: Vec<String>,
}

/// Orchestrate the full download pipeline; returns the human/agent summary.
pub async fn download_images(
    client: &FigmaClient,
    file_key: &str,
    nodes: Vec<DownloadNode>,
    local_path: &Path,
    png_scale: f64,
) -> Result<String> {
    // 1. Dedupe into unique download items (mirrors upstream service).
    let mut items: Vec<Item> = vec![];
    let mut aliases: HashMap<usize, Vec<String>> = HashMap::new();
    let mut seen: HashMap<String, usize> = HashMap::new();

    for n in &nodes {
        let mut file_name = n.file_name.clone();
        if let Some(suffix) = &n.filename_suffix
            && !file_name.contains(suffix)
            && let Some(dot) = file_name.rfind('.')
        {
            file_name = format!("{}-{}.{}", &file_name[..dot], suffix, &file_name[dot + 1..]);
        }
        let base = Item {
            file_name: file_name.clone(),
            needs_cropping: n.needs_cropping,
            crop_transform: n.crop_transform.clone(),
            requires_dims: n.requires_image_dimensions,
            image_ref: None,
            gif_ref: None,
            node_id: None,
        };
        if let Some(g) = &n.gif_ref {
            let idx = items.len();
            items.push(Item {
                gif_ref: Some(g.clone()),
                ..base
            });
            aliases.insert(idx, vec![file_name]);
        } else if let Some(r) = &n.image_ref {
            let key = format!("{r}-{}", n.filename_suffix.as_deref().unwrap_or("none"));
            if n.filename_suffix.is_none()
                && let Some(&idx) = seen.get(&key)
            {
                let entry = aliases.entry(idx).or_default();
                if !entry.contains(&file_name) {
                    entry.push(file_name);
                }
                if base.requires_dims {
                    items[idx].requires_dims = true;
                }
            } else {
                let idx = items.len();
                items.push(Item {
                    image_ref: Some(r.clone()),
                    ..base
                });
                aliases.insert(idx, vec![file_name]);
                seen.insert(key, idx);
            }
        } else {
            let idx = items.len();
            items.push(Item {
                node_id: Some(n.node_id.clone()),
                ..base
            });
            aliases.insert(idx, vec![file_name]);
        }
    }

    // 2. Resolve URLs.
    let fill_urls: HashMap<String, String> = if items
        .iter()
        .any(|i| i.image_ref.is_some() || i.gif_ref.is_some())
    {
        client.get_image_fill_urls(file_key).await?
    } else {
        HashMap::new()
    };
    let png_ids: Vec<String> = items
        .iter()
        .filter(|i| i.node_id.is_some() && !i.file_name.to_lowercase().ends_with(".svg"))
        .filter_map(|i| i.node_id.clone())
        .collect();
    let svg_ids: Vec<String> = items
        .iter()
        .filter(|i| i.node_id.is_some() && i.file_name.to_lowercase().ends_with(".svg"))
        .filter_map(|i| i.node_id.clone())
        .collect();
    let png_urls = client
        .get_node_render_urls(file_key, &png_ids, "png", png_scale)
        .await?;
    let svg_urls = client
        .get_node_render_urls(file_key, &svg_ids, "svg", png_scale)
        .await?;

    // 3. Download + process each item.
    std::fs::create_dir_all(local_path)
        .with_context(|| format!("creating output dir {}", local_path.display()))?;
    let mut results: Vec<DownloadResult> = vec![];
    let mut missing: Vec<String> = vec![];
    for (idx, item) in items.iter().enumerate() {
        let url: Option<&String> = if let Some(g) = &item.gif_ref {
            fill_urls.get(g)
        } else if let Some(r) = &item.image_ref {
            fill_urls.get(r)
        } else if let Some(id) = &item.node_id {
            if item.file_name.to_lowercase().ends_with(".svg") {
                svg_urls.get(id)
            } else {
                png_urls.get(id)
            }
        } else {
            None
        };
        let Some(url) = url else {
            missing.push(item.file_name.clone());
            continue;
        };
        let bytes = client.download_bytes(url).await?;
        let dest = local_path.join(&item.file_name);
        let res = process_bytes(&bytes, &item.file_name, &dest, item).await?;
        let mut res = res;
        res.requested_names = aliases
            .get(&idx)
            .cloned()
            .unwrap_or_else(|| vec![item.file_name.clone()]);
        results.push(res);
    }

    if results.is_empty() {
        anyhow::bail!(
            "no images could be resolved ({} requested, {} missing URLs: {}). The imageRef/gifRef values must come from the get-figma-data output for this file; node renders return null when the node cannot be exported.",
            nodes.len(),
            missing.len(),
            missing.join(", ")
        );
    }

    // 4. Summary (mirrors MCP tool formatting).
    let mut lines = vec![];
    for r in &results {
        let name = r
            .file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?");
        let dims = format!("{}x{}", r.final_dims.0, r.final_dims.1);
        let dim_info = match &r.css_vars {
            Some(v) => format!("{dims} | {v}"),
            None => dims,
        };
        let crop = if r.was_cropped { " (cropped)" } else { "" };
        let extra: Vec<&str> = r
            .requested_names
            .iter()
            .map(|s| s.as_str())
            .filter(|s| *s != name)
            .collect();
        let alias = if extra.is_empty() {
            String::new()
        } else {
            format!(" (also requested as: {})", extra.join(", "))
        };
        lines.push(format!("- {name}: {dim_info}{crop}{alias}"));
    }
    if !missing.is_empty() {
        lines.push(format!("(missing URLs for: {})", missing.join(", ")));
    }
    Ok(format!(
        "Downloaded {} images to `{}`:\n{}",
        results.len(),
        local_path.display(),
        lines.join("\n")
    ))
}

async fn process_bytes(
    bytes: &[u8],
    file_name: &str,
    dest: &Path,
    item: &Item,
) -> Result<DownloadResult> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating dir {}", parent.display()))?;
    }
    let lower = file_name.to_lowercase();
    if lower.ends_with(".svg") {
        std::fs::write(dest, bytes).with_context(|| format!("writing {}", dest.display()))?;
        let text = String::from_utf8_lossy(bytes);
        let dims = parse_svg_dimensions(&text);
        let css = item.requires_dims.then(|| css_vars(dims));
        return Ok(DownloadResult {
            file_path: dest.to_path_buf(),
            final_dims: dims,
            was_cropped: false,
            css_vars: css,
            requested_names: vec![],
        });
    }

    // GIFs: never crop (destroys animation) — mirror upstream.
    if lower.ends_with(".gif") || !item.needs_cropping || item.crop_transform.is_none() {
        std::fs::write(dest, bytes).with_context(|| format!("writing {}", dest.display()))?;
        let dims = raster_dimensions(bytes).unwrap_or((0, 0));
        let css = item.requires_dims.then(|| css_vars(dims));
        return Ok(DownloadResult {
            file_path: dest.to_path_buf(),
            final_dims: dims,
            was_cropped: false,
            css_vars: css,
            requested_names: vec![],
        });
    }

    // Raster crop via transform matrix [[sx, skew, tx],[sy...]].
    let t = item.crop_transform.as_ref().unwrap();
    let (sx, tx) = (
        t.first().and_then(|r| r.first()).copied().unwrap_or(1.0),
        t.first().and_then(|r| r.get(2)).copied().unwrap_or(0.0),
    );
    let (sy, ty) = (
        t.get(1).and_then(|r| r.get(1)).copied().unwrap_or(1.0),
        t.get(1).and_then(|r| r.get(2)).copied().unwrap_or(0.0),
    );

    let img = image::load_from_memory(bytes).context("decoding downloaded image")?;
    let (w, h) = (img.width(), img.height());
    let left = (tx * w as f64).round().clamp(0.0, w as f64) as u32;
    let top = (ty * h as f64).round().clamp(0.0, h as f64) as u32;
    let cw = ((sx * w as f64).round() as u32).min(w.saturating_sub(left));
    let ch = ((sy * h as f64).round() as u32).min(h.saturating_sub(top));
    if cw == 0 || ch == 0 {
        std::fs::write(dest, bytes).with_context(|| format!("writing {}", dest.display()))?;
        let css = item.requires_dims.then(|| css_vars((w, h)));
        return Ok(DownloadResult {
            file_path: dest.to_path_buf(),
            final_dims: (w, h),
            was_cropped: false,
            css_vars: css,
            requested_names: vec![],
        });
    }
    let cropped = img.crop_imm(left, top, cw, ch);
    cropped
        .save(dest)
        .with_context(|| format!("writing cropped {}", dest.display()))?;
    let css = item.requires_dims.then(|| css_vars((cw, ch)));
    Ok(DownloadResult {
        file_path: dest.to_path_buf(),
        final_dims: (cw, ch),
        was_cropped: true,
        css_vars: css,
        requested_names: vec![],
    })
}

fn raster_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::load_from_memory(bytes)
        .ok()
        .map(|i| (i.width(), i.height()))
}

fn css_vars((w, h): (u32, u32)) -> String {
    format!("--original-width: {w}px; --original-height: {h}px;")
}

/// Read intrinsic SVG size from markup: width/height attrs, else viewBox.
pub fn parse_svg_dimensions(svg: &str) -> (u32, u32) {
    let tag = svg
        .find("<svg")
        .and_then(|i| svg[i..].find('>').map(|j| &svg[i..i + j + 1]))
        .unwrap_or("");
    let attr = |name: &str| -> Option<String> {
        for q in ['"', '\''] {
            let needle = format!("{name}={q}");
            if let Some(i) = tag.find(&needle) {
                let rest = &tag[i + needle.len()..];
                if let Some(end) = rest.find(q) {
                    return Some(rest[..end].to_string());
                }
            }
        }
        None
    };
    let len = |raw: Option<String>| -> Option<f64> {
        let r = raw?;
        if r.contains('%') {
            return None;
        }
        r.trim_end_matches(|c: char| c.is_alphabetic())
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| *v > 0.0)
    };
    if let (Some(w), Some(h)) = (len(attr("width")), len(attr("height"))) {
        return (w as u32, h as u32);
    }
    if let Some(vb) = attr("viewBox") {
        let parts: Vec<f64> = vb
            .split([' ', ','])
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        if parts.len() == 4 && parts[2] > 0.0 && parts[3] > 0.0 {
            return (parts[2] as u32, parts[3] as u32);
        }
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_dims_from_attrs() {
        assert_eq!(
            parse_svg_dimensions(r#"<svg width="52" height="24">"#,),
            (52, 24)
        );
    }

    #[test]
    fn svg_dims_from_viewbox() {
        assert_eq!(
            parse_svg_dimensions(r#"<svg viewBox="0 0 100 50">"#),
            (100, 50)
        );
    }

    #[test]
    fn svg_dims_missing() {
        assert_eq!(parse_svg_dimensions("<svg></svg>"), (0, 0));
    }
}
