//! Async Figma REST client. Mirrors upstream `services/figma.ts`:
//! auth headers, raw file/node fetch, image-fill + render URL resolution,
//! and batched downloads with SVG/PNG routing.

use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::Value;

use crate::config::Auth;

const BASE_URL: &str = "https://api.figma.com/v1";

#[derive(Debug, Clone)]
pub struct FigmaClient {
    client: Client,
    api_key: String,
    oauth_token: String,
    use_oauth: bool,
}

impl FigmaClient {
    pub fn new(auth: Auth, proxy: Option<Option<String>>) -> Result<Self> {
        let mut builder = Client::builder()
            .user_agent(concat!("figma-framelink-cli/", env!("CARGO_PKG_VERSION")));
        match proxy {
            // Explicit --proxy URL.
            Some(Some(url)) => {
                builder = builder.proxy(
                    reqwest::Proxy::all(&url)
                        .with_context(|| format!("parsing --proxy URL {url:?}"))?,
                );
            }
            // --proxy none: ignore env proxies.
            Some(None) => {
                builder = builder.no_proxy();
            }
            // Default: reqwest honors HTTPS_PROXY/HTTP_PROXY/NO_PROXY itself.
            None => {}
        }
        let client = builder.build().context("building HTTP client")?;
        Ok(Self {
            client,
            api_key: auth.api_key,
            oauth_token: auth.oauth_token,
            use_oauth: auth.use_oauth,
        })
    }

    fn auth_header(&self) -> Result<(String, String)> {
        if self.use_oauth {
            Ok((
                "Authorization".to_string(),
                format!("Bearer {}", self.oauth_token),
            ))
        } else if !self.api_key.is_empty() {
            Ok(("X-Figma-Token".to_string(), self.api_key.clone()))
        } else {
            anyhow::bail!(
                "Figma API authentication is required (FIGMA_API_KEY or FIGMA_OAUTH_TOKEN)"
            )
        }
    }

    async fn get_json(&self, endpoint: &str) -> Result<Value> {
        let (name, value) = self.auth_header()?;
        let url = format!("{BASE_URL}{endpoint}");
        let resp = self
            .client
            .get(&url)
            .header(name, value)
            .send()
            .await
            .with_context(|| format!("requesting Figma API endpoint {endpoint:?}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            anyhow::bail!(
                "Figma API rate limit exceeded (429). Wait before retrying; free-plan files are most affected."
            );
        }
        if status == reqwest::StatusCode::FORBIDDEN {
            anyhow::bail!(
                "Figma API returned 403 for {endpoint:?}: the token likely lacks access to this file, or the file uses features outside the REST API plan."
            );
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let short = body.chars().take(300).collect::<String>();
            anyhow::bail!("Figma API request to {endpoint:?} failed with status {status}: {short}");
        }
        resp.json::<Value>()
            .await
            .with_context(|| format!("decoding Figma API response for {endpoint:?}"))
    }

    fn encode_ids(ids: &[String]) -> String {
        ids.join(",")
    }

    /// GET /files/:key — full document (optional depth).
    pub async fn get_raw_file(&self, file_key: &str, depth: Option<u32>) -> Result<Value> {
        let endpoint = match depth {
            Some(d) => format!("/files/{file_key}?depth={d}"),
            None => format!("/files/{file_key}"),
        };
        self.get_json(&endpoint).await
    }

    /// GET /files/:key/nodes?ids=... — single node subtree.
    pub async fn get_raw_node(
        &self,
        file_key: &str,
        node_id: &str,
        depth: Option<u32>,
    ) -> Result<Value> {
        let endpoint = match depth {
            Some(d) => format!("/files/{file_key}/nodes?ids={node_id}&depth={d}"),
            None => format!("/files/{file_key}/nodes?ids={node_id}"),
        };
        let body = self.get_json(&endpoint).await?;
        // Mirror upstream's actionable not-found error (proto/figjam/branch/stale links).
        if let Some(nodes) = body.get("nodes").and_then(|n| n.as_object()) {
            for (id, data) in nodes {
                if data.is_null() {
                    anyhow::bail!(
                        "node {id} was not found in the Figma file. Likely causes: (1) the URL was a /proto/, /figjam/, /slides/, /board/ or /deck/ link — only /design/ and /file/ URLs are supported; (2) the node is inside a Figma branch (use the branch fileKey); (3) the link is stale or the node was deleted."
                    );
                }
            }
        }
        Ok(body)
    }

    /// GET /files/:key/images — imageRef → download URL map.
    pub async fn get_image_fill_urls(
        &self,
        file_key: &str,
    ) -> Result<std::collections::HashMap<String, String>> {
        let body = self.get_json(&format!("/files/{file_key}/images")).await?;
        let mut out = std::collections::HashMap::new();
        if let Some(images) = body.pointer("/meta/images").and_then(|v| v.as_object()) {
            for (k, v) in images {
                if let Some(url) = v.as_str()
                    && !url.is_empty()
                {
                    out.insert(k.clone(), url.to_string());
                }
            }
        }
        Ok(out)
    }

    /// GET /images/:key — nodeId → rendered image URL map (png or svg).
    pub async fn get_node_render_urls(
        &self,
        file_key: &str,
        node_ids: &[String],
        format: &str,
        png_scale: f64,
    ) -> Result<std::collections::HashMap<String, String>> {
        if node_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let endpoint = if format == "png" {
            // Figma scales: 0.01–4. Clamp defensively.
            let scale = png_scale.clamp(0.01, 4.0);
            // Render as integer when whole to keep URLs canonical.
            let scale_q = if scale.fract() == 0.0 {
                format!("{}", scale as u64)
            } else {
                format!("{scale}")
            };
            format!(
                "/images/{file_key}?ids={}&format=png&scale={scale_q}",
                Self::encode_ids(node_ids)
            )
        } else {
            format!(
                "/images/{file_key}?ids={}&format=svg&svg_outline_text=true&svg_include_id=false&svg_simplify_stroke=true",
                Self::encode_ids(node_ids)
            )
        };
        let body = self.get_json(&endpoint).await?;
        let mut out = std::collections::HashMap::new();
        if let Some(images) = body.get("images").and_then(|v| v.as_object()) {
            for (k, v) in images {
                if let Some(url) = v.as_str()
                    && !url.is_empty()
                {
                    out.insert(k.clone(), url.to_string());
                }
            }
        }
        Ok(out)
    }

    /// Raw bytes download for one image URL.
    pub async fn download_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("downloading image from {url}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("image download failed with status {}", resp.status());
        }
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .context("reading image bytes")
    }
}
