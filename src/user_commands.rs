//! User-facing app workflows built around a normal local Git repository.

use crate::credentials::{self, CredentialUser};
use crate::http as http_request;
use crate::{bundle_upload, project};
use anyhow::{bail, Context, Result};
use reqwest::{header, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const APP_ID_KEY: &str = "maypop.app-id";
const API_URL_KEY: &str = "maypop.api-url";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateAppResponse {
    app_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishedVersion {
    app_id: String,
    n: i32,
    source_commit_sha: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    id: String,
    name: String,
    description: Option<String>,
    visibility: String,
    link_access: String,
    allow_remixing: bool,
    tags: Vec<String>,
    thumbnail_cid: Option<String>,
    latest_version: i32,
    stable_version: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppSessionTokens {
    session_id: String,
    token: String,
    expires_in: i64,
    refresh_token: String,
    scopes: String,
}

/// Machine-readable app session handed to the local SDK development host.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SdkSession {
    api_url: String,
    app_id: String,
    session_id: String,
    token: String,
    expires_in: i64,
    refresh_token: String,
    scopes: String,
}

struct AppConnection {
    repository: PathBuf,
    app_id: String,
    api_url: String,
    http: Client,
}

/// Build an HTTP client with an optional Maypop bearer credential.
pub(crate) fn client(token: Option<&str>) -> Result<Client> {
    let mut headers = header::HeaderMap::new();
    if let Some(token) = token {
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {token}"))?,
        );
    }
    Ok(Client::builder().default_headers(headers).build()?)
}

/// Return an explicit or saved credential, with an actionable error when absent.
pub(crate) fn required_token(
    api_url: &str,
    profile: Option<&str>,
    explicit: Option<&str>,
) -> Result<String> {
    if let Some(token) = explicit {
        return Ok(token.to_string());
    }
    credentials::token_for(api_url, profile)?.with_context(|| match profile {
        Some(profile) => {
            format!("profile `{profile}` is not authenticated; run `maypop --profile {profile} --url {api_url} auth`")
        }
        None => format!("not authenticated for {api_url}; run `maypop --url {api_url} auth`"),
    })
}

fn resolve_initial_app(
    repository: &Path,
    configured: Option<project::AppConfig>,
    name: Option<&str>,
    description: Option<&str>,
    visibility: Option<&str>,
) -> Result<project::AppConfig> {
    let mut app = configured.unwrap_or_default();
    app.name = name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or(app.name)
        .or_else(|| {
            repository
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_string)
        });
    app.description = description.map(str::to_string).or(app.description);
    app.visibility = Some(
        visibility
            .map(str::to_string)
            .or(app.visibility)
            .unwrap_or_else(|| "private".into()),
    );
    app.link_access.get_or_insert_with(|| {
        if app.visibility.as_deref() == Some("public") {
            "view".into()
        } else {
            "request".into()
        }
    });
    app.allow_remixing.get_or_insert(true);
    app.tags.get_or_insert_with(Vec::new);
    if app.name.is_none() {
        bail!("could not infer an app name; pass --name or set app.name in maypop.toml");
    }
    Ok(app)
}

