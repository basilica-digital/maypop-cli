//! Owner and app-admin access to one app's KV entries and Drive files.
//!
//! Writes and deletes print the change and do nothing until `--yes`. Each
//! one names a single key or file.

use crate::bundle_upload::upload_confirmed_file;
use crate::credentials::{self, api_urls_match};
use crate::http::{self, failure};
use crate::user_commands::{client, connected_checkout, required_token};
use anyhow::{anyhow, bail, Context, Result};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde_json::{json, Value};
use std::path::Path;

const PAGE_MAX: u32 = 100;
const KEY_MAX: usize = 1024;
const NAME_MAX: usize = 512;
const FORBIDDEN: &str = "Only the app's owner or an app admin can inspect or change this app's data, and credentials held by a Studio sandbox are read-only.";

pub(crate) struct Session {
    app_id: String,
    api_url: String,
    http: Client,
}

#[derive(Debug)]
pub(crate) enum KvSetOutcome {
    DryRun {
        new_entry: bool,
        old_value: Option<Value>,
        new_value: Value,
    },
    Applied(Value),
}

#[derive(Debug)]
pub(crate) enum KvRemoveOutcome {
    DryRun(Value),
    Applied,
}

#[derive(Debug)]
pub(crate) enum DrivePutOutcome {
    DryRun {
        name: String,
        bytes: u64,
        new_file: bool,
        existing: Option<Value>,
    },
    Applied(Value),
}

#[derive(Debug)]
pub(crate) enum DriveRemoveOutcome {
    DryRun(Value),
    Applied,
}

/// Resolve the app and the saved credential. `--app` wins over the checkout.
pub(crate) fn open_session(
    profile: Option<&str>,
    explicit_url: Option<&str>,
    explicit_token: Option<&str>,
    app: Option<uuid::Uuid>,
) -> Result<Session> {
    let (app_id, api_url) = match connected_checkout()? {
        Some((app_id, api_url)) => {
            if let Some(explicit_url) = explicit_url {
                if !api_urls_match(&api_url, explicit_url) {
                    bail!("--url does not match this repository's Maypop server ({api_url})");
                }
            }
            (app.map(|id| id.to_string()).unwrap_or(app_id), api_url)
        }
        None => {
            let Some(app) = app else {
                bail!(
                    "pass --app, or run this inside an app repository created with `maypop init`"
                );
            };
            let api_url = match explicit_url {
                Some(url) => credentials::api_origin(url)?,
                None => {
                    let profile = profile
                        .context("could not resolve a Maypop profile; pass --profile or --url")?;
                    credentials::api_url_for(profile, None)?
                }
            };
            (app.to_string(), api_url)
        }
    };
    if app_id.is_empty() {
        bail!("this repository's app id is empty; pass --app");
    }
    let token = required_token(&api_url, profile, explicit_token)?;
    Ok(Session {
        app_id,
        api_url,
        http: client(Some(&token))?,
    })
}

