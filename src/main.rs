//! figma-framelink-cli: agent-first Rust port of the Framelink Figma MCP server.
//!
//! The upstream MCP server (`figma-developer-mcp`, GLips/Figma-Context-MCP)

#![recursion_limit = "512"]
//! exposes two tools over MCP stdio/HTTP:
//!   - `get_figma_data`    → fetch + simplify + serialize a file/node
//!   - `download_figma_images` → resolve + download + post-process images
//!
//! This binary exposes the same two capabilities as plain subcommands that
//! print to stdout, so any coding agent with shell access can use them
//! without installing or configuring an MCP server per harness:
//!
//! ```sh
//! export FIGMA_API_KEY=figd_...
//! figma-framelink-cli get-figma-data "https://www.figma.com/design/ABC123/Name?node-id=1-2"
//! figma-framelink-cli download-images --file-key ABC123 \
//!   --nodes-json '[{"nodeId":"1:2","fileName":"icon.svg"}]' --local-path public/images
//! ```

mod cli;
mod config;
mod figma;
mod figma_url;
mod images;
mod path_util;
mod serialize;
mod simplify;

use anyhow::{Context, Result};
use clap::Parser;

use cli::{Cli, Command};

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        // Usage errors print bare; unexpected errors get a prefix.
        // `anyhow` chains with `: ` separators — print the full chain once.
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();

    // Load .env first so env-backed auth resolution sees it (mirrors upstream
    // `loadEnvFile`: explicit --env path wins, otherwise ./​.env if present).
    config::load_env_file(cli.env.as_deref());

    let proxy = cli.proxy_config()?;
    let verbose = cli.verbose;
    if verbose {
        eprintln!("figma-framelink-cli v{}", env!("CARGO_PKG_VERSION"));
    }

    match cli.command {
        Command::GetFigmaData(args) => {
            let auth = config::resolve_auth(
                cli.figma_api_key.as_deref(),
                cli.figma_oauth_token.as_deref(),
            )?;
            let (file_key, node_id) = config::resolve_file_and_node(
                args.url.as_deref(),
                args.file_key.clone(),
                args.node_id.clone(),
            )?;
            let depth = args.depth;
            let format = args.format();
            let client = figma::FigmaClient::new(auth, proxy)?;
            let formatted =
                simplify::get_figma_data(&client, &file_key, node_id.as_deref(), depth, format)
                    .await
                    .with_context(|| format!("fetching Figma data for file {file_key}"))?;
            if let Some(out) = args.output {
                std::fs::write(&out, &formatted)
                    .with_context(|| format!("writing output to {}", out.display()))?;
            } else {
                print!("{formatted}");
                if !formatted.ends_with('\n') {
                    println!();
                }
            }
            Ok(())
        }
        Command::DownloadImages(args) => {
            let auth = config::resolve_auth(
                cli.figma_api_key.as_deref(),
                cli.figma_oauth_token.as_deref(),
            )?;
            let nodes = args.load_nodes()?;
            let base_dir = args.image_dir()?;
            let local_path =
                path_util::resolve_local_path(&args.local_path, &base_dir).map_err(|e| {
                    anyhow::anyhow!(
                        "invalid --local-path {:?}: {} (image dir is {:?}; pass a path relative to it, e.g. \"public/images\")",
                        args.local_path,
                        e,
                        base_dir.display()
                    )
                })?;
            let client = figma::FigmaClient::new(auth, proxy)?;
            let summary = images::download_images(
                &client,
                &args.file_key,
                nodes,
                &local_path,
                args.png_scale,
            )
            .await?;
            println!("{summary}");
            Ok(())
        }
        Command::ParseUrl(args) => {
            let parts = figma_url::parse_figma_url(&args.url)?;
            println!("fileKey: {}", parts.file_key);
            match parts.node_id {
                Some(n) => println!("nodeId: {n}"),
                None => println!("nodeId: (none)"),
            }
            Ok(())
        }
    }
}