/// Create an app identity and connect the selected directory to Maypop Git.
pub(crate) async fn init(
    api_url: &str,
    explicit_token: Option<&str>,
    profile: Option<&str>,
    path: &Path,
    name: Option<&str>,
    description: Option<&str>,
    visibility: Option<&str>,
) -> Result<()> {
    let repository = prepare_repository(path)?;
    if git_config(&repository, APP_ID_KEY)?.is_some() {
        bail!("this repository is already connected to a Maypop app");
    }
    if git_output(&repository, &["remote", "get-url", "origin"])?
        .status
        .success()
    {
        bail!("this repository already has an origin remote");
    }
    let token = required_token(api_url, profile, explicit_token)?;
    let http = client(Some(&token))?;
    let user = current_user(&http, api_url).await?;
    if user.git_user_id.is_empty() {
        bail!("your saved login predates Git access; run `maypop auth` again");
    }
    let app_id = uuid::Uuid::new_v4().to_string();
    let initial_app = resolve_initial_app(
        &repository,
        project::existing_app_config(&repository)?,
        name,
        description,
        visibility,
    )?;
    let app_name = initial_app
        .name
        .clone()
        .context("initial app name is missing")?;
    let app_visibility = initial_app
        .visibility
        .clone()
        .context("initial app visibility is missing")?;
    let config_path = project::create_config(&repository, Some(&initial_app))?;
    let response = http_request::json(
        http.post(format!("{}/cli/apps", trim_url(api_url))),
        &json!({
            "id": app_id,
            "name": &app_name,
            "description": initial_app.description.as_deref(),
            "visibility": &app_visibility,
        }),
    )?
    .send()
    .await
    .context("could not create the Maypop app")?;
    let created = successful_json::<CreateAppResponse>(response).await?;
    let remote = git_remote_url(
        git_server_url(&user, api_url).as_str(),
        &user.git_user_id,
        &created.app_id,
    )?;
    configure_repository(&repository, api_url, &created.app_id, remote.as_str()).with_context(
        || {
            format!(
                "app {} was created, but the local Git repository could not be configured",
                created.app_id
            )
        },
    )?;
    println!("Initialized Maypop app {} ({app_name}).", created.app_id);
    println!("Git remote: {remote}");
    println!("Build configuration: {}", config_path.display());
    println!("\nCommit your files normally, then run `maypop publish`.");
    Ok(())
}

/// Push the current Git HEAD and promote that exact commit as a new app version.
pub(crate) async fn publish(
    profile: Option<&str>,
    explicit_token: Option<&str>,
    changelog: Option<&str>,
) -> Result<()> {
    let repository = repository_root(std::env::current_dir()?)?;
    let app_id = required_git_config(&repository, APP_ID_KEY)?;
    let api_url = required_git_config(&repository, API_URL_KEY)?;
    project::create_config(&repository, None)?;
    let token = required_token(&api_url, profile, explicit_token)?;
    let http = client(Some(&token))?;
    let user = current_user(&http, &api_url).await?;
    let expected_remote = git_remote_url(
        git_server_url(&user, &api_url).as_str(),
        &user.git_user_id,
        &app_id,
    )?;
    let legacy_remote = git_remote_url(
        &format!("{}/git", trim_url(&api_url)),
        &user.git_user_id,
        &app_id,
    )?;
    repair_legacy_remote(
        &repository,
        &api_url,
        legacy_remote.as_str(),
        expected_remote.as_str(),
    )?;
    let configured_remote = successful_git(&repository, &["remote", "get-url", "origin"])?;
    if configured_remote.trim_end_matches('/') != expected_remote.as_str().trim_end_matches('/') {
        bail!("origin is not the Maypop Git remote configured for this app");
    }
    let dirty = successful_git(&repository, &["status", "--porcelain"])?;
    if !dirty.is_empty() {
        bail!("the Git worktree is not clean; commit or stash changes before publishing");
    }
    let head = successful_git(&repository, &["rev-parse", "HEAD"])?;
    if head.len() != 40 || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Git HEAD is not a SHA-1 commit");
    }
    let build = project::build(&repository)?;
    if !git_output(&repository, &["diff", "--quiet"])?
        .status
        .success()
        || !git_output(&repository, &["diff", "--cached", "--quiet"])?
            .status
            .success()
    {
        bail!("the build modified tracked files; restore or commit them before publishing");
    }
    println!(
        "Built {} output at {}.",
        build.framework,
        build.output_directory.display()
    );

    run_git_with_auth(
        &repository,
        &["push", "--set-upstream", "origin", "HEAD:refs/heads/main"],
        &api_url,
        &token,
    )
    .context("Git push failed; fix the Git error and run `maypop publish` again")?;
    let bundle_id = bundle_upload::upload_directory(
        &http,
        &api_url,
        &build.output_directory,
        &build.entry,
        build.routing,
    )
    .await?;
    let response = http_request::json(
        http.post(format!("{}/cli/apps/{app_id}/publish", trim_url(&api_url))),
        &json!({
            "sourceCommitSha": head,
            "bundleId": bundle_id,
            "changelog": changelog,
        }),
    )?
    .send()
    .await
    .context("could not publish the Maypop app")?;
    let published = successful_json::<PublishedVersion>(response).await?;
    if published.app_id != app_id || published.source_commit_sha.as_deref() != Some(head.as_str()) {
        bail!("Maypop returned a publish result for a different app or commit");
    }
    println!(
        "Published version {} from {}.",
        published.n,
        short_sha(&head)
    );
    Ok(())
}