pub(crate) async fn kv_ls(
    session: &Session,
    json_output: bool,
    prefix: Option<&str>,
    limit: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value> {
    let mut request = session.http.get(resource(session, "kv"));
    let query = page_query(prefix, limit, cursor)?;
    if !query.is_empty() {
        request = request.query(&query);
    }
    let page = get_json(
        request,
        "This app was not found, or this account has no access to it.",
    )
    .await?;
    print_kv_page(&page, json_output)?;
    Ok(page)
}

pub(crate) async fn kv_get(session: &Session, json_output: bool, key: &str) -> Result<()> {
    let entry = require_kv(session, key).await?;
    if json_output {
        print_json(&entry)?;
    } else {
        let value = entry.get("value").unwrap_or(&Value::Null);
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

pub(crate) async fn kv_set(
    session: &Session,
    json_output: bool,
    key: &str,
    raw_value: &str,
    yes: bool,
) -> Result<KvSetOutcome> {
    require_key(key)?;
    let new_value = parse_kv_value(raw_value)?;
    let current = read_kv(session, key).await?;
    let outcome = if yes {
        let applied = mutate_kv(session, key, Some(&new_value)).await?;
        KvSetOutcome::Applied(applied)
    } else {
        KvSetOutcome::DryRun {
            new_entry: current.is_none(),
            old_value: current
                .as_ref()
                .and_then(|entry| entry.get("value").cloned()),
            new_value,
        }
    };
    print_kv_set(key, &outcome, json_output)?;
    Ok(outcome)
}

pub(crate) async fn kv_rm(
    session: &Session,
    json_output: bool,
    key: &str,
    yes: bool,
) -> Result<KvRemoveOutcome> {
    let entry = require_kv(session, key).await?;
    let outcome = if yes {
        mutate_kv(session, key, None).await?;
        KvRemoveOutcome::Applied
    } else {
        KvRemoveOutcome::DryRun(entry)
    };
    print_kv_rm(key, &outcome, json_output)?;
    Ok(outcome)
}

pub(crate) async fn drive_ls(
    session: &Session,
    json_output: bool,
    prefix: Option<&str>,
    limit: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value> {
    let mut request = session.http.get(resource(session, "drive"));
    let query = page_query(prefix, limit, cursor)?;
    if !query.is_empty() {
        request = request.query(&query);
    }
    let page = get_json(
        request,
        "This app was not found, or this account has no access to it.",
    )
    .await?;
    print_drive_page(&page, json_output)?;
    Ok(page)
}

pub(crate) async fn drive_get(
    session: &Session,
    json_output: bool,
    name: &str,
    output: Option<&Path>,
) -> Result<()> {
    let file = require_drive(session, name).await?;
    if json_output && output.is_none() {
        return print_json(&file);
    }
    let url = file
        .get("url")
        .and_then(Value::as_str)
        .context("Maypop did not return a download URL")?;
    let bytes = download_drive_bytes(url).await?;
    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("could not create {}", parent.display()))?;
            }
        }
        std::fs::write(path, &bytes)
            .with_context(|| format!("could not write {}", path.display()))?;
        if json_output {
            print_json(&file)?;
        } else {
            println!("Wrote {} bytes to {}", bytes.len(), path.display());
        }
    } else {
        std::io::Write::write_all(&mut std::io::stdout().lock(), &bytes)?;
    }
    Ok(())
}

pub(crate) async fn drive_put(
    session: &Session,
    json_output: bool,
    file: &Path,
    name: Option<&str>,
    yes: bool,
) -> Result<DrivePutOutcome> {
    let metadata =
        std::fs::metadata(file).with_context(|| format!("could not read {}", file.display()))?;
    if !metadata.is_file() {
        bail!("{} is not a file", file.display());
    }
    let name = drive_name(file, name)?;
    let existing = read_drive(session, &name).await?;
    let outcome = if yes {
        let cid =
            upload_confirmed_file(&session.http, &session.api_url, file, "Drive file").await?;
        let applied = put_drive(session, &name, &cid).await?;
        DrivePutOutcome::Applied(applied)
    } else {
        DrivePutOutcome::DryRun {
            name: name.clone(),
            bytes: metadata.len(),
            new_file: existing.is_none(),
            existing,
        }
    };
    print_drive_put(file, &outcome, json_output)?;
    Ok(outcome)
}

pub(crate) async fn drive_rm(
    session: &Session,
    json_output: bool,
    name: &str,
    yes: bool,
) -> Result<DriveRemoveOutcome> {
    require_drive_name(name)?;
    let file = require_drive(session, name).await?;
    let outcome = if yes {
        delete_drive(session, name).await?;
        DriveRemoveOutcome::Applied
    } else {
        DriveRemoveOutcome::DryRun(file)
    };
    print_drive_rm(name, &outcome, json_output)?;
    Ok(outcome)
}

fn require_key(key: &str) -> Result<()> {
    if key.is_empty() {
        bail!("key is empty");
    }
    if key.len() > KEY_MAX {
        bail!("key is too long");
    }
    Ok(())
}

