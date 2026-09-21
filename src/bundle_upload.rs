//! Direct upload of a built web application to Maypop's immutable bundle store.

use crate::http;
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Document-routing behavior attached to an uploaded bundle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Routing {
    Spa,
    StaticPages,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestEntry {
    path: String,
    size: u64,
    content_type: String,
}

struct LocalFile {
    manifest: ManifestEntry,
    local_path: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PresignRequest<'a> {
    files: Vec<&'a ManifestEntry>,
    entry: &'a str,
    routing: Routing,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PresignResponse {
    bundle_id: String,
    files: Vec<PresignedFile>,
}

#[derive(Deserialize)]
struct PresignedFile {
    path: String,
    url: String,
    fields: HashMap<String, String>,
}

#[derive(Deserialize)]
struct PresignedUpload {
    url: String,
    fields: HashMap<String, String>,
    cid: String,
}

/// Upload a built directory directly to object storage and confirm its entry point.
pub(crate) async fn upload_directory(
    client: &Client,
    api_url: &str,
    directory: &Path,
    entry: &str,
    routing: Routing,
) -> Result<String> {
    let files = collect_files(directory)?;
    if !files.iter().any(|file| file.manifest.path == entry) {
        bail!(
            "build output {} does not contain its entry point {entry}",
            directory.display()
        );
    }
    println!("Uploading {} build files...", files.len());
    let response = http::json(
        client.post(format!("{}/bundles/presign", trim_url(api_url))),
        &PresignRequest {
            files: files.iter().map(|file| &file.manifest).collect(),
            entry,
            routing,
        },
    )?
    .send()
    .await
    .context("could not prepare the Maypop bundle upload")?;
    let presigned: PresignResponse = successful_json(response).await?;
    let local_paths = files
        .into_iter()
        .map(|file| (file.manifest.path, file.local_path))
        .collect::<HashMap<_, _>>();

    for file in presigned.files {
        let local_path = local_paths
            .get(&file.path)
            .with_context(|| format!("Maypop requested an unknown build file {}", file.path))?;
        upload_file(&file, local_path).await?;
    }

    let response = http::empty(client.post(format!(
        "{}/bundles/{}/confirm",
        trim_url(api_url),
        presigned.bundle_id
    )))
    .send()
    .await
    .context("could not confirm the Maypop bundle")?;
    successful_empty(response).await?;
    Ok(presigned.bundle_id)
}

/// Upload and confirm one image for use as app metadata.
pub(crate) async fn upload_image(client: &Client, api_url: &str, path: &Path) -> Result<String> {
    if !path.is_file() {
        bail!("thumbnail {} is not a file", path.display());
    }
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    if mime.type_().as_str() != "image" {
        bail!("thumbnail {} is not a recognized image", path.display());
    }
    let response = http::empty(client.post(format!("{}/uploads/presign", trim_url(api_url))))
        .send()
        .await
        .context("could not prepare the thumbnail upload")?;
    let presigned: PresignedUpload = successful_json(response).await?;
    upload_to_policy(&presigned.url, &presigned.fields, path, "thumbnail").await?;
    let response = http::empty(client.post(format!(
        "{}/uploads/{}/confirm",
        trim_url(api_url),
        presigned.cid
    )))
    .send()
    .await
    .context("could not confirm the thumbnail upload")?;
    successful_empty(response).await?;
    Ok(presigned.cid)
}

fn collect_files(directory: &Path) -> Result<Vec<LocalFile>> {
    if !directory.is_dir() {
        bail!("build output {} is not a directory", directory.display());
    }
    let mut files = Vec::new();
    let walker = walkdir::WalkDir::new(directory)
        .into_iter()
        .filter_entry(|entry| {
            entry.path() == directory || !is_local_only_path(directory, entry.path())
        });
    for entry in walker {
        let entry = entry.with_context(|| format!("could not scan {}", directory.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(directory)
            .context("build output path escaped its directory")?;
        let path = relative_path(relative)?;
        let size = entry
            .metadata()
            .with_context(|| format!("could not inspect {}", entry.path().display()))?
            .len();
        files.push(LocalFile {
            manifest: ManifestEntry {
                path,
                size,
                content_type: mime_guess::from_path(entry.path())
                    .first_or_octet_stream()
                    .to_string(),
            },
            local_path: entry.path().to_path_buf(),
        });
    }
    files.sort_by(|left, right| left.manifest.path.cmp(&right.manifest.path));
    if files.is_empty() {
        bail!("build output {} has no files", directory.display());
    }
    Ok(files)
}

fn is_local_only_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    let Some(Component::Normal(name)) = relative.components().next() else {
        return false;
    };
    name == ".git"
        || name == ".maypop"
        || name == "node_modules"
        || name == "maypop.toml"
        || name == ".env"
        || name.to_string_lossy().starts_with(".env.")
}

fn relative_path(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            bail!("build output contains an unsafe path: {}", path.display());
        };
        let part = part
            .to_str()
            .with_context(|| format!("build output path is not UTF-8: {}", path.display()))?;
        parts.push(part);
    }
    Ok(parts.join("/"))
}

async fn upload_file(file: &PresignedFile, local_path: &Path) -> Result<()> {
    upload_to_policy(&file.url, &file.fields, local_path, &file.path).await
}

async fn upload_to_policy(
    url: &str,
    fields: &HashMap<String, String>,
    local_path: &Path,
    label: &str,
) -> Result<()> {
    let mime = mime_guess::from_path(local_path)
        .first_or_octet_stream()
        .to_string();
    let mut form = reqwest::multipart::Form::new();
    for (key, value) in fields {
        form = form.text(key.clone(), value.clone());
    }
    form = form.text("Content-Type", mime.clone());
    let bytes = std::fs::read(local_path)
        .with_context(|| format!("could not read {}", local_path.display()))?;
    let name = local_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("build output filename is not UTF-8")?;
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(name.to_string())
        .mime_str(&mime)?;
    // GCS rejects chunked POST policy uploads with 411. HTTP/1.1 preserves
    // reqwest's computed Content-Length for this fully buffered form.
    let response = Client::builder()
        .http1_only()
        .build()?
        .post(url)
        .multipart(form.part("file", part))
        .send()
        .await
        .with_context(|| format!("could not upload {label}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("uploading {label} failed with {status}: {body}");
    }
    Ok(())
}

async fn successful_json<T: for<'de> Deserialize<'de>>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    if status.is_success() {
        return response
            .json()
            .await
            .context("Maypop returned invalid JSON");
    }
    let body = response.text().await.unwrap_or_default();
    bail!("Maypop returned {status}: {body}")
}

async fn successful_empty(response: reqwest::Response) -> Result<()> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    bail!("Maypop returned {status}: {body}")
}

fn trim_url(url: &str) -> &str {
    url.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_control_directories_are_not_part_of_static_bundles() {
        let root = Path::new("/tmp/app");
        assert!(is_local_only_path(root, Path::new("/tmp/app/.git")));
        assert!(is_local_only_path(root, Path::new("/tmp/app/node_modules")));
        assert!(is_local_only_path(root, Path::new("/tmp/app/.env.local")));
        assert!(is_local_only_path(root, Path::new("/tmp/app/maypop.toml")));
        assert!(!is_local_only_path(root, Path::new("/tmp/app/assets")));
    }

    #[test]
    fn relative_paths_use_bundle_separators() {
        assert_eq!(
            relative_path(Path::new("assets/app.js")).unwrap(),
            "assets/app.js"
        );
        assert!(relative_path(Path::new("../secret")).is_err());
    }
}
