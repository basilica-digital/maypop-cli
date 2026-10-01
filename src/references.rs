//! Import released app source as disposable, isolated reference material.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

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

fn ignore_reference_files(repository: &Path) -> Result<()> {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(repository)
            .output()
    };
    let location = git(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "info/exclude",
    ])
    .context("could not locate the app's Git exclude file")?;
    if !location.status.success() {
        if repository.join(".git").exists() {
            bail!("could not locate the app's Git exclude file");
        }
        return crate::project::ignore_maypop_local(repository);
    }
    let prefix = git(&["rev-parse", "--show-prefix"])?;
    if !prefix.status.success() {
        bail!("could not locate the app within its Git repository");
    }
    let prefix = String::from_utf8(prefix.stdout)?;
    let prefix = prefix.strip_suffix('\n').unwrap_or(&prefix);
    if prefix.contains(['\n', '\r']) {
        bail!("the app directory contains a line break");
    }
    let escaped: String = prefix
        .chars()
        .flat_map(|ch| {
            if matches!(ch, '\\' | '*' | '?' | '[') {
                vec!['\\', ch]
            } else {
                vec![ch]
            }
        })
        .collect();
    let pattern = format!("/{escaped}.maypop/local/");
    let exclude = PathBuf::from(String::from_utf8(location.stdout)?.trim_end_matches(['\n', '\r']));
    if std::fs::symlink_metadata(&exclude).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!("the app's Git exclude file is a symbolic link");
    }
    let existing = match std::fs::read_to_string(&exclude) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    if existing.lines().any(|line| line == pattern) {
        return Ok(());
    }
    std::fs::create_dir_all(exclude.parent().context("Git exclude file has no parent")?)?;
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    // Reference snapshots belong to this checkout; importing cannot dirty app-owned ignore files.
    std::fs::write(exclude, format!("{existing}{separator}{pattern}\n"))?;
    Ok(())
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
    ignore_reference_files(&repository)?;
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
mod tests;