/// Same name rules as the owner Drive route.
fn require_drive_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= NAME_MAX
        && !name.chars().any(char::is_control)
        && !name.contains('\\')
        && name
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    if ok {
        Ok(())
    } else {
        bail!("invalid Drive file name {name:?}")
    }
}

fn drive_name(file: &Path, name: Option<&str>) -> Result<String> {
    let name = match name {
        Some(name) => name.to_string(),
        None => file
            .file_name()
            .and_then(|name| name.to_str())
            .context("the file name is not UTF-8; pass a Drive name")?
            .to_string(),
    };
    require_drive_name(&name)?;
    Ok(name)
}

fn parse_kv_value(raw: &str) -> Result<Value> {
    let text = if let Some(path) = raw.strip_prefix('@') {
        if path.is_empty() {
            bail!("write the JSON value, or @file to read it from a file");
        }
        std::fs::read_to_string(path).with_context(|| format!("could not read {path}"))?
    } else {
        raw.to_string()
    };
    serde_json::from_str(text.trim()).context("the value must be JSON, or @file containing JSON")
}

fn page_query(
    prefix: Option<&str>,
    limit: Option<u32>,
    cursor: Option<&str>,
) -> Result<Vec<(&'static str, String)>> {
    if let Some(limit) = limit {
        if !(1..=PAGE_MAX).contains(&limit) {
            bail!("--limit must be from 1 to 100");
        }
    }
    let mut pairs = Vec::new();
    if let Some(prefix) = present(prefix) {
        pairs.push(("prefix", prefix.to_string()));
    }
    if let Some(limit) = limit {
        pairs.push(("limit", limit.to_string()));
    }
    if let Some(cursor) = present(cursor) {
        pairs.push(("cursor", cursor.to_string()));
    }
    Ok(pairs)
}

fn present(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

fn resource(session: &Session, suffix: &str) -> String {
    format!(
        "{}/apps/{}/data/{suffix}",
        trim_url(&session.api_url),
        session.app_id
    )
}

fn trim_url(url: &str) -> &str {
    url.trim_end_matches('/')
}

async fn require_kv(session: &Session, key: &str) -> Result<Value> {
    require_key(key)?;
    read_kv(session, key)
        .await?
        .with_context(|| kv_missing(key))
}

async fn read_kv(session: &Session, key: &str) -> Result<Option<Value>> {
    get_optional(
        session
            .http
            .get(resource(session, "kv/entry"))
            .query(&[("key", key)]),
    )
    .await
}

async fn mutate_kv(session: &Session, key: &str, value: Option<&Value>) -> Result<Value> {
    let url = resource(session, "kv/entry");
    let request = match value {
        Some(value) => http::json(
            session.http.put(&url).query(&[("key", key)]),
            &json!({ "value": value }),
        )?,
        None => http::empty(session.http.delete(&url).query(&[("key", key)])),
    };
    get_json(request, &kv_missing(key)).await
}

fn kv_missing(key: &str) -> String {
    format!("KV entry `{key}` was not found. An account the app grants nothing also gets 404.")
}

async fn require_drive(session: &Session, name: &str) -> Result<Value> {
    require_drive_name(name)?;
    read_drive(session, name)
        .await?
        .with_context(|| drive_missing(name))
}

async fn read_drive(session: &Session, name: &str) -> Result<Option<Value>> {
    get_optional(
        session
            .http
            .get(resource(session, "drive/file"))
            .query(&[("name", name)]),
    )
    .await
}

async fn put_drive(session: &Session, name: &str, cid: &str) -> Result<Value> {
    let request = http::json(
        session.http.put(resource(session, "drive/file")),
        &json!({ "name": name, "cid": cid }),
    )?;
    get_json(request, &drive_missing(name)).await
}

async fn delete_drive(session: &Session, name: &str) -> Result<()> {
    let request = http::empty(
        session
            .http
            .delete(resource(session, "drive/file"))
            .query(&[("name", name)]),
    );
    get_json(request, &drive_missing(name)).await?;
    Ok(())
}

fn drive_missing(name: &str) -> String {
    format!("Drive file `{name}` was not found. An account the app grants nothing also gets 404.")
}

/// Download with a fresh client so the saved credential is not sent to object storage.
async fn download_drive_bytes(url: &str) -> Result<Vec<u8>> {
    let response = Client::new()
        .get(url)
        .send()
        .await
        .context("could not download the Drive file")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("downloading the Drive file failed with {status}: {body}");
    }
    response
        .bytes()
        .await
        .context("could not read the Drive file")
        .map(|bytes| bytes.to_vec())
}

