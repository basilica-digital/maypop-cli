//! Import released app source as disposable, isolated reference material.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_BYTES: usize = 20 * 1024 * 1024;
const MAX_JSON_BYTES: u64 = (MAX_BYTES * 2) as u64;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    app_id: Uuid,
    name: String,
    revision: i32,
    source_commit_sha: Option<String>,
    files: Vec<ReferenceFile>,
}

#[derive(Deserialize, Serialize)]
struct ReferenceFile {
    path: String,
    content: String,
}

/// Import an authenticated live snapshot, or one the Studio supplies on stdin.
pub(crate) async fn import(
    api_url: &str,
    profile: Option<&str>,
    token: Option<&str>,
    app_id: Uuid,
    repository: &Path,
    stdin: bool,
) -> Result<()> {
    let snapshot = if stdin {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(MAX_JSON_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_JSON_BYTES {
            bail!("reference snapshot is too large");
        }
        serde_json::from_slice(&bytes).context("invalid reference snapshot")?
    } else {
        let token = crate::user_commands::required_token(api_url, profile, token)?;
        let http = crate::user_commands::client(Some(&token))?;
        let response = http
            .get(format!("{api_url}/apps/{app_id}/source/snapshot"))
            .send()
            .await?;
        crate::user_commands::successful_json(response).await?
    };
    let path = import_snapshot(repository, app_id, snapshot)?;
    println!("{}", path.display());
    Ok(())
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path.contains('\\')
        && !path.contains(':')
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| {
            !matches!(
                part,
                "" | "." | ".." | ".git" | ".chat" | ".meta" | ".attachments" | "node_modules"
            ) && part != ".env"
                && (!part.starts_with(".env.") || part == ".env.example")
        })
        && !path.starts_with(".maypop/local/")
        && !path.starts_with(".maypop/publish/")
        && path != ".maypop-reference.json"
}

fn decoded_files(snapshot: &Snapshot) -> Result<BTreeMap<String, Vec<u8>>> {
    if snapshot.files.is_empty() || snapshot.files.len() > 1000 {
        bail!("reference must contain between 1 and 1000 files");
    }
    let mut files = BTreeMap::new();
    let mut total = 0;
    for file in &snapshot.files {
        if !safe_path(&file.path) {
            bail!("unsafe reference path: {}", file.path);
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&file.content)
            .context("invalid base64 in reference file")?;
        total += bytes.len();
        if total > MAX_BYTES {
            bail!("reference source exceeds 20 MB");
        }
        if files.insert(file.path.clone(), bytes).is_some() {
            bail!("duplicate reference path: {}", file.path);
        }
    }
    if snapshot.source_commit_sha.is_none() {
        if let Some(embedded) = files
            .get("index.html")
            .and_then(|bytes| embedded_files(bytes))
        {
            files = embedded
                .into_iter()
                .map(|(path, text)| -> Result<_> {
                    let extension = Path::new(&path)
                        .extension()
                        .and_then(|value| value.to_str())
                        .unwrap_or("");
                    let text_file = matches!(
                        extension,
                        "html"
                            | "htm"
                            | "js"
                            | "jsx"
                            | "ts"
                            | "tsx"
                            | "json"
                            | "css"
                            | "svg"
                            | "txt"
                            | "md"
                            | "csv"
                            | "toml"
                            | "yaml"
                            | "yml"
                    );
                    let bytes = if !text_file && text.starts_with("data:") {
                        if let Some((_, data)) = text
                            .split_once(',')
                            .filter(|(header, _)| header.ends_with(";base64"))
                        {
                            base64::engine::general_purpose::STANDARD
                                .decode(data)
                                .context("invalid embedded binary asset")?
                        } else {
                            text.into_bytes()
                        }
                    } else {
                        text.into_bytes()
                    };
                    Ok((path, bytes))
                })
                .collect::<Result<_>>()?;
        }
    }
    let paths: HashSet<&str> = files.keys().map(String::as_str).collect();
    let mut total = 0;
    for (path, bytes) in &files {
        total += bytes.len();
        if !safe_path(path) || total > MAX_BYTES || files.len() > 1000 {
            bail!("unsafe or oversized embedded reference source");
        }
        for (index, _) in path.match_indices('/') {
            if paths.contains(&path[..index]) {
                bail!(
                    "reference path is both a file and directory: {}",
                    &path[..index]
                );
            }
        }
    }
    Ok(files)
}

fn embedded_files(bytes: &[u8]) -> Option<BTreeMap<String, String>> {
    let html = std::str::from_utf8(bytes).ok()?;
    for script in html.split("<script").skip(1) {
        let (attributes, body) = script.split_once('>')?;
        if !attributes.contains("id=\"__studio-workspace-files__\"")
            && !attributes.contains("id='__studio-workspace-files__'")
        {
            continue;
        }
        let json = body.split_once("</script>")?.0;
        let files: BTreeMap<String, String> = serde_json::from_str(json).ok()?;
        return files.contains_key("index.html").then_some(files);
    }
    None
}

fn ensure_directory(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => bail!(
            "reference destination is not an ordinary directory: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(path)
            .with_context(|| format!("could not create {}", path.display())),
        Err(error) => Err(error.into()),
    }
}

