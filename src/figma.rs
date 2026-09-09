//! Async Figma REST client. Mirrors upstream `services/figma.ts`:
//! auth headers, raw file/node fetch, image-fill + render URL resolution,
//! and batched downloads with SVG/PNG routing.

use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::Value;

use crate::config::Auth;

const BASE_URL: &str = "https://api.figma.com/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProxyMode {
    /// Default: reqwest honors HTTPS_PROXY/HTTP_PROXY/NO_PROXY itself.
    Env,
    /// `--proxy none`: direct connection.
    None,
    /// Explicit `--proxy URL` / FIGMA_PROXY.
    Explicit,
}

fn proxy_env_present() -> bool {
    [
        "FIGMA_PROXY",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "https_proxy",
        "http_proxy",
    ]
    .iter()
    .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

/// Port of upstream `buildForbiddenMessage`
/// (`src/services/errors/forbidden.ts` in `figma-developer-mcp`):
/// surface Figma's response body verbatim — Figma returns distinct `err`
/// strings for distinct causes (expired token, missing scopes, un-exportable
/// file, ...) — and list the common causes it could map to, instead of
/// dumping only a canned guess.
pub fn build_forbidden_message(endpoint: &str, body: &str, proxy_mode: ProxyMode) -> String {
    const FORBIDDEN_CAUSES: &[&str] = &[
        "- The access token is missing required scopes (File content: Read, Dev resources: Read)",
        "- The access token has expired, been revoked, or was mistyped (both PATs and OAuth tokens can expire)",
        "- The access token doesn't have permission to this specific file — it must be owned by or shared with the token's account, and for team/org files the account must belong to that team",
        "- The file's share settings don't allow viewers to copy/share/export",
        "- An HTTP intermediary (corporate proxy, firewall, VPN) rejected the request before it reached Figma",
    ];
    const TROUBLESHOOTING_GUIDE: &str =
        "Troubleshooting guide: https://www.framelink.ai/docs/troubleshooting#cannot-access-file";
    const LLM_INSTRUCTIONS: &str = "Instructions: explain the specific reason from the response body above to the user in plain language and walk them through resolving it.";

    let body = body.trim();
    // Cap pathological bodies (e.g. proxy HTML pages); Figma `err` payloads
    // are tiny, so this only kicks in for non-Figma responses.
    let body: Option<String> = if body.is_empty() {
        None
    } else if body.chars().count() > 2000 {
        Some(format!(
            "{}... [truncated {} chars]",
            body.chars().take(2000).collect::<String>(),
            body.chars().count() - 2000
        ))
    } else {
        Some(body.to_string())
    };

    let mut sections: Vec<String> = Vec::new();
    let mut first = format!("Request to Figma API endpoint '{endpoint}' returned 403 Forbidden.");
    if let Some(b) = &body {
        first.push_str(&format!("\nResponse body: {b}"));
        // Expired credentials come back as 403 `{"err":"Token expired"}` on
        // file/nodes endpoints (vs 401 `"Token has expired"` on /v1/me).
        // Without this pointer, agents debug file sharing / REST API plan
        // instead of replacing the token.
        if b.to_lowercase().contains("expir") {
            first.push_str("\nNote: the response body indicates the token is expired — replace the Figma token before investigating file access or plan limits.");
        }
    }
    sections.push(first);

    let header = if body.is_some() {
        "Depending on the specific error message above, the issue may be one of the following:"
    } else {
        "The issue is typically one of the following:"
    };
    sections.push(format!("{header}\n{}", FORBIDDEN_CAUSES.join("\n")));
    sections.push(TROUBLESHOOTING_GUIDE.to_string());
    if body.is_some() {
        sections.push(LLM_INSTRUCTIONS.to_string());
    }
    let proxy_hint: Option<&str> = match proxy_mode {
        ProxyMode::Explicit => Some(
            "Note: this CLI is configured to route requests through an explicit proxy (--proxy/FIGMA_PROXY). If the proxy may be the source of the 403, unset it, pass --proxy=none, or bypass it for this host.",
        ),
        ProxyMode::Env if proxy_env_present() => Some(
            "Note: this CLI picked up a proxy from HTTP_PROXY/HTTPS_PROXY in your environment. If the proxy may be the source of the 403, set NO_PROXY=api.figma.com, pass --proxy=none, or unset HTTP_PROXY/HTTPS_PROXY.",
        ),
        ProxyMode::Env | ProxyMode::None => None,
    };
    if let Some(hint) = proxy_hint {
        sections.push(hint.to_string());
    }
    sections.join("\n\n")
}

/// 401 means the credential itself was rejected (expired, revoked, mistyped)
/// before any file ACL or plan check ran. Surface the body verbatim so a
/// `{"err":"Token has expired"}` is never mistaken for a permissions issue.
pub fn build_unauthorized_message(endpoint: &str, body: &str, use_oauth: bool) -> String {
    let credential = if use_oauth {
        "FIGMA_OAUTH_TOKEN (--figma-oauth-token)"
    } else {
        "FIGMA_API_KEY (--figma-api-key)"
    };
    let body = body.trim();
    let mut msg = format!(
        "Figma API returned 401 for '{endpoint}': authentication failed — the token was rejected before any file access was checked. The token has likely expired, been revoked, or was mistyped; verify {credential}."
    );
    if !body.is_empty() {
        let short: String = body.chars().take(2000).collect();
        msg.push_str(&format!("\nResponse body: {short}"));
        if body.to_lowercase().contains("expir") {
            msg.push_str("\nNote: the response indicates the token is expired — create a replacement token rather than debugging file sharing or plan limits.");
        }
    }
    msg.push_str("\nTroubleshooting guide: https://www.framelink.ai/docs/troubleshooting");
    msg
}

#[derive(Debug, Clone)]
pub struct FigmaClient {
    client: Client,
    api_key: String,
    oauth_token: String,
    use_oauth: bool,
    proxy_mode: ProxyMode,
}

impl FigmaClient {
    pub fn new(auth: Auth, proxy: Option<Option<String>>) -> Result<Self> {
        let mut builder = Client::builder()
            .user_agent(concat!("figma-framelink-cli/", env!("CARGO_PKG_VERSION")));
        let mut proxy_mode = ProxyMode::Env;
        match proxy {
            // Explicit --proxy URL.
            Some(Some(url)) => {
                builder = builder.proxy(
                    reqwest::Proxy::all(&url)
                        .with_context(|| format!("parsing --proxy URL {url:?}"))?,
                );
                proxy_mode = ProxyMode::Explicit;
            }
            // --proxy none: ignore env proxies.
            Some(None) => {
                builder = builder.no_proxy();
                proxy_mode = ProxyMode::None;
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
            proxy_mode,
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
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "{}",
                build_forbidden_message(endpoint, &body, self.proxy_mode)
            );
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "{}",
                build_unauthorized_message(endpoint, &body, self.use_oauth)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_message_preserves_expired_token_body() {
        // Regression test: an expired PAT returns 403 `{"err":"Token expired"}`
        // on file/nodes endpoints. The message must surface that body verbatim
        // instead of only blaming file ACL / REST API plan.
        let msg = build_forbidden_message(
            "/files/ABC/nodes?ids=1:2",
            r#"{"status":403,"err":"Token expired"}"#,
            ProxyMode::None,
        );
        assert!(msg.contains("403 Forbidden"), "{msg}");
        assert!(msg.contains("Token expired"), "{msg}");
        assert!(msg.contains("Response body:"), "{msg}");
        // Must not send operators down the file-sharing path first.
        assert!(
            msg.to_lowercase().contains("expired"),
            "expected expired-token pointer in: {msg}"
        );
        assert!(msg.contains("framelink.ai/docs/troubleshooting"), "{msg}");
    }

    #[test]
    fn forbidden_message_without_body_uses_generic_header() {
        let msg = build_forbidden_message("/files/ABC", "", ProxyMode::None);
        assert!(msg.contains("403 Forbidden"), "{msg}");
        assert!(!msg.contains("Response body:"), "{msg}");
        assert!(msg.contains("typically one of the following"), "{msg}");
    }

    #[test]
    fn unauthorized_message_preserves_body_and_names_credential() {
        let msg = build_unauthorized_message(
            "/v1/me",
            r#"{"status":401,"err":"Token has expired"}"#,
            false,
        );
        assert!(msg.contains("401"), "{msg}");
        assert!(msg.contains("Token has expired"), "{msg}");
        assert!(msg.contains("FIGMA_API_KEY"), "{msg}");
    }

    #[test]
    fn unauthorized_message_names_oauth_credential() {
        let msg = build_unauthorized_message("/v1/me", "unauthorized", true);
        assert!(msg.contains("FIGMA_OAUTH_TOKEN"), "{msg}");
    }
}
