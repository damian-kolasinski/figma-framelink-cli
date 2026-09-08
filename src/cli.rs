//! CLI surface definition (clap). Mirrors the upstream MCP tool schemas plus
//! the `fetch` command's URL handling, reshaped for shell/agent invocation.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::serialize::OutputFormat;

#[derive(Debug, Parser)]
#[command(
    name = "figma-framelink-cli",
    version,
    about = "Read Figma designs and download assets without running an MCP server.\n\nPort of the Framelink Figma MCP server (figma-developer-mcp) as a single static binary for macOS/Linux agents.",
    after_help = "EXAMPLES:\n  \
        figma-framelink-cli get-figma-data \"https://www.figma.com/design/ABC123/Site?node-id=1-2\"\n  \
        figma-framelink-cli get-figma-data --file-key ABC123 --node-id 1:2 --format json\n  \
        figma-framelink-cli download-images --file-key ABC123 --nodes-json '[{\"nodeId\":\"1:2\",\"fileName\":\"hero.png\"}]' --local-path public/images\n\nCredentials resolve as: --figma-api-key/--figma-oauth-token flags > FIGMA_API_KEY/FIGMA_OAUTH_TOKEN env > .env file."
)]
pub struct Cli {
    /// Figma Personal Access Token (overrides FIGMA_API_KEY env).
    #[arg(long, global = true, env = "FIGMA_API_KEY", hide_env_values = true)]
    pub figma_api_key: Option<String>,

    /// Figma OAuth Bearer token (overrides FIGMA_OAUTH_TOKEN env). Takes
    /// precedence over the API key when both are set.
    #[arg(long, global = true, env = "FIGMA_OAUTH_TOKEN", hide_env_values = true)]
    pub figma_oauth_token: Option<String>,

    /// Path to a custom .env file to load before resolving credentials.
    #[arg(long, global = true)]
    pub env: Option<PathBuf>,

    /// HTTP proxy URL (e.g. http://proxy:8080). Also honors FIGMA_PROXY,
    /// HTTPS_PROXY/HTTP_PROXY + NO_PROXY via the standard env handling.
    /// Pass `none` to ignore proxy env vars and connect directly.
    #[arg(long, global = true, env = "FIGMA_PROXY")]
    pub proxy: Option<String>,

