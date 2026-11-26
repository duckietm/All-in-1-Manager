//! A retro's catalogue image and icon URLs, read from its client configuration:
//! * Nitro: `renderer-config.json` and `ui-config.json` (nitro-react's
//!   `public/*.json.example`): `catalog.asset.image.url` (`${catalog.asset.url}/%name%.gif`) and
//!   `catalog.asset.icon.url` (`${catalog.asset.url}/icon_%name%.png`), where
//!   `catalog.asset.url` is `${image.library.url}catalogue`; `${key}` is replaced by another
//!   key's value, `%name%` by the image name / icon number.
//! * Flash: `external_variables.txt` lines `key=value`; the client builds page images as
//!   `<image.library.catalogue.url><name>.gif` and icons as
//!   `<image.library.catalogue.url>icon_<n>.png` (HabboCatalog.as, CatalogNodeRenderable.as).
//!
//! The result is in the shape the image download takes: a catalogue folder URL (images are
//! `<folder>/<name>.gif|png`) and an icon prefix (`<prefix><n>.png`).

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// Where the catalogue images are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageUrls {
    /// The catalogue image folder (no trailing slash).
    pub catalog_url: String,
    /// The icon URL prefix (the icon number and `.png` follow).
    pub icon_url: String,
}

/// Replaces every `${key}` with the key's value (repeatedly, for chains).
fn expand(value: &str, keys: &HashMap<String, String>) -> String {
    let mut text = value.to_string();
    for _ in 0..8 {
        let Some(start) = text.find("${") else { break };
        let Some(len) = text[start..].find('}') else { break };
        let key = &text[start + 2..start + len];
        let replacement = keys.get(key).cloned().unwrap_or_default();
        text.replace_range(start..start + len + 1, &replacement);
    }
    text
}

/// The URLs from Nitro's configuration (the keys of renderer-config.json and ui-config.json
/// together).
pub fn from_nitro(configs: &[Value]) -> Result<ImageUrls> {
    let mut keys = HashMap::new();
    for config in configs {
        if let Some(map) = config.as_object() {
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    keys.insert(k.clone(), s.to_string());
                }
            }
        }
    }
    let get = |key: &str| keys.get(key).map(|v| expand(v, &keys));
    let image = get("catalog.asset.image.url").or_else(|| {
        get("image.library.url").map(|lib| format!("{lib}catalogue/%name%.gif"))
    });
    let icon = get("catalog.asset.icon.url").or_else(|| {
        get("image.library.url").map(|lib| format!("{lib}catalogue/icon_%name%.png"))
    });
    let (Some(image), Some(icon)) = (image, icon) else {
        bail!("the Nitro configuration has neither catalog.asset.image.url nor image.library.url");
    };
    let catalog_url = image
        .split_once("/%name%")
        .map(|(base, _)| base.to_string())
        .with_context(|| format!("catalog.asset.image.url is not <folder>/%name%...: {image}"))?;
    let icon_url = icon
        .split_once("%name%")
        .map(|(prefix, _)| prefix.to_string())
        .with_context(|| format!("catalog.asset.icon.url has no %name%: {icon}"))?;
    Ok(ImageUrls {
        catalog_url,
        icon_url,
    })
}

/// The URLs from a Flash client's external_variables.
pub fn from_external_variables(text: &str) -> Result<ImageUrls> {
    let mut keys = HashMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            keys.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let folder = keys
        .get("image.library.catalogue.url")
        .cloned()
        .or_else(|| keys.get("image.library.url").map(|lib| format!("{}/catalogue/", lib.trim_end_matches('/'))))
        .context("the external_variables have neither image.library.catalogue.url nor image.library.url")?;
    let folder = folder.trim_end_matches('/').to_string();
    Ok(ImageUrls {
        icon_url: format!("{folder}/icon_"),
        catalog_url: folder,
    })
}

async fn fetch(url: &str) -> Result<Option<String>> {
    Ok(crate::http::try_get_bytes(url)
        .await?
        .map(|b| String::from_utf8_lossy(&b).into_owned()))
}

