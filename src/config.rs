//! Credential + input resolution. Priority: CLI flag > env var > .env file.
//! Mirrors upstream `config.ts` (`resolveAuth`, `requireGlobalCredentials`)
//! minus the MCP-server parts (port/host/stdio/telemetry).

use std::path::Path;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Auth {
    pub api_key: String,
    pub oauth_token: String,
    pub use_oauth: bool,
}

/// Load `.env` if present. Explicit path wins; otherwise try `./.env`.
/// Missing files are fine (env/flags may already provide credentials).
pub fn load_env_file(explicit: Option<&Path>) {
    if let Some(p) = explicit {
        let _ = dotenvy::from_path(p);
        return;
    }
    let candidate = std::env::current_dir()
        .map(|d| d.join(".env"))
        .unwrap_or_else(|_| Path::new(".env").to_path_buf());
    if candidate.exists() {
        let _ = dotenvy::from_path(&candidate);
    }
}

pub fn resolve_auth(flag_key: Option<&str>, flag_oauth: Option<&str>) -> Result<Auth> {
    let api_key = flag_key
        .map(str::to_string)
        .or_else(|| std::env::var("FIGMA_API_KEY").ok())
        .unwrap_or_default();
    let oauth_token = flag_oauth
        .map(str::to_string)
        .or_else(|| std::env::var("FIGMA_OAUTH_TOKEN").ok())
        .unwrap_or_default();
    let auth = Auth {
        use_oauth: !oauth_token.is_empty(),
        api_key,
        oauth_token,
    };
    if !auth.use_oauth && auth.api_key.is_empty() {
        anyhow::bail!(
            "either FIGMA_API_KEY or FIGMA_OAUTH_TOKEN is required (via --figma-api-key/--figma-oauth-token, env, or .env file)"
        );
    }
    Ok(auth)
}

/// Merge URL + flags into (file_key, node_id). Mirrors the upstream `fetch`
/// command: URL parts fill gaps, flags win, file_key is required.
pub fn resolve_file_and_node(
    url: Option<&str>,
    file_key: Option<String>,
    node_id: Option<String>,
) -> Result<(String, Option<String>)> {
    let mut key = file_key;
    let mut node = node_id.map(|n| n.replace('-', ":"));

    if let Some(u) = url {
        match crate::figma_url::parse_figma_url(u) {
            Ok(parts) => {
                if key.is_none() {
                    key = Some(parts.file_key);
                }
                if node.is_none() {
                    node = parts.node_id;
                }
            }
            Err(e) => {
                // Malformed URL is non-fatal when --file-key was given
                // (mirrors upstream fetch behavior).
                if key.is_none() {
                    return Err(e).with_context(|| format!("parsing Figma URL {u:?}"));
                }
            }
        }
    }

    match key {
        Some(k) if is_valid_file_key(&k) => Ok((k, node)),
        Some(k) => Err(anyhow::anyhow!(
            "invalid file key {k:?}: must be alphanumeric"
        )),
        None => Err(anyhow::anyhow!("a Figma URL or --file-key is required")),
    }
}

fn is_valid_file_key(k: &str) -> bool {
    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric())
}
