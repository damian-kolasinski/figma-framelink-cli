//! Figma URL parsing. Mirrors upstream `utils/figma-url.ts`:
//! only /file/ and /design/ URLs are supported by the Figma REST API;
//! node-id uses dashes in URLs, colons in the API.

use anyhow::Result;

pub struct FigmaUrlParts {
    pub file_key: String,
    pub node_id: Option<String>,
}

pub fn parse_figma_url(input: &str) -> Result<FigmaUrlParts> {
    let url =
        url::Url::parse(input).map_err(|e| anyhow::anyhow!("not a valid URL {input:?}: {e}"))?;
    let host = url.host_str().unwrap_or("");
    if host != "figma.com" && !host.ends_with(".figma.com") {
        anyhow::bail!("not a Figma URL: {input}");
    }
    // Path looks like /file/<key>/... or /design/<key>/...
    let mut segs = url
        .path_segments()
        .ok_or_else(|| anyhow::anyhow!("URL has no path: {input}"))?;
    let kind = segs.next().unwrap_or("");
    let key = segs.next().unwrap_or("");
    if (kind != "file" && kind != "design") || !is_file_key(key) {
        anyhow::bail!(
            "could not extract file key from Figma URL: {input} (only /design/ and /file/ URLs are supported)"
        );
    }
    let node_id = url
        .query_pairs()
        .find(|(k, _)| k == "node-id")
        .map(|(_, v)| v.replace('-', ":"))
        .filter(|s| !s.is_empty());
    Ok(FigmaUrlParts {
        file_key: key.to_string(),
        node_id,
    })
}

fn is_file_key(k: &str) -> bool {
    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_design_url_with_node() {
        let p = parse_figma_url("https://www.figma.com/design/ABC123xyz/Site?node-id=1-2").unwrap();
        assert_eq!(p.file_key, "ABC123xyz");
        assert_eq!(p.node_id.as_deref(), Some("1:2"));
    }

    #[test]
    fn parses_file_url_without_node() {
        let p = parse_figma_url("https://www.figma.com/file/ABC123/Name").unwrap();
        assert_eq!(p.file_key, "ABC123");
        assert!(p.node_id.is_none());
    }

    #[test]
    fn rejects_non_figma_host() {
        assert!(parse_figma_url("https://example.com/design/ABC/x").is_err());
    }

    #[test]
    fn rejects_proto_links() {
        assert!(parse_figma_url("https://www.figma.com/proto/ABC123/x").is_err());
    }
}