    /// Verbose stderr logging.
    #[arg(long, global = true, short = 'v')]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// `None` = default env handling; `Some(None)` = direct connection
    /// (`--proxy none`); `Some(Some(url))` = explicit proxy.
    pub fn proxy_config(&self) -> anyhow::Result<Option<Option<String>>> {
        match self.proxy.as_deref() {
            None => Ok(None),
            Some("none") => Ok(Some(None)),
            Some(url) => Ok(Some(Some(url.to_string()))),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Fetch a Figma file/node, simplify it like the MCP `get_figma_data`
    /// tool, and print it to stdout. Accepts a full Figma URL (from which
    /// fileKey/nodeId are parsed) or explicit --file-key/--node-id flags.
    #[command(alias = "fetch", alias = "get")]
    GetFigmaData(GetFigmaDataArgs),

    /// Download image/SVG/GIF assets like the MCP `download_figma_images`
    /// tool. Node specs come from the simplified `get-figma-data` output
    /// (imageRef/gifRef/fileName/needsCropping/cropTransform/...).
    #[command(alias = "download")]
    DownloadImages(DownloadImagesArgs),

    /// Parse a Figma URL and print its fileKey/nodeId (for agent plumbing).
    #[command(alias = "url")]
    ParseUrl(ParseUrlArgs),
}

#[derive(Debug, Args)]
pub struct GetFigmaDataArgs {
    /// Figma file/design URL. fileKey and node-id are extracted from it;
    /// explicit flags override the URL parts.
    pub url: Option<String>,

    /// Figma file key (overrides URL).
    #[arg(long)]
    pub file_key: Option<String>,

    /// Node ID like 1234:5678 (dashes accepted, converted to colons).
    /// Overrides URL.
    #[arg(long)]
    pub node_id: Option<String>,

    /// How many levels deep to traverse. Omit for the full subtree.
    /// Upstream marks this OPTIONAL — only set when explicitly needed.
    #[arg(long)]
    pub depth: Option<u32>,

    /// Output format: tree (default, compact + token-efficient), yaml, json.
    #[arg(long, value_parser = ["tree", "yaml", "json"])]
    pub format: Option<String>,

    /// Back-compat alias for --format=json (mirrors upstream --json flag).
    #[arg(long)]
    pub json: bool,

    /// Write output to a file instead of stdout.
    #[arg(long, short = 'o')]
    pub output: Option<PathBuf>,
}

impl GetFigmaDataArgs {
    pub fn format(&self) -> OutputFormat {
        match self.format.as_deref() {
            Some("json") => OutputFormat::Json,
            Some("yaml") => OutputFormat::Yaml,
            Some("tree") | None if self.json => OutputFormat::Json,
            _ => OutputFormat::Tree,
        }
    }
}

#[derive(Debug, Args)]
pub struct DownloadImagesArgs {
    /// Key of the Figma file containing the images.
    #[arg(long)]
    pub file_key: String,

    /// JSON array of node specs, matching the MCP tool schema:
    /// [{"nodeId":"1234:5678","imageRef":"...","gifRef":"...","fileName":"a.png",
    ///   "needsCropping":false,"cropTransform":[[..],[..]],
    ///   "requiresImageDimensions":false,"filenameSuffix":"abc123"}]
    /// Use --nodes-file to read the same JSON from a file.
    #[arg(long, conflicts_with = "nodes_file")]
    pub nodes_json: Option<String>,

    /// Path to a JSON file containing the nodes array (alternative to --nodes-json).
    #[arg(long)]
    pub nodes_file: Option<PathBuf>,

    /// Directory to save images in, resolved against --image-dir.
    /// e.g. 'public/images' or 'assets/icons'. Created if missing.
    #[arg(long)]
    pub local_path: String,

    /// Base directory all downloads must stay inside (default: cwd).
    /// Absolute --local-path values are accepted only inside this dir.
    #[arg(long)]
    pub image_dir: Option<PathBuf>,

    /// Export scale for PNG renders (default 2). PNG only.
    #[arg(long, default_value_t = 2.0)]
    pub png_scale: f64,
}

impl DownloadImagesArgs {
    pub fn load_nodes(&self) -> anyhow::Result<Vec<crate::images::DownloadNode>> {
        let raw = match (&self.nodes_json, &self.nodes_file) {
            (Some(j), _) => j.clone(),
            (_, Some(p)) => std::fs::read_to_string(p)
                .map_err(|e| anyhow::anyhow!("reading nodes file {}: {e}", p.display()))?,
            (None, None) => {
                return Err(anyhow::anyhow!(
                    "either --nodes-json or --nodes-file is required"
                ));
            }
        };
        let mut nodes: Vec<crate::images::DownloadNode> =
            serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("parsing nodes JSON: {e}"))?;
        if nodes.is_empty() {
            return Err(anyhow::anyhow!("nodes array is empty"));
        }
        for n in &mut nodes {
            n.node_id = n.node_id.replace('-', ":");
            validate_file_name(&n.file_name)?;
            if let Some(s) = &n.filename_suffix
                && !is_valid_suffix(s)
            {
                return Err(anyhow::anyhow!(
                    "invalid filenameSuffix {s:?}: use only letters, numbers, _ or -"
                ));
            }
        }
        Ok(nodes)
    }

    pub fn image_dir(&self) -> anyhow::Result<PathBuf> {
        match &self.image_dir {
            Some(d) => Ok(d.clone()),
            None => std::env::current_dir().map_err(|e| anyhow::anyhow!("resolving cwd: {e}")),
        }
    }
}

pub fn validate_file_name(name: &str) -> anyhow::Result<()> {
    let ok_ext = name.ends_with(".png") || name.ends_with(".svg") || name.ends_with(".gif");
    let ok_chars = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-');
    if !ok_ext || !ok_chars || name.len() > 200 {
        return Err(anyhow::anyhow!(
            "invalid fileName {name:?}: use only letters, numbers, _, . or - and end with .png, .svg or .gif"
        ));
    }
    Ok(())
}

fn is_valid_suffix(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[derive(Debug, Args)]
pub struct ParseUrlArgs {
    /// Figma file/design URL to parse.
    pub url: String,
}