/// Reads the URLs from `source`: a Nitro config file (renderer-config.json / ui-config.json;
/// the other one next to it is read too), a site or folder URL holding them, or a Flash
/// external_variables URL.
pub async fn load(source: &str) -> Result<ImageUrls> {
    let source = source.trim();
    let is_file = source.ends_with(".json")
        || source.ends_with(".txt")
        || source.contains("external_variables");
    let (folder, first) = if is_file {
        let folder = source.rsplit_once('/').map(|(b, _)| b.to_string()).unwrap_or_default();
        (folder, Some(source.to_string()))
    } else {
        (source.trim_end_matches('/').to_string(), None)
    };
    let mut configs = Vec::new();
    let mut tried = Vec::new();
    let candidates: Vec<String> = first
        .into_iter()
        .chain(["renderer-config.json", "ui-config.json"].iter().map(|f| format!("{folder}/{f}")))
        .collect();
    for url in candidates {
        if tried.contains(&url) {
            continue;
        }
        tried.push(url.clone());
        let Some(text) = fetch(&url).await? else { continue };
        let trimmed = text.trim_start_matches('\u{feff}').trim_start();
        if trimmed.starts_with('{') {
            configs.push(serde_json::from_str::<Value>(trimmed).with_context(|| format!("reading {url}"))?);
        } else if trimmed.contains('=') {
            return from_external_variables(trimmed);
        }
    }
    if configs.is_empty() {
        bail!("no client configuration at {source} (tried {})", tried.join(", "));
    }
    from_nitro(&configs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nitro_example_configs() {
        let renderer = json!({
            "asset.url": "https://website.com",
            "image.library.url": "https://website.com/c_images/",
        });
        let ui = json!({
            "catalog.asset.url": "${image.library.url}catalogue",
            "catalog.asset.image.url": "${catalog.asset.url}/%name%.gif",
            "catalog.asset.icon.url": "${catalog.asset.url}/icon_%name%.png",
        });
        let urls = from_nitro(&[renderer.clone(), ui]).unwrap();
        assert_eq!(urls.catalog_url, "https://website.com/c_images/catalogue");
        assert_eq!(urls.icon_url, "https://website.com/c_images/catalogue/icon_");
        // Without the ui-config: the default layout under image.library.url.
        let urls = from_nitro(&[renderer]).unwrap();
        assert_eq!(urls.catalog_url, "https://website.com/c_images/catalogue");
        assert!(from_nitro(&[json!({})]).is_err());
    }

    #[test]
    fn flash_external_variables() {
        let text = "image.library.url=https://cdn.x.com/c_images/\nimage.library.catalogue.url=https://cdn.x.com/c_images/catalogue/\n";
        let urls = from_external_variables(text).unwrap();
        assert_eq!(urls.catalog_url, "https://cdn.x.com/c_images/catalogue");
        assert_eq!(urls.icon_url, "https://cdn.x.com/c_images/catalogue/icon_");
        let urls = from_external_variables("image.library.url=https://cdn.y.com/c_images").unwrap();
        assert_eq!(urls.catalog_url, "https://cdn.y.com/c_images/catalogue");
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serves renderer-config.json and ui-config.json like a Nitro site, 404 for the rest.
    async fn site() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let image_library = format!("{base}/c_images/");
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else { return };
                let lib = image_library.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let body = match path.as_str() {
                        "/client/renderer-config.json" => format!(r#"{{"image.library.url": "{lib}"}}"#),
                        "/client/ui-config.json" => r#"{"catalog.asset.url": "${image.library.url}catalogue", "catalog.asset.image.url": "${catalog.asset.url}/%name%.gif", "catalog.asset.icon.url": "${catalog.asset.url}/icon_%name%.png"}"#.to_string(),
                        "/gamedata/external_variables.txt" => format!("image.library.catalogue.url={lib}catalogue/
"),
                        _ => String::new(),
                    };
                    let response = if body.is_empty() {
                        "HTTP/1.1 404 Not Found
Content-Length: 0
Connection: close

".to_string()
                    } else {
                        format!("HTTP/1.1 200 OK
Content-Length: {}
Connection: close

{body}", body.len())
                    };
                    let _ = s.write_all(response.as_bytes()).await;
                });
            }
        });
        base
    }

    #[tokio::test]
    async fn reads_a_nitro_site_and_flash_variables() {
        let base = site().await;
        for source in [format!("{base}/client"), format!("{base}/client/ui-config.json")] {
            let urls = load(&source).await.unwrap();
            assert_eq!(urls.catalog_url, format!("{base}/c_images/catalogue"));
            assert_eq!(urls.icon_url, format!("{base}/c_images/catalogue/icon_"));
        }
        let urls = load(&format!("{base}/gamedata/external_variables.txt")).await.unwrap();
        assert_eq!(urls.icon_url, format!("{base}/c_images/catalogue/icon_"));
        assert!(load(&format!("{base}/nothing")).await.is_err());
    }
}