async fn get_optional(request: RequestBuilder) -> Result<Option<Value>> {
    let (status, body) = send(request).await?;
    if status == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    success_value(status, &body, "").map(Some)
}

async fn get_json(request: RequestBuilder, not_found: &str) -> Result<Value> {
    let (status, body) = send(request).await?;
    success_value(status, &body, not_found)
}

async fn send(request: RequestBuilder) -> Result<(StatusCode, String)> {
    let response = request.send().await.context("could not reach Maypop")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Ok((status, body))
}

fn success_value(status: StatusCode, body: &str, not_found: &str) -> Result<Value> {
    if status.is_success() {
        if body.trim().is_empty() {
            return Ok(Value::Null);
        }
        return serde_json::from_str(body).context("Maypop returned invalid JSON");
    }
    Err(explain(status, body, not_found))
}

fn explain(status: StatusCode, body: &str, not_found: &str) -> anyhow::Error {
    if status == StatusCode::FORBIDDEN {
        anyhow!(FORBIDDEN)
    } else if status == StatusCode::NOT_FOUND {
        anyhow!("{not_found}")
    } else {
        failure(status, body)
    }
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_kv_page(page: &Value, json_output: bool) -> Result<()> {
    if json_output {
        return print_json(page);
    }
    let entries = array_field(page, "entries", "KV page")?;
    if entries.is_empty() {
        println!("No KV entries.");
    }
    for entry in entries {
        println!(
            "{}\t{} bytes\tauthor {}\tupdated {}",
            text_field(entry, "key")?,
            number_field(entry, "size")?,
            text_field(entry, "authorPseudonymousMemberId")?,
            text_field(entry, "updatedAt")?
        );
    }
    print_cursor(page);
    Ok(())
}

fn print_drive_page(page: &Value, json_output: bool) -> Result<()> {
    if json_output {
        return print_json(page);
    }
    let files = array_field(page, "files", "Drive page")?;
    if files.is_empty() {
        println!("No Drive files.");
    }
    for file in files {
        println!(
            "{}\t{}\tauthor {}\tcreated {}",
            text_field(file, "name")?,
            file_size(file),
            text_field(file, "authorPseudonymousMemberId")?,
            text_field(file, "createdAt")?
        );
    }
    print_cursor(page);
    Ok(())
}

fn print_cursor(page: &Value) {
    if let Some(cursor) = page.get("nextCursor").and_then(Value::as_str) {
        println!("Next cursor: {cursor}");
    }
}

fn print_kv_set(key: &str, outcome: &KvSetOutcome, json_output: bool) -> Result<()> {
    match outcome {
        KvSetOutcome::DryRun {
            new_entry,
            old_value,
            new_value,
        } => {
            let machine = if *new_entry {
                json!({
                    "dryRun": true,
                    "action": "set",
                    "key": key,
                    "newEntry": true,
                    "newValue": new_value,
                })
            } else {
                json!({
                    "dryRun": true,
                    "action": "set",
                    "key": key,
                    "newEntry": false,
                    "oldValue": old_value,
                    "newValue": new_value,
                })
            };
            if json_output {
                return print_json(&machine);
            }
            let new_text = serde_json::to_string_pretty(new_value)?;
            if *new_entry {
                println!("Would create KV entry `{key}` (new entry).");
                println!("Value: {new_text}");
            } else {
                let old_text =
                    serde_json::to_string_pretty(old_value.as_ref().unwrap_or(&Value::Null))?;
                println!("Would update KV entry `{key}`.");
                println!("Old: {old_text}");
                println!("New: {new_text}");
            }
            println!("Run again with --yes to apply this change.");
        }
        KvSetOutcome::Applied(entry) => {
            if json_output {
                print_json(entry)?;
            } else {
                println!(
                    "Set KV entry `{key}` ({} bytes).",
                    entry.get("size").and_then(Value::as_i64).unwrap_or(0)
                );
            }
        }
    }
    Ok(())
}

fn print_kv_rm(key: &str, outcome: &KvRemoveOutcome, json_output: bool) -> Result<()> {
    match outcome {
        KvRemoveOutcome::DryRun(entry) => {
            let machine = json!({
                "dryRun": true,
                "action": "delete",
                "key": key,
                "entry": entry,
            });
            if json_output {
                return print_json(&machine);
            }
            let value = serde_json::to_string_pretty(entry.get("value").unwrap_or(&Value::Null))?;
            println!(
                "Would delete KV entry `{key}` ({} bytes).",
                entry.get("size").and_then(Value::as_i64).unwrap_or(0)
            );
            println!("Value: {value}");
            println!("Run again with --yes to apply this change.");
        }
        KvRemoveOutcome::Applied => {
            if json_output {
                print_json(&json!({ "deleted": true, "key": key }))?;
            } else {
                println!("Deleted KV entry `{key}`.");
            }
        }
    }
    Ok(())
}

fn print_drive_put(file: &Path, outcome: &DrivePutOutcome, json_output: bool) -> Result<()> {
    match outcome {
        DrivePutOutcome::DryRun {
            name,
            bytes,
            new_file,
            existing,
        } => {
            let machine = json!({
                "dryRun": true,
                "action": "put",
                "name": name,
                "localFile": file,
                "bytes": bytes,
                "newFile": new_file,
                "existing": existing,
            });
            if json_output {
                return print_json(&machine);
            }
            println!(
                "Would file {} as Drive name `{name}` ({bytes} bytes).",
                file.display()
            );
            if let Some(existing) = existing {
                println!("Replaces the current file ({}).", describe_file(existing));
            } else {
                println!("New file.");
            }
            println!("Run again with --yes to apply this change.");
        }
        DrivePutOutcome::Applied(file) => {
            if json_output {
                print_json(file)?;
            } else {
                println!(
                    "Filed Drive name `{}` ({}).",
                    text_field(file, "name")?,
                    describe_file(file)
                );
            }
        }
    }
    Ok(())
}

fn print_drive_rm(name: &str, outcome: &DriveRemoveOutcome, json_output: bool) -> Result<()> {
    match outcome {
        DriveRemoveOutcome::DryRun(file) => {
            let machine = json!({
                "dryRun": true,
                "action": "delete",
                "name": name,
                "file": file,
            });
            if json_output {
                return print_json(&machine);
            }
            println!(
                "Would delete Drive file `{name}` ({}). Every live row with that exact name is removed.",
                describe_file(file)
            );
            println!("Run again with --yes to apply this change.");
        }
        DriveRemoveOutcome::Applied => {
            if json_output {
                print_json(&json!({ "deleted": true, "name": name }))?;
            } else {
                println!("Deleted Drive file `{name}`.");
            }
        }
    }
    Ok(())
}

fn array_field<'a>(value: &'a Value, key: &str, kind: &str) -> Result<&'a Vec<Value>> {
    value
        .get(key)
        .and_then(Value::as_array)
        .with_context(|| format!("Maypop returned an unexpected {kind}"))
}

