# figma-framelink-cli

Agent-first Rust port of the [Framelink Figma MCP server](https://github.com/GLips/Figma-Context-MCP)
(`figma-developer-mcp`) as a single static binary for macOS and Linux.

Instead of installing and configuring an MCP server in every harness
(stdio command, env vars, per-client JSON), agents with shell access run:

```sh
export FIGMA_API_KEY=figd_...
figma-framelink-cli get-figma-data "https://www.figma.com/design/ABC123/Site?node-id=1-2"
figma-framelink-cli download-images --file-key ABC123 \
  --nodes-json '[{"nodeId":"1:2","fileName":"hero.png"}]' --local-path public/images
```

## Why a CLI instead of MCP?

| Concern | MCP server | This CLI |
|---|---|---|
| Install per harness | `npx -y figma-developer-mcp` + client JSON in each agent harness | one binary on `PATH` |
| Runtime | Node ≥ 20 + npm fetch at first run | zero runtime deps, static binary |
| Transports | stdio / HTTP server to babysit | stdout + files, exit codes |
| Telemetry | on by default (`FRAMELINK_TELEMETRY=off` to disable) | none, no network calls except api.figma.com |
| Output | `tree` / `yaml` / `json` via tool config | same three via `--format` |

Behavioral parity is the goal: same Figma REST calls, same simplified
schema (`layout`, `fills`, `textStyle`, `globalVars`, `elements`,
`components`, …), same path-safety rules for downloads.

## Install

Latest release via curl (detects macOS/Linux + x86_64/arm64, installs to
`/usr/local/bin` when writable, otherwise `~/.local/bin`):

```sh
curl -fsSL https://raw.githubusercontent.com/damian-kolasinski/figma-framelink-cli/main/install.sh | sh
```

Pinned version or custom directory:

```sh
curl -fsSL https://raw.githubusercontent.com/damian-kolasinski/figma-framelink-cli/main/install.sh | sh -s -- --version v0.1.0
curl -fsSL https://raw.githubusercontent.com/damian-kolasinski/figma-framelink-cli/main/install.sh | sh -s -- --dir ~/.local/bin
```

Or download the tarball directly (one per [release](../../releases)):

```sh
# macOS arm64 / Linux x86_64 shown; see install.sh for the other targets
curl -fsSL -o figma.tgz https://github.com/damian-kolasinski/figma-framelink-cli/releases/latest/download/figma-framelink-cli-aarch64-apple-darwin.tar.gz
tar xzf figma.tgz
./figma-framelink-cli --help
```

Build from source (Rust stable):

```sh
cargo build --release
# binary at ./target/release/figma-framelink-cli
```

## Credentials

Resolution order: `--figma-api-key` / `--figma-oauth-token` flags →
`FIGMA_API_KEY` / `FIGMA_OAUTH_TOKEN` env → `.env` file in cwd
(`--env PATH` for a custom one). OAuth takes precedence when both are set,
mirroring upstream.

Create a token under Figma → Settings → Security → Personal access tokens.

## Usage

### `get-figma-data` (aliases: `fetch`, `get`)

Mirrors the MCP `get_figma_data` tool: fetch a file or node, simplify the
raw Figma response (layout / fills / strokes / effects / rich text /
components), and print it.

```sh
# Full URL (fileKey + node-id parsed automatically)
figma-framelink-cli get-figma-data "https://www.figma.com/design/ABC123/Site?node-id=1-2"

# Explicit keys, JSON output, depth-limited
figma-framelink-cli get-figma-data --file-key ABC123 --node-id 1:2 --format json --depth 3

# Save to file instead of stdout
figma-framelink-cli get-figma-data --file-key ABC123 -o design.yaml --format yaml
```

Node IDs accept dashes (`1-2`, as found in URLs) or colons (`1:2`).
`--depth` is optional — omit it for the full subtree (upstream marks it
"do NOT use unless explicitly requested").

Output formats: `tree` (default — compact, token-efficient, same as the MCP
server default), `yaml` (upstream `fetch` CLI default, good for piping),
`json`.

### `download-images` (alias: `download`)

Mirrors the MCP `download_figma_images` tool. The node specs come from the
simplified `get-figma-data` output — each `IMAGE` fill carries `imageRef`
(+ `imageDownloadArguments` with `needsCropping` / `cropTransform` /
`requiresImageDimensions` / `filenameSuffix`); animated GIFs carry `gifRef`.

```sh
figma-framelink-cli download-images --file-key ABC123 \
  --nodes-json '[
    {"nodeId":"1:2","fileName":"hero.png","requiresImageDimensions":true},
    {"nodeId":"1:3","imageRef":"abc123","fileName":"logo.png"},
    {"nodeId":"1:4","fileName":"icon.svg"}
  ]' \
  --local-path public/images --png-scale 2
```

- `--nodes-file PATH` reads the same JSON array from a file (better for long lists).
- `--local-path` is resolved against `--image-dir` (default: cwd) and created
  if missing. Absolute paths are accepted only inside the image dir; escapes
  (`../..`, other roots, drive letters on POSIX) are rejected with a retry hint.
- PNG renders use `--png-scale` (default 2, PNG only); SVGs are fetched with
  outlined text / simplified strokes like upstream.
- Rasters with a `cropTransform` are cropped; GIFs are never cropped
  (preserves animation); SVGs report intrinsic dimensions from markup;
  TILE fills emit `--original-width/height` CSS vars when requested.
- Filenames must match `[A-Za-z0-9_.-]+\.(png|svg|gif)`; identical `imageRef`s
  dedupe to one download (aliases listed in the summary).

Output mirrors the MCP tool: `Downloaded N images to \`<path>\`:` plus
`- <file>: <WxH> [| <css vars>] [(cropped)]`.

### `parse-url` (alias: `url`)

Plumbing helper for agents: extract `fileKey` / `nodeId` from a Figma URL.

```sh
figma-framelink-cli parse-url "https://www.figma.com/design/ABC123/Site?node-id=1-2"
# fileKey: ABC123
# nodeId: 1:2
```

Only `/design/` and `/file/` URLs are supported by the Figma REST API —
proto/figjam/slides/board/deck links are rejected with an explanation.

### Proxy

`--proxy URL` / `FIGMA_PROXY`, or `none` to bypass `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY`.
Standard proxy env vars are honored by default.

## Agent recipe

1. `parse-url` (or paste the URL directly into `get-figma-data`).
2. `get-figma-data` → read layout/styles/text; note `imageRef`/`gifRef` fills.
3. `download-images` with those refs into the project's asset dir.
4. Implement the design; use reported `WxH` / CSS vars for TILE backgrounds.

No MCP configuration, no daemon, no Node — the same binary works in every
harness on macOS/Linux.

## Development

```sh
cargo test          # unit + pipeline tests (offline fixtures)
cargo clippy -- -D warnings
cargo fmt --check
```

## License

MIT — see [LICENSE](LICENSE). Upstream project: GLips/Figma-Context-MCP (MIT).