fn import_snapshot(repository: &Path, app_id: Uuid, snapshot: Snapshot) -> Result<PathBuf> {
    if snapshot.app_id != app_id || snapshot.revision < 1 {
        bail!("reference snapshot does not match the requested app release");
    }
    let files = decoded_files(&snapshot)?;
    let repository = repository
        .canonicalize()
        .context("destination app directory does not exist")?;
    let mut directory = repository.clone();
    for part in [".maypop", "local", "references", &app_id.to_string()] {
        directory.push(part);
        ensure_directory(&directory)?;
    }
    let ignore = repository.join(".maypop/.gitignore");
    if std::fs::symlink_metadata(&ignore).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!(".maypop/.gitignore is a symbolic link");
    }
    crate::project::ignore_maypop_local(&repository)?;
    let temporary = directory.join(format!(".import-{}", Uuid::new_v4()));
    std::fs::create_dir(&temporary)?;
    let result = (|| -> Result<PathBuf> {
        for (path, bytes) in files {
            let target = temporary.join(path);
            std::fs::create_dir_all(target.parent().context("missing reference parent")?)?;
            std::fs::write(target, bytes)?;
        }
        std::fs::write(
            temporary.join(".maypop-reference.json"),
            serde_json::to_vec(&serde_json::json!({
                "appId": snapshot.app_id, "name": snapshot.name,
                "revision": snapshot.revision, "sourceCommitSha": snapshot.source_commit_sha,
            }))?,
        )?;
        // Each import has its own tree so queued prompts retain the release they attached.
        // TODO: Reuse verified immutable snapshots to bound repeated-reference disk usage.
        let destination = directory.join(format!("{}-{}", snapshot.revision, Uuid::new_v4()));
        std::fs::rename(&temporary, &destination)?;
        Ok(destination.strip_prefix(repository)?.to_path_buf())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(app_id: Uuid, files: &[(&str, &[u8])]) -> Snapshot {
        Snapshot {
            app_id,
            name: "Reference".into(),
            revision: 2,
            source_commit_sha: Some("a".repeat(40)),
            files: files
                .iter()
                .map(|(path, bytes)| ReferenceFile {
                    path: path.to_string(),
                    content: base64::engine::general_purpose::STANDARD.encode(bytes),
                })
                .collect(),
        }
    }

    #[test]
    fn source_import_rejects_escaping_private_and_conflicting_paths() {
        let id = Uuid::new_v4();
        for path in [
            "../secret",
            "/tmp/secret",
            "a\\b",
            ".chat/session.json",
            ".maypop/local/key",
            ".env.production",
        ] {
            assert!(
                decoded_files(&snapshot(id, &[(path, b"x")])).is_err(),
                "{path}"
            );
        }
        assert!(decoded_files(&snapshot(id, &[("src", b"x"), ("src/App.tsx", b"y")])).is_err());
        assert!(decoded_files(&snapshot(id, &[("src.ts", b"x"), ("src.ts", b"y")])).is_err());
    }

    #[test]
    fn legacy_import_reads_editable_embedded_source_and_rejects_unsafe_paths() {
        let id = Uuid::new_v4();
        let html = br#"<script type="application/json" id="__studio-workspace-files__">{"index.html":"original","src/App.tsx":"component","logo.png":"data:image/png;base64,AP8="}</script>"#;
        let mut source = snapshot(id, &[("index.html", html)]);
        source.source_commit_sha = None;
        let files = decoded_files(&source).unwrap();
        assert_eq!(files["src/App.tsx"], b"component");
        assert_eq!(files["index.html"], b"original");
        assert_eq!(files["logo.png"], [0, 255]);
        source.files[0].content = base64::engine::general_purpose::STANDARD.encode(
            br#"<script id="__studio-workspace-files__">{"index.html":"ok","../escape":"bad"}</script>"#);
        assert!(decoded_files(&source).is_err());
    }

    #[test]
    fn importing_binary_source_keeps_each_snapshot_separate_and_git_ignored() {
        let root = std::env::temp_dir().join(format!("maypop-reference-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let id = Uuid::new_v4();
        let first = import_snapshot(&root, id, snapshot(id, &[("image.png", &[0, 255])])).unwrap();
        let second = import_snapshot(&root, id, snapshot(id, &[("src.ts", b"changed")])).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            std::fs::read(root.join(first).join("image.png")).unwrap(),
            [0, 255]
        );
        assert!(std::fs::read_to_string(root.join(".maypop/.gitignore"))
            .unwrap()
            .contains("/local/"));
        assert!(import_snapshot(&root, Uuid::new_v4(), snapshot(id, &[("x", b"x")])).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn importing_never_follows_a_link_out_of_the_app() {
        let root = std::env::temp_dir().join(format!("maypop-reference-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("outside")).unwrap();
        std::fs::create_dir_all(root.join("app/.maypop")).unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("app/.maypop/local")).unwrap();
        let id = Uuid::new_v4();
        assert!(import_snapshot(&root.join("app"), id, snapshot(id, &[("src.ts", b"x")])).is_err());
        assert_eq!(std::fs::read_dir(root.join("outside")).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}