fn text_field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("Maypop response is missing {key}"))
}

fn number_field(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .with_context(|| format!("Maypop response is missing {key}"))
}

fn file_size(file: &Value) -> String {
    match file.get("size").and_then(Value::as_i64) {
        Some(size) => format!("{size} bytes"),
        None => "unknown size".to_string(),
    }
}

fn describe_file(file: &Value) -> String {
    let mime = file
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("unknown type");
    match file.get("size").and_then(Value::as_i64) {
        Some(size) => format!("{size} bytes, {mime}"),
        None => mime.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const APP: &str = "00000000-0000-0000-0000-0000000000aa";

    struct Hit {
        method: String,
        target: String,
        body: Vec<u8>,
    }

    fn session(api_url: &str) -> Session {
        Session {
            app_id: APP.into(),
            api_url: api_url.into(),
            http: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        }
    }

    fn serve<F>(handler: F) -> (String, Arc<Mutex<Vec<Hit>>>)
    where
        F: Fn(&Hit) -> (u16, String) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&hits);
        std::thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                let hit = read_hit(stream.try_clone().unwrap());
                let (status, body) = handler(&hit);
                recorded.lock().unwrap().push(hit);
                write_response(stream, status, &body);
            }
        });
        (url, hits)
    }

    fn read_hit(stream: std::net::TcpStream) -> Hit {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut reader = BufReader::new(stream);
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        let mut parts = request.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("").to_string();
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap_or(0);
            }
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        let mut body = vec![0; length];
        if length > 0 {
            reader.read_exact(&mut body).unwrap();
        }
        Hit {
            method,
            target,
            body,
        }
    }

    fn write_response(mut stream: std::net::TcpStream, status: u16, body: &str) {
        let text = match status {
            200 => "200 OK",
            204 => "204 No Content",
            403 => "403 Forbidden",
            404 => "404 Not Found",
            _ => "500 Internal Server Error",
        };
        let header = format!(
            "HTTP/1.1 {text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(body.as_bytes());
    }

    fn paths(hits: &Mutex<Vec<Hit>>) -> Vec<String> {
        hits.lock()
            .unwrap()
            .iter()
            .map(|hit| format!("{} {}", hit.method, hit.target))
            .collect()
    }

    fn entry(color: &str) -> String {
        format!(
            r#"{{"key":"theme","value":{{"color":"{color}"}},"size":18,"authorPseudonymousMemberId":"00000000-0000-0000-0000-000000000002","updatedAt":"2026-01-01T00:00:00Z"}}"#
        )
    }

    /// Guards a dry run sending the PUT, and `--yes` leaving the stored value unchanged.
    #[tokio::test]
    async fn kv_set_dry_run_does_not_write_and_yes_does() {
        let (api, hits) = serve(|hit| {
            if hit.method == "GET" && hit.target.contains("key=fresh") {
                return (404, r#"{"code":"not_found"}"#.into());
            }
            if hit.method == "GET" {
                return (200, entry("green"));
            }
            if hit.method == "PUT" {
                return (200, entry("blue"));
            }
            (500, "unexpected".into())
        });
        let session = session(&api);

        let dry = kv_set(&session, false, "theme", r#"{"color":"blue"}"#, false)
            .await
            .unwrap();
        match dry {
            KvSetOutcome::DryRun {
                new_entry: false,
                old_value,
                new_value,
            } => {
                assert_eq!(old_value.unwrap()["color"], "green");
                assert_eq!(new_value["color"], "blue");
            }
            other => panic!("dry run applied the write: {other:?}"),
        }
        assert_eq!(
            paths(&hits),
            vec![format!("GET /apps/{APP}/data/kv/entry?key=theme")]
        );

        hits.lock().unwrap().clear();
        let file = std::env::temp_dir().join(format!("maypop-kv-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&file, r#"{"color":"blue"}"#).unwrap();
        let fresh = kv_set(
            &session,
            false,
            "fresh",
            &format!("@{}", file.display()),
            false,
        )
        .await
        .unwrap();
        let _ = std::fs::remove_file(&file);
        assert!(matches!(
            fresh,
            KvSetOutcome::DryRun {
                new_entry: true,
                old_value: None,
                ..
            }
        ));
        assert_eq!(
            paths(&hits),
            vec![format!("GET /apps/{APP}/data/kv/entry?key=fresh")]
        );

        hits.lock().unwrap().clear();
        let applied = kv_set(&session, false, "theme", r#"{"color":"blue"}"#, true)
            .await
            .unwrap();
        assert!(matches!(applied, KvSetOutcome::Applied(_)));
        let recorded = hits.lock().unwrap();
        assert_eq!(recorded.len(), 2, "yes reads the old value, then writes");
        assert!(recorded[0].target.contains("key=theme"));
        assert_eq!(recorded[1].method, "PUT");
        let body: Value = serde_json::from_slice(&recorded[1].body).unwrap();
        assert_eq!(body, json!({ "value": { "color": "blue" } }));
    }

    /// Guards `--yes` on delete being ignored, and a dry run sending DELETE.
    #[tokio::test]
    async fn kv_rm_dry_run_does_not_delete_and_yes_does() {
        let (api, hits) = serve(|hit| match hit.method.as_str() {
            "GET" => (200, entry("green")),
            "DELETE" => (204, String::new()),
            _ => (500, "unexpected".into()),
        });
        let session = session(&api);

        let dry = kv_rm(&session, false, "theme", false).await.unwrap();
        assert!(matches!(dry, KvRemoveOutcome::DryRun(_)));
        assert_eq!(paths(&hits).len(), 1);
        assert!(paths(&hits)[0].starts_with("GET "));

        hits.lock().unwrap().clear();
        let applied = kv_rm(&session, false, "theme", true).await.unwrap();
        assert!(matches!(applied, KvRemoveOutcome::Applied));
        let recorded = paths(&hits);
        assert_eq!(recorded.len(), 2);
        assert!(recorded[0].starts_with("GET "));
        assert!(recorded[1].starts_with("DELETE "));
        assert!(recorded[1].contains("key=theme"));
    }

    /// Guards the next page being fetched without the cursor from the previous page.
    #[tokio::test]
    async fn kv_list_sends_the_cursor_for_the_next_page() {
        let (api, hits) = serve(|hit| {
            if hit.target.contains("cursor=a") {
                (
                    200,
                    r#"{"entries":[{"key":"b","size":1,"authorPseudonymousMemberId":"00000000-0000-0000-0000-000000000002","updatedAt":"2026-01-02T00:00:00Z"}]}"#
                        .into(),
                )
            } else {
                (
                    200,
                    r#"{"entries":[{"key":"a","size":1,"authorPseudonymousMemberId":"00000000-0000-0000-0000-000000000002","updatedAt":"2026-01-01T00:00:00Z"}],"nextCursor":"a"}"#
                        .into(),
                )
            }
        });
        let session = session(&api);
        let first = kv_ls(&session, true, Some("p"), Some(1), None)
            .await
            .unwrap();
        assert_eq!(first["nextCursor"], "a");
        let cursor = first["nextCursor"].as_str().unwrap();
        let second = kv_ls(&session, true, Some("p"), Some(1), Some(cursor))
            .await
            .unwrap();
        assert!(second.get("nextCursor").is_none());
        assert_eq!(second["entries"][0]["key"], "b");

        let recorded = paths(&hits);
        assert_eq!(recorded.len(), 2, "the command requests one page at a time");
        assert!(recorded[0].contains("prefix=p"));
        assert!(recorded[0].contains("limit=1"));
        assert!(!recorded[0].contains("cursor="));
        assert!(recorded[1].contains("prefix=p"));
        assert!(recorded[1].contains("cursor=a"));
        assert!(recorded.iter().all(|hit| hit.starts_with("GET ")));
    }

    /// Guards drive put uploading or filing before `--yes`, and `--yes` filing an unconfirmed cid.
    #[tokio::test]
    async fn drive_put_dry_run_does_not_upload_and_yes_files_the_confirmed_cid() {
        let (api, hits) = serve_drive();
        let session = session(&api);
        let file = std::env::temp_dir().join(format!("maypop-drive-{}", uuid::Uuid::new_v4()));
        std::fs::write(&file, b"hello").unwrap();

        let dry = drive_put(&session, false, &file, Some("notes/hello.txt"), false)
            .await
            .unwrap();
        match dry {
            DrivePutOutcome::DryRun {
                name,
                bytes: 5,
                new_file: false,
                existing: Some(existing),
            } => {
                assert_eq!(name, "notes/hello.txt");
                assert_eq!(existing["size"], 4);
            }
            other => panic!("dry run filed the upload: {other:?}"),
        }
        assert_eq!(paths(&hits).len(), 1);
        assert!(paths(&hits)[0].starts_with("GET "));
        assert!(!paths(&hits)[0].contains("/uploads"));

        hits.lock().unwrap().clear();
        let applied = drive_put(&session, false, &file, Some("notes/hello.txt"), true)
            .await
            .unwrap();
        let _ = std::fs::remove_file(&file);
        assert!(matches!(applied, DrivePutOutcome::Applied(_)));
        let recorded = hits.lock().unwrap();
        let targets: Vec<&str> = recorded.iter().map(|hit| hit.target.as_str()).collect();
        assert!(
            targets
                .iter()
                .any(|target| target.starts_with("/uploads/presign")),
            "yes confirms an upload: {targets:?}"
        );
        assert!(
            targets.contains(&"/gcs"),
            "yes uploads the bytes: {targets:?}"
        );
        assert!(
            targets.contains(&"/uploads/cid-1/confirm"),
            "yes confirms the cid before filing: {targets:?}"
        );
        let filed = recorded
            .iter()
            .find(|hit| hit.method == "PUT")
            .expect("yes files the drive name");
        let body: Value = serde_json::from_slice(&filed.body).unwrap();
        assert_eq!(body, json!({ "name": "notes/hello.txt", "cid": "cid-1" }));
    }

    fn serve_drive() -> (String, Arc<Mutex<Vec<Hit>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&hits);
        let presign = url.clone();
        std::thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                let hit = read_hit(stream.try_clone().unwrap());
                let (status, body) = drive_reply(&presign, &hit);
                recorded.lock().unwrap().push(hit);
                write_response(stream, status, &body);
            }
        });
        (url, hits)
    }

    fn drive_reply(api: &str, hit: &Hit) -> (u16, String) {
        let path = hit.target.split('?').next().unwrap_or(hit.target.as_str());
        if hit.method == "GET" && path.ends_with("/data/drive/file") {
            return (
                200,
                r#"{"name":"notes/hello.txt","cid":"old","url":"http://127.0.0.1:9/do-not-fetch","size":4,"mimeType":"text/plain","authorPseudonymousMemberId":"00000000-0000-0000-0000-000000000002","createdAt":"2026-01-01T00:00:00Z"}"#.into(),
            );
        }
        if hit.method == "POST" && path == "/uploads/presign" {
            return (
                200,
                format!(r#"{{"url":"{api}/gcs","fields":{{"key":"v"}},"cid":"cid-1"}}"#),
            );
        }
        if hit.method == "POST" && path == "/gcs" {
            return (204, String::new());
        }
        if hit.method == "POST" && path == "/uploads/cid-1/confirm" {
            return (200, r#"{"cid":"cid-1","status":"confirmed"}"#.into());
        }
        if hit.method == "PUT" && path.ends_with("/data/drive/file") {
            return (
                200,
                r#"{"name":"notes/hello.txt","size":5,"mimeType":"text/plain","authorPseudonymousMemberId":"00000000-0000-0000-0000-000000000002","createdAt":"2026-01-02T00:00:00Z"}"#.into(),
            );
        }
        if hit.method == "DELETE" && path.ends_with("/data/drive/file") {
            return (204, String::new());
        }
        (500, format!("unexpected {} {}", hit.method, hit.target))
    }

    /// Guards a raw 403, and a 404 that does not say what is missing.
    #[tokio::test]
    async fn forbidden_and_missing_explain_who_can_write_and_what_is_absent() {
        let (api, _) = serve(|hit| {
            if hit.target.contains("key=missing") {
                (404, r#"{"code":"not_found"}"#.into())
            } else {
                (403, r#"{"code":"forbidden"}"#.into())
            }
        });
        let session = session(&api);
        let forbidden = kv_ls(&session, false, None, None, None).await.unwrap_err();
        assert_eq!(forbidden.to_string(), FORBIDDEN);
        let missing = kv_get(&session, false, "missing").await.unwrap_err();
        assert_eq!(
            missing.to_string(),
            "KV entry `missing` was not found. An account the app grants nothing also gets 404."
        );
    }
}