/// Show the connected app without changing local or remote state.
pub(crate) async fn info(profile: Option<&str>, explicit_token: Option<&str>) -> Result<()> {
    let connection = app_connection(profile, explicit_token)?;
    let response = connection
        .http
        .get(format!(
            "{}/cli/apps/{}",
            trim_url(&connection.api_url),
            connection.app_id
        ))
        .send()
        .await
        .context("could not load the Maypop app")?;
    let app = successful_json::<AppInfo>(response).await?;
    print_app_info(&app, &connection.api_url);
    Ok(())
}

/// Mint a scoped app session for an SDK development host without exposing the CLI credential.
pub(crate) async fn sdk_session(profile: Option<&str>, explicit_token: Option<&str>) -> Result<()> {
    let connection = app_connection(profile, explicit_token)?;
    let response = http_request::json(
        connection
            .http
            .post(format!("{}/app-sessions", trim_url(&connection.api_url))),
        &serde_json::json!({
            "appId": connection.app_id,
            "deviceLabel": "Maypop SDK development",
        }),
    )?
    .send()
    .await
    .context("could not create an authenticated SDK development session")?;
    let session = successful_json::<AppSessionTokens>(response).await?;
    let output = SdkSession {
        api_url: trim_url(&connection.api_url).to_string(),
        app_id: connection.app_id,
        session_id: session.session_id,
        token: session.token,
        expires_in: session.expires_in,
        refresh_token: session.refresh_token,
        scopes: session.scopes,
    };
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

/// Apply the declarative `[app]` metadata to the connected app.
pub(crate) async fn apply(profile: Option<&str>, explicit_token: Option<&str>) -> Result<()> {
    let connection = app_connection(profile, explicit_token)?;
    let config = project::app_config(&connection.repository)?
        .context("maypop.toml has no [app] table; add one or run `maypop init` in a new project")?;
    let mut fields = serde_json::Map::new();
    if let Some(name) = config.name {
        fields.insert("name".into(), name.into());
    }
    if let Some(description) = config.description {
        fields.insert("description".into(), description.into());
    }
    if let Some(visibility) = config.visibility {
        fields.insert("visibility".into(), visibility.into());
    }
    if let Some(link_access) = config.link_access {
        fields.insert("linkAccess".into(), link_access.into());
    }
    if let Some(allow_remixing) = config.allow_remixing {
        fields.insert("allowRemixing".into(), allow_remixing.into());
    }
    if let Some(tags) = config.tags {
        fields.insert("tags".into(), serde_json::to_value(tags)?);
    }
    if let Some(thumbnail) = config.thumbnail {
        let repository = std::fs::canonicalize(&connection.repository)?;
        let thumbnail = std::fs::canonicalize(repository.join(thumbnail))
            .context("could not resolve app.thumbnail")?;
        if !thumbnail.starts_with(&repository) {
            bail!("app.thumbnail must stay inside the Git repository");
        }
        println!("Uploading thumbnail {}...", thumbnail.display());
        let cid =
            bundle_upload::upload_image(&connection.http, &connection.api_url, &thumbnail).await?;
        fields.insert("thumbnailCid".into(), cid.into());
    }
    if fields.is_empty() {
        bail!("maypop.toml [app] has no fields to apply");
    }
    let response = http_request::json(
        connection.http.put(format!(
            "{}/cli/apps/{}",
            trim_url(&connection.api_url),
            connection.app_id
        )),
        &fields,
    )?
    .send()
    .await
    .context("could not update the Maypop app")?;
    let app = successful_json::<AppInfo>(response).await?;
    println!("Applied app metadata from maypop.toml.\n");
    print_app_info(&app, &connection.api_url);
    Ok(())
}

/// Report API reachability and the identity represented by the current token.
pub(crate) async fn status(
    api_url: &str,
    profile: Option<&str>,
    explicit_token: Option<&str>,
) -> Result<()> {
    let api_url = trim_url(api_url);
    if let Some(profile) = profile {
        println!("Profile: {profile}");
    }
    println!("API: {api_url}");
    let plain = Client::new();
    match plain.get(format!("{api_url}/health")).send().await {
        Ok(response) if response.status().is_success() => println!("Backend: reachable"),
        Ok(response) => println!("Backend: unhealthy ({})", response.status()),
        Err(error) => println!("Backend: unreachable ({error})"),
    }

    let token = match explicit_token {
        Some(token) => Some(token.to_string()),
        None => credentials::token_for(api_url, profile)?,
    };
    let Some(token) = token else {
        println!("Authenticated: no");
        return Ok(());
    };
    let http = client(Some(&token))?;
    match current_user(&http, api_url).await {
        Ok(user) => {
            println!("Authenticated: yes");
            println!("Username: @{}", user.username);
            println!("Email: {}", user.email);
        }
        Err(_) => println!("Authenticated: no (token expired or revoked)"),
    }
    Ok(())
}

/// Implement Git's credential-helper protocol using the owner-only Maypop login.
pub(crate) fn git_credential(operation: &str, api_url: &str, profile: Option<&str>) -> Result<()> {
    if operation != "get" {
        return Ok(());
    }
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let Some((_git_url, path)) = credential_request(&input) else {
        return Ok(());
    };
    if let (Ok(token), Ok(configured_url)) =
        (std::env::var("MAYPOP_TOKEN"), std::env::var("MAYPOP_URL"))
    {
        if trim_url(&configured_url) == trim_url(api_url)
            && path.trim_start_matches('/').starts_with("git/")
        {
            println!("username=maypop");
            println!("password={token}");
            return Ok(());
        }
    }
    let saved = if let Some(profile) = profile {
        credentials::load_for(api_url, Some(profile))?
            .filter(|saved| credential_matches(&saved.user, &path))
    } else {
        credentials::all_for(api_url)?
            .into_iter()
            .find(|saved| credential_matches(&saved.user, &path))
    };
    let Some(saved) = saved else {
        return Ok(());
    };
    println!("username=maypop");
    println!("password={}", saved.token);
    Ok(())
}

async fn current_user(client: &Client, api_url: &str) -> Result<CredentialUser> {
    let response = client
        .get(format!("{}/cli/me", trim_url(api_url)))
        .send()
        .await
        .context("could not reach Maypop")?;
    successful_json(response).await
}

fn app_connection(profile: Option<&str>, explicit_token: Option<&str>) -> Result<AppConnection> {
    let repository = repository_root(std::env::current_dir()?)?;
    let app_id = required_git_config(&repository, APP_ID_KEY)?;
    let api_url = required_git_config(&repository, API_URL_KEY)?;
    let token = required_token(&api_url, profile, explicit_token)?;
    let http = client(Some(&token))?;
    Ok(AppConnection {
        repository,
        app_id,
        api_url,
        http,
    })
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

fn prepare_repository(path: &Path) -> Result<PathBuf> {
    Command::new("git")
        .arg("--version")
        .output()
        .context("Git is required but was not found in PATH")?;
    std::fs::create_dir_all(path)
        .with_context(|| format!("could not create {}", path.display()))?;
    if repository_root(path.to_path_buf()).is_err() {
        let initialized = Command::new("git")
            .args(["init", "-b", "main"])
            .arg(path)
            .output()
            .context("could not start git init")?;
        if !initialized.status.success() {
            bail!("git init failed: {}", stderr(&initialized));
        }
    }
    let root = repository_root(path.to_path_buf())?;
    let expected = std::fs::canonicalize(path)?;
    if root != expected {
        bail!("{} is inside another Git repository", path.display());
    }
    Ok(root)
}

fn repository_root(path: PathBuf) -> Result<PathBuf> {
    let root = successful_git(&path, &["rev-parse", "--show-toplevel"])?;
    std::fs::canonicalize(root).context("could not resolve the Git repository root")
}

fn configure_repository(
    repository: &Path,
    api_url: &str,
    app_id: &str,
    remote: &str,
) -> Result<()> {
    run_git(repository, &["remote", "add", "origin", remote])?;
    run_git(repository, &["config", "--local", APP_ID_KEY, app_id])?;
    run_git(
        repository,
        &["config", "--local", API_URL_KEY, trim_url(api_url)],
    )?;
    configure_credential_helper(repository, api_url, remote)?;
    Ok(())
}

fn configure_credential_helper(repository: &Path, api_url: &str, remote: &str) -> Result<()> {
    let helper_scope = credential_scope(remote)?;
    let helper_key = format!("credential.{helper_scope}.helper");
    let _ = git_output(
        repository,
        &["config", "--local", "--unset-all", &helper_key],
    )?;
    run_git(repository, &["config", "--local", "--add", &helper_key, ""])?;
    let helper = format!(
        "!maypop --url={} git-credential",
        shell_quote(trim_url(api_url))
    );
    run_git(
        repository,
        &["config", "--local", "--add", &helper_key, &helper],
    )?;
    run_git(
        repository,
        &["config", "--local", "credential.useHttpPath", "true"],
    )?;
    Ok(())
}

fn repair_legacy_remote(
    repository: &Path,
    api_url: &str,
    legacy_remote: &str,
    expected_remote: &str,
) -> Result<bool> {
    let configured = successful_git(repository, &["remote", "get-url", "origin"])?;
    if trim_url(&configured) != trim_url(legacy_remote)
        || trim_url(legacy_remote) == trim_url(expected_remote)
    {
        return Ok(false);
    }
    run_git(
        repository,
        &["remote", "set-url", "origin", expected_remote],
    )?;
    configure_credential_helper(repository, api_url, expected_remote)?;
    println!("Updated the Maypop Git remote to {expected_remote}.");
    Ok(true)
}

fn git_remote_url(git_server_url: &str, git_user_id: &str, app_id: &str) -> Result<Url> {
    let mut url = Url::parse(trim_url(git_server_url)).context("invalid Maypop Git server URL")?;
    let base_path = url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/{git_user_id}/{app_id}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn git_server_url(user: &CredentialUser, api_url: &str) -> String {
    if user.git_server_url.is_empty() {
        format!("{}/git", trim_url(api_url))
    } else {
        trim_url(&user.git_server_url).to_string()
    }
}

fn credential_scope(remote: &str) -> Result<String> {
    let url = Url::parse(remote).context("invalid Maypop Git remote URL")?;
    Ok(url.origin().ascii_serialization())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn required_git_config(repository: &Path, key: &str) -> Result<String> {
    git_config(repository, key)?.with_context(|| {
        "this repository is not connected to Maypop; run `maypop init`".to_string()
    })
}

fn git_config(repository: &Path, key: &str) -> Result<Option<String>> {
    let output = git_output(repository, &["config", "--local", "--get", key])?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8(output.stdout)?.trim().to_string()))
}

fn run_git(repository: &Path, args: &[&str]) -> Result<()> {
    let output = git_output(repository, args)?;
    if !output.status.success() {
        bail!("git {} failed: {}", args.join(" "), stderr(&output));
    }
    Ok(())
}

fn run_git_with_auth(repository: &Path, args: &[&str], api_url: &str, token: &str) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .env("MAYPOP_URL", trim_url(api_url))
        .env("MAYPOP_TOKEN", token)
        .output()
        .with_context(|| format!("could not run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!("git {} failed: {}", args.join(" "), stderr(&output));
    }
    Ok(())
}

fn successful_git(repository: &Path, args: &[&str]) -> Result<String> {
    let output = git_output(repository, args)?;
    if !output.status.success() {
        bail!("git {} failed: {}", args.join(" "), stderr(&output));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn git_output(repository: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .with_context(|| format!("could not run git {}", args.join(" ")))
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

fn credential_fields(input: &str) -> HashMap<&str, &str> {
    input
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect()
}

fn credential_request(input: &str) -> Option<(String, String)> {
    let fields = credential_fields(input);
    let protocol = fields.get("protocol")?;
    let host = fields.get("host")?;
    let path = fields.get("path")?;
    Some((format!("{protocol}://{host}"), (*path).to_string()))
}

fn credential_matches(user: &CredentialUser, path: &str) -> bool {
    let expected_prefix = format!("git/{}/", user.git_user_id);
    !user.git_user_id.is_empty() && path.trim_start_matches('/').starts_with(&expected_prefix)
}

fn short_sha(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

fn print_app_info(app: &AppInfo, api_url: &str) {
    println!("App: {}", app.name);
    println!("ID: {}", app.id);
    println!("Description: {}", app.description.as_deref().unwrap_or("—"));
    println!("Visibility: {}", app.visibility);
    println!("Link access: {}", app.link_access);
    println!(
        "Allow remixing: {}",
        if app.allow_remixing { "yes" } else { "no" }
    );
    println!(
        "Tags: {}",
        if app.tags.is_empty() {
            "—".into()
        } else {
            app.tags.join(", ")
        }
    );
    println!("Thumbnail: {}", app.thumbnail_cid.as_deref().unwrap_or("—"));
    println!(
        "Versions: latest {}, stable {}",
        app.latest_version, app.stable_version
    );
    println!("API: {}", trim_url(api_url));
}

fn trim_url(url: &str) -> &str {
    url.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_uses_configured_app_metadata() {
        let configured = project::AppConfig {
            name: Some("Configured app".into()),
            description: Some("Configured description".into()),
            visibility: Some("unlisted".into()),
            link_access: Some("use".into()),
            allow_remixing: Some(false),
            tags: Some(vec!["design".into()]),
            thumbnail: Some("assets/thumbnail.png".into()),
        };

        let resolved = resolve_initial_app(
            Path::new("/tmp/directory-name"),
            Some(configured.clone()),
            None,
            None,
            None,
        )
        .unwrap();

        assert_eq!(resolved, configured);
    }

    #[test]
    fn init_flags_override_configured_creation_metadata() {
        let configured = project::AppConfig {
            name: Some("Configured app".into()),
            description: Some("Configured description".into()),
            visibility: Some("unlisted".into()),
            link_access: Some("view".into()),
            allow_remixing: Some(false),
            tags: Some(vec!["design".into()]),
            thumbnail: None,
        };

        let resolved = resolve_initial_app(
            Path::new("/tmp/directory-name"),
            Some(configured),
            Some("CLI app"),
            Some("CLI description"),
            Some("public"),
        )
        .unwrap();

        assert_eq!(resolved.name.as_deref(), Some("CLI app"));
        assert_eq!(resolved.description.as_deref(), Some("CLI description"));
        assert_eq!(resolved.visibility.as_deref(), Some("public"));
        assert_eq!(resolved.link_access.as_deref(), Some("view"));
        assert_eq!(resolved.allow_remixing, Some(false));
        assert_eq!(resolved.tags, Some(vec!["design".into()]));
    }

    #[test]
    fn init_defaults_metadata_without_configuration_or_flags() {
        let resolved =
            resolve_initial_app(Path::new("/tmp/directory-name"), None, None, None, None).unwrap();

        assert_eq!(resolved.name.as_deref(), Some("directory-name"));
        assert_eq!(resolved.description, None);
        assert_eq!(resolved.visibility.as_deref(), Some("private"));
        assert_eq!(resolved.link_access.as_deref(), Some("request"));
        assert_eq!(resolved.allow_remixing, Some(true));
        assert_eq!(resolved.tags, Some(Vec::new()));
        assert_eq!(resolved.thumbnail, None);
    }

    #[test]
    fn git_remote_uses_the_server_url() {
        let url = git_remote_url(
            "https://api.app.maypop.ai/git/",
            "user_123",
            "00000000-0000-0000-0000-000000000001",
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.app.maypop.ai/git/user_123/00000000-0000-0000-0000-000000000001"
        );
    }

    #[test]
    fn credential_protocol_parser_ignores_unknown_fields() {
        let request = credential_request(
            "protocol=https\nhost=api.app.maypop.ai\npath=git/user_1/app\nwwwauth[]=Basic\n",
        )
        .unwrap();
        assert_eq!(request.0, "https://api.app.maypop.ai");
        assert_eq!(request.1, "git/user_1/app");

        let user = CredentialUser {
            id: "id".into(),
            username: "user".into(),
            name: None,
            email: "user@example.com".into(),
            git_user_id: "user_1".into(),
            git_server_url: "https://api.app.maypop.ai/git".into(),
        };
        assert!(credential_matches(&user, "git/user_1/app"));
        assert!(!credential_matches(&user, "git/user_2/app"));
    }

    #[test]
    fn repository_configuration_uses_the_maypop_credential_helper() {
        let directory =
            std::env::temp_dir().join(format!("maypop-cli-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        run_git(&directory, &["init", "-b", "main"]).unwrap();

        configure_repository(
            &directory,
            "https://api.app.maypop.ai",
            "00000000-0000-0000-0000-000000000001",
            "http://localhost:3005/git/user_1/00000000-0000-0000-0000-000000000001",
        )
        .unwrap();

        assert_eq!(
            git_config(&directory, APP_ID_KEY).unwrap().as_deref(),
            Some("00000000-0000-0000-0000-000000000001")
        );
        assert_eq!(
            successful_git(&directory, &["remote", "get-url", "origin"]).unwrap(),
            "http://localhost:3005/git/user_1/00000000-0000-0000-0000-000000000001"
        );
        let helpers = successful_git(
            &directory,
            &[
                "config",
                "--local",
                "--get-all",
                "credential.http://localhost:3005.helper",
            ],
        )
        .unwrap();
        assert!(helpers
            .lines()
            .any(|line| line == "!maypop --url='https://api.app.maypop.ai' git-credential"));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_repairs_the_legacy_local_git_remote() {
        let directory =
            std::env::temp_dir().join(format!("maypop-cli-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        run_git(&directory, &["init", "-b", "main"]).unwrap();
        let legacy = "http://localhost:3000/git/user_1/app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        configure_repository(&directory, "http://localhost:3000", "app_1", legacy).unwrap();

        assert!(
            repair_legacy_remote(&directory, "http://localhost:3000", legacy, expected,).unwrap()
        );
        assert_eq!(
            successful_git(&directory, &["remote", "get-url", "origin"]).unwrap(),
            expected
        );
        let helpers = successful_git(
            &directory,
            &[
                "config",
                "--local",
                "--get-all",
                "credential.http://localhost:3005.helper",
            ],
        )
        .unwrap();
        assert!(helpers
            .lines()
            .any(|line| line == "!maypop --url='http://localhost:3000' git-credential"));

        std::fs::remove_dir_all(directory).unwrap();
    }
}
