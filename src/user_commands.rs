//! User-facing app workflows built around a normal local Git repository.

use crate::credentials::{self, api_origin, api_urls_match, CredentialUser};
use crate::http as http_request;
use crate::{bundle_upload, project};
use anyhow::{bail, Context, Result};
use reqwest::{header, Client, Url};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(test)]
thread_local! {
    static LAST_GIT_ARGS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    static TEST_GIT_CONFIG_GLOBAL: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

const APP_ID_KEY: &str = "maypop.app-id";
const API_URL_KEY: &str = "maypop.api-url";
/// The remote `maypop init` adds. `publish` finds the app's remote by URL, so
/// `origin` stays free for wherever else the repository lives.
const MAYPOP_REMOTE: &str = "maypop";

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

pub(crate) struct AppConnection {
    repository: PathBuf,
    pub(crate) app_id: String,
    pub(crate) api_url: String,
    pub(crate) http: Client,
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
    if git_output(&repository, &["remote", "get-url", MAYPOP_REMOTE])?
        .status
        .success()
    {
        bail!("this repository already has a {MAYPOP_REMOTE} remote");
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
    project::ignore_maypop_local(&repository)?;
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
    let remote = app_repo_remote(api_url, &created.app_id)?;
    configure_repository(&repository, api_url, &created.app_id, &remote).with_context(|| {
        format!(
            "app {} was created, but the local Git repository could not be configured",
            created.app_id
        )
    })?;
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
    if user.git_user_id.is_empty() {
        bail!("your saved login predates Git access; run `maypop auth` again");
    }
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
    let remote = find_app_remote(
        &repository,
        &api_url,
        expected_remote.as_str(),
        legacy_remote.as_str(),
        &app_id,
    )?;
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

    push_head_to_app(&repository, &remote, &api_url, &token)
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
    if let Some(url) = web_app_url(&api_url, &app_id) {
        println!("Open it at {url}");
    }
    Ok(())
}

/// Move a retired Maypop remote onto the app repository and push `HEAD`.
pub(crate) async fn migrate(profile: Option<&str>, explicit_token: Option<&str>) -> Result<()> {
    let repository = repository_root(std::env::current_dir()?)?;
    let app_id = required_git_config(&repository, APP_ID_KEY)?;
    let api_url = required_git_config(&repository, API_URL_KEY)?;
    let token = required_token(&api_url, profile, explicit_token)?;
    let http = client(Some(&token))?;
    let user = current_user(&http, &api_url).await?;
    if user.git_user_id.is_empty() {
        bail!("your saved login predates Git access; run `maypop auth` again");
    }
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
    let remote = find_app_remote(
        &repository,
        &api_url,
        expected_remote.as_str(),
        legacy_remote.as_str(),
        &app_id,
    )?;
    let head = successful_git(&repository, &["rev-parse", "HEAD"])?;
    if head.len() != 40 || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Git HEAD is not a SHA-1 commit");
    }
    push_head_to_app(&repository, &remote, &api_url, &token)
        .context("Git push failed; fix the Git error and run `maypop migrate` again")?;
    println!(
        "Pushed {} to {}.",
        short_sha(&head),
        app_repo_remote(&api_url, &app_id)?
    );
    Ok(())
}

/// The web app serves on the API host minus its `api.` label
/// (`api.app.maypop.ai` -> `app.maypop.ai`); the app page is `/app/:appId`.
/// `None` for API hosts that don't follow that scheme (e.g. localhost).
fn web_app_url(api_url: &str, app_id: &str) -> Option<String> {
    let (scheme, rest) = trim_url(api_url).split_once("://")?;
    let host = rest.strip_prefix("api.")?;
    Some(format!("{scheme}://{host}/app/{app_id}"))
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
        if api_urls_match(&configured_url, api_url)
            && path.trim_start_matches('/').starts_with("git/")
        {
            println!("username=maypop");
            println!("password={token}");
            return Ok(());
        }
    }
    let app_id = std::env::current_dir()
        .ok()
        .and_then(|dir| checkout_app_id(&dir));
    let saved = credential_login(api_url, profile, &path, app_id.as_deref())?;
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

pub(crate) fn app_connection(
    profile: Option<&str>,
    explicit_token: Option<&str>,
) -> Result<AppConnection> {
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

pub(crate) async fn successful_json<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> Result<T> {
    let status = response.status();
    if status.is_success() {
        return response
            .json()
            .await
            .context("Maypop returned invalid JSON");
    }
    let body = response.text().await.unwrap_or_default();
    Err(http_request::failure(status, &body))
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
    run_git(repository, &["remote", "add", MAYPOP_REMOTE, remote])?;
    run_git(repository, &["config", "--local", APP_ID_KEY, app_id])?;
    run_git(
        repository,
        &["config", "--local", API_URL_KEY, &api_origin(api_url)?],
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
        shell_quote(&api_origin(api_url)?)
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

/// `git push <remote> HEAD:refs/heads/main` with no extra flags.
fn git_head_push_args(remote: &str) -> Vec<String> {
    vec![
        "push".to_string(),
        remote.to_string(),
        "HEAD:refs/heads/main".to_string(),
    ]
}

fn push_head_to_app(repository: &Path, remote: &str, api_url: &str, token: &str) -> Result<()> {
    let args = git_head_push_args(remote);
    run_git_with_auth(
        repository,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        api_url,
        token,
    )
}

/// The name of the remote that points at this app, whatever it is called.
fn find_app_remote(
    repository: &Path,
    api_url: &str,
    expected_remote: &str,
    legacy_remote: &str,
    app_id: &str,
) -> Result<String> {
    let remotes: Vec<String> = successful_git(repository, &["remote"])?
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect();
    for remote in &remotes {
        repair_legacy_remote(
            repository,
            remote,
            api_url,
            legacy_remote,
            expected_remote,
            app_id,
        )?;
    }
    let app_remote = app_repo_remote(api_url, app_id)?;
    for remote in &remotes {
        let fetch_urls = configured_remote_urls(repository, remote)?;
        let push_urls = configured_push_urls(repository, remote, &fetch_urls)?;
        if !remote_is_configured_for_app(&fetch_urls, &push_urls, api_url, app_id) {
            continue;
        }
        let pushed = git_push_urls(repository, remote)?;
        if pushed.len() == 1 && urls_equal(&pushed[0], &app_remote) {
            return Ok(remote.clone());
        }
    }
    bail!("no Git remote points at this Maypop app; add one with `git remote add {MAYPOP_REMOTE} {app_remote}`");
}

fn configured_remote_urls(repository: &Path, remote: &str) -> Result<Vec<String>> {
    let urls = git_config_values(repository, &format!("remote.{remote}.url"))?;
    if urls.is_empty() {
        bail!("remote {remote} has no URL");
    }
    Ok(urls)
}

fn configured_push_urls(
    repository: &Path,
    remote: &str,
    fetch_urls: &[String],
) -> Result<Vec<String>> {
    let pushurls = git_config_values(repository, &format!("remote.{remote}.pushurl"))?;
    if pushurls.is_empty() {
        Ok(fetch_urls.to_vec())
    } else {
        Ok(pushurls)
    }
}

fn remote_is_configured_for_app(
    fetch_urls: &[String],
    push_urls: &[String],
    api_url: &str,
    app_id: &str,
) -> bool {
    let Ok(app_remote) = app_repo_remote(api_url, app_id) else {
        return false;
    };
    let Some(app_remote) = canonical_git_url(&app_remote) else {
        return false;
    };
    let expected_path = format!("/git/apps/{app_id}");
    let fetches_ok = !fetch_urls.is_empty()
        && fetch_urls.iter().all(|fetch_url| {
            let Some(fetch) = canonical_git_url(fetch_url) else {
                return false;
            };
            fetch == app_remote
                || git_url_path(fetch_url).as_deref() == Some(expected_path.as_str())
        });
    fetches_ok
        && !push_urls.is_empty()
        && push_urls
            .iter()
            .all(|push| canonical_git_url(push).as_deref() == Some(app_remote.as_str()))
}

fn canonical_git_url(url: &str) -> Option<String> {
    let mut parsed = Url::parse(url).ok()?;
    if parsed.query().is_some() || !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    parsed.set_fragment(None);
    let path = parsed.path().trim_end_matches('/').to_string();
    parsed.set_path(&path);
    Some(trim_url(parsed.as_str()).to_string())
}

fn git_url_path(url: &str) -> Option<String> {
    canonical_git_url(url).and_then(|canonical| {
        Url::parse(&canonical)
            .ok()
            .map(|parsed| parsed.path().to_string())
    })
}

fn urls_equal(left: &str, right: &str) -> bool {
    canonical_git_url(left).is_some() && canonical_git_url(left) == canonical_git_url(right)
}

fn app_repo_remote(api_url: &str, app_id: &str) -> Result<String> {
    Ok(format!("{}/git/apps/{app_id}", api_origin(api_url)?))
}

fn repair_legacy_remote(
    repository: &Path,
    remote: &str,
    api_url: &str,
    legacy_remote: &str,
    expected_remote: &str,
    app_id: &str,
) -> Result<bool> {
    let app_remote = app_repo_remote(api_url, app_id)?;
    let fetch_urls = configured_remote_urls(repository, remote)?;
    if fetch_urls.len() != 1 {
        return Ok(false);
    }
    let fetch = fetch_urls[0].clone();
    let retired = urls_equal(&fetch, legacy_remote) || urls_equal(&fetch, expected_remote);
    if !retired || urls_equal(&fetch, &app_remote) {
        return Ok(false);
    }
    let pushurl_key = format!("remote.{remote}.pushurl");
    let previous_pushurls = git_config_values(repository, &pushurl_key)?;
    let push_urls = if previous_pushurls.is_empty() {
        vec![fetch.clone()]
    } else {
        previous_pushurls.clone()
    };
    if push_urls.is_empty() || !push_urls.iter().all(|push| urls_equal(push, &fetch)) {
        return Ok(false);
    }
    let restore = || -> Result<()> {
        run_git(repository, &["remote", "set-url", remote, &fetch])?;
        unset_git_config_all(repository, &pushurl_key)?;
        for push in &previous_pushurls {
            run_git(
                repository,
                &["remote", "set-url", "--add", "--push", remote, push],
            )?;
        }
        Ok(())
    };
    configure_credential_helper(repository, api_url, &app_remote)?;
    if let Err(error) = run_git(repository, &["remote", "set-url", remote, &app_remote]) {
        restore()?;
        return Err(error);
    }
    if let Err(error) = unset_git_config_all(repository, &pushurl_key) {
        restore()?;
        return Err(error);
    }
    let pushed = git_push_urls(repository, remote)?;
    if pushed.len() != 1 || !urls_equal(&pushed[0], &app_remote) {
        restore()?;
        bail!("could not move remote {remote} onto the Maypop app repository");
    }
    if let (Ok(old_scope), Ok(new_scope)) =
        (credential_scope(&fetch), credential_scope(&app_remote))
    {
        if old_scope != new_scope {
            unset_git_config_all(repository, &format!("credential.{old_scope}.helper"))?;
        }
    }
    println!("Moved the Git remote to {app_remote}.");
    Ok(true)
}

fn git_push_urls(repository: &Path, remote: &str) -> Result<Vec<String>> {
    Ok(successful_git(
        repository,
        &["remote", "get-url", "--all", "--push", remote],
    )?
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .map(str::to_string)
    .collect())
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
    Ok(git_config_values(repository, key)?.into_iter().next())
}

fn git_config_values(repository: &Path, key: &str) -> Result<Vec<String>> {
    let output = git_output(repository, &["config", "--local", "--get-all", key])?;
    match output.status.code() {
        Some(0) => Ok(String::from_utf8(output.stdout)?
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()),
        Some(1) | Some(5) => Ok(Vec::new()),
        _ => bail!("git config --get-all {key} failed: {}", stderr(&output)),
    }
}

fn unset_git_config_all(repository: &Path, key: &str) -> Result<()> {
    let output = git_output(repository, &["config", "--local", "--unset-all", key])?;
    match output.status.code() {
        Some(0) | Some(5) => Ok(()),
        _ => bail!("git config --unset-all {key} failed: {}", stderr(&output)),
    }
}

fn run_git(repository: &Path, args: &[&str]) -> Result<()> {
    let output = git_output(repository, args)?;
    if !output.status.success() {
        bail!("git {} failed: {}", args.join(" "), stderr(&output));
    }
    Ok(())
}

fn run_git_with_auth(repository: &Path, args: &[&str], api_url: &str, token: &str) -> Result<()> {
    #[cfg(test)]
    LAST_GIT_ARGS.with(|cell| {
        *cell.borrow_mut() = args.iter().map(|arg| (*arg).to_string()).collect();
    });
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .env("MAYPOP_URL", api_origin(api_url)?)
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
    let mut command = Command::new("git");
    command.arg("-C").arg(repository).args(args);
    #[cfg(test)]
    TEST_GIT_CONFIG_GLOBAL.with(|path| {
        if let Some(config) = path.borrow().as_ref() {
            command.env("GIT_CONFIG_GLOBAL", config);
            command.env("GIT_CONFIG_NOSYSTEM", "1");
        }
    });
    command
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

fn checkout_app_id(start: &Path) -> Option<String> {
    repository_root(start.to_path_buf())
        .ok()
        .and_then(|root| git_config(&root, APP_ID_KEY).ok().flatten())
}

fn credential_login(
    api_url: &str,
    profile: Option<&str>,
    path: &str,
    app_id: Option<&str>,
) -> Result<Option<credentials::Credentials>> {
    if profile.is_some() || app_repo_git_path(path, app_id) {
        Ok(credentials::load_for(api_url, profile)?
            .filter(|saved| credential_matches(&saved.user, path, app_id)))
    } else {
        Ok(credentials::all_for(api_url)?
            .into_iter()
            .find(|saved| credential_matches(&saved.user, path, app_id)))
    }
}

fn app_repo_git_path(path: &str, app_id: Option<&str>) -> bool {
    let Some(app_id) = app_id.filter(|id| !id.is_empty()) else {
        return false;
    };
    path.trim_start_matches('/') == format!("git/apps/{app_id}")
}

fn credential_matches(user: &CredentialUser, path: &str, app_id: Option<&str>) -> bool {
    let path = path.trim_start_matches('/');
    if !user.git_user_id.is_empty() {
        let expected_prefix = format!("git/{}/", user.git_user_id);
        if path.starts_with(&expected_prefix) {
            return true;
        }
    }
    app_repo_git_path(path, app_id)
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
    use crate::credentials::{self, Credentials};
    use std::sync::Mutex;

    static CREDENTIAL_CONFIG: Mutex<()> = Mutex::new(());

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
    fn web_app_url_drops_the_api_label() {
        assert_eq!(
            web_app_url("https://api.app.maypop.ai/", "a1").as_deref(),
            Some("https://app.maypop.ai/app/a1")
        );
        assert_eq!(
            web_app_url("https://api.dev.maypop.ai", "a1").as_deref(),
            Some("https://dev.maypop.ai/app/a1")
        );
        assert_eq!(web_app_url("http://localhost:3000", "a1"), None);
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
        assert!(credential_matches(&user, "git/user_1/app", Some("app_1")));
        assert!(!credential_matches(&user, "git/user_2/app", Some("app_1")));
        assert!(credential_matches(&user, "git/apps/app_1", Some("app_1")));
        assert!(!credential_matches(&user, "git/apps/other", Some("app_1")));
        assert!(!credential_matches(&user, "git/apps/app_1", None));
        assert!(!credential_matches(&user, "git/apps/app_10", Some("app_1")));
        assert!(!credential_matches(
            &user,
            "git/apps/app_1/extra",
            Some("app_1")
        ));
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
            successful_git(&directory, &["remote", "get-url", MAYPOP_REMOTE]).unwrap(),
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
        let app_remote = "http://localhost:3000/git/apps/app_1";
        configure_repository(&directory, "http://localhost:3000", "app_1", legacy).unwrap();

        find_app_remote(
            &directory,
            "http://localhost:3000",
            expected,
            legacy,
            "app_1",
        )
        .unwrap();
        assert_eq!(
            successful_git(&directory, &["remote", "get-url", MAYPOP_REMOTE]).unwrap(),
            app_remote
        );
        assert_eq!(
            successful_git(&directory, &["remote", "get-url", "--push", MAYPOP_REMOTE]).unwrap(),
            app_remote
        );
        let helpers = successful_git(
            &directory,
            &[
                "config",
                "--local",
                "--get-all",
                "credential.http://localhost:3000.helper",
            ],
        )
        .unwrap();
        assert!(helpers
            .lines()
            .any(|line| line == "!maypop --url='http://localhost:3000' git-credential"));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn public_fetch_does_not_accept_a_push_url_that_fails_to_canonicalize() {
        assert!(!remote_is_configured_for_app(
            &["http://127.0.0.1:18080/git/apps/app_1".into()],
            &[
                "https://evil.example/git/apps/app_1?x=1".into(),
                "http://user@evil.example/git/apps/app_1".into(),
            ],
            "http://localhost:3000",
            "app_1",
        ));
        assert!(!remote_is_configured_for_app(
            &["http://127.0.0.1:18080/git/apps/app_1".into()],
            &["http://localhost:3000/git/apps/app_1".into()],
            "http://user@localhost:3000",
            "app_1",
        ));
        assert!(!remote_is_configured_for_app(
            &["http://localhost:3005/git/user_1/app_1".into()],
            &["http://localhost:3005/git/user_1/app_1".into()],
            "http://localhost:3000",
            "app_1",
        ));
        assert!(!remote_is_configured_for_app(
            &[
                "http://localhost:3000/git/apps/app_1".into(),
                "https://github.com/someone/game.git".into(),
            ],
            &[
                "http://localhost:3000/git/apps/app_1".into(),
                "https://github.com/someone/game.git".into(),
            ],
            "http://localhost:3000",
            "app_1",
        ));
    }

    fn temp_git_repo() -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("maypop-cli-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        run_git(&directory, &["init", "-b", "main"]).unwrap();
        directory
    }

    fn origin_urls(directory: &Path) -> (String, String) {
        (
            successful_git(directory, &["remote", "get-url", MAYPOP_REMOTE]).unwrap(),
            successful_git(directory, &["remote", "get-url", "--push", MAYPOP_REMOTE]).unwrap(),
        )
    }

    fn assert_rejected_origin(
        directory: &Path,
        api: &str,
        expected: &str,
        legacy: &str,
        app_id: &str,
    ) {
        let before = origin_urls(directory);
        let error = find_app_remote(directory, api, expected, legacy, app_id).unwrap_err();
        assert!(error
            .to_string()
            .contains("no Git remote points at this Maypop app"));
        assert_eq!(origin_urls(directory), before);
    }

    #[test]
    fn publish_accepts_the_app_repo_origin_without_rewriting_it() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let origin = format!("{api}/git/apps/{app_id}");
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        configure_repository(&directory, api, app_id, &origin).unwrap();

        find_app_remote(&directory, api, expected, legacy, app_id).unwrap();
        assert_eq!(origin_urls(&directory), (origin.clone(), origin));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_accepts_a_public_fetch_url_with_the_api_push_url() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let fetch = format!("http://127.0.0.1:18080/git/apps/{app_id}");
        let push = format!("{api}/git/apps/{app_id}");
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        configure_repository(&directory, api, app_id, &fetch).unwrap();
        run_git(
            &directory,
            &["remote", "set-url", "--push", MAYPOP_REMOTE, &push],
        )
        .unwrap();

        find_app_remote(&directory, api, expected, legacy, app_id).unwrap();
        assert_eq!(
            successful_git(&directory, &["remote", "get-url", MAYPOP_REMOTE]).unwrap(),
            fetch
        );
        assert_eq!(
            successful_git(&directory, &["remote", "get-url", "--push", MAYPOP_REMOTE]).unwrap(),
            push
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_moves_the_git_server_per_user_remote_to_the_app_repo() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        let app_remote = format!("{api}/git/apps/{app_id}");
        configure_repository(&directory, api, app_id, expected).unwrap();

        find_app_remote(&directory, api, expected, legacy, app_id).unwrap();
        assert_eq!(origin_urls(&directory), (app_remote.clone(), app_remote));
        let helpers = successful_git(
            &directory,
            &[
                "config",
                "--local",
                "--get-all",
                "credential.http://localhost:3000.helper",
            ],
        )
        .unwrap();
        assert!(helpers
            .lines()
            .any(|line| line == "!maypop --url='http://localhost:3000' git-credential"));
        assert!(
            git_config(&directory, "credential.http://localhost:3005.helper")
                .unwrap()
                .is_none()
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_moves_an_explicit_per_user_pushurl() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        let app_remote = format!("{api}/git/apps/{app_id}");
        configure_repository(&directory, api, app_id, expected).unwrap();
        run_git(
            &directory,
            &["remote", "set-url", "--push", MAYPOP_REMOTE, expected],
        )
        .unwrap();
        assert_eq!(
            git_config_values(&directory, &format!("remote.{MAYPOP_REMOTE}.pushurl")).unwrap(),
            vec![expected.to_string()]
        );

        find_app_remote(&directory, api, expected, legacy, app_id).unwrap();
        assert_eq!(origin_urls(&directory), (app_remote.clone(), app_remote));
        assert!(
            git_config_values(&directory, &format!("remote.{MAYPOP_REMOTE}.pushurl"))
                .unwrap()
                .is_empty()
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_repairs_every_retired_remote_before_choosing() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        let app_remote = format!("{api}/git/apps/{app_id}");
        configure_repository(&directory, api, app_id, &app_remote).unwrap();
        run_git(&directory, &["remote", "add", "later", expected]).unwrap();

        let chosen = find_app_remote(&directory, api, expected, legacy, app_id).unwrap();
        assert!(chosen == MAYPOP_REMOTE || chosen == "later");
        assert_eq!(
            successful_git(&directory, &["remote", "get-url", "later"]).unwrap(),
            app_remote
        );
        assert_eq!(
            git_config(&directory, "remote.later.url")
                .unwrap()
                .as_deref(),
            Some(app_remote.as_str())
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_does_not_rewrite_a_foreign_url_that_instead_of_maps_to_per_user() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        let foreign = "https://example.com/other.git";
        configure_repository(&directory, api, app_id, foreign).unwrap();
        run_git(
            &directory,
            &[
                "config",
                "--local",
                &format!("url.{expected}.insteadOf"),
                foreign,
            ],
        )
        .unwrap();
        assert_eq!(
            successful_git(&directory, &["remote", "get-url", MAYPOP_REMOTE]).unwrap(),
            expected
        );

        assert_rejected_origin(&directory, api, expected, legacy, app_id);
        assert_eq!(
            git_config(&directory, &format!("remote.{MAYPOP_REMOTE}.url"))
                .unwrap()
                .as_deref(),
            Some(foreign)
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    fn last_git_args() -> Vec<String> {
        LAST_GIT_ARGS.with(|cell| cell.borrow().clone())
    }

    fn commit_file(repository: &Path, name: &str, contents: &str) {
        std::fs::write(repository.join(name), contents).unwrap();
        run_git(repository, &["add", name]).unwrap();
        run_git(
            repository,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                name,
            ],
        )
        .unwrap();
    }

    fn temp_bare_repo() -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("maypop-cli-bare-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        run_git(&directory, &["init", "--bare", "-b", "main"]).unwrap();
        directory
    }

    fn file_url(path: &Path) -> String {
        format!("file://{}", path.display())
    }

    #[test]
    fn git_push_args_are_only_the_remote_and_main_refspec() {
        assert_eq!(
            git_head_push_args("maypop"),
            ["push", "maypop", "HEAD:refs/heads/main"]
        );
        assert_eq!(
            git_head_push_args("origin"),
            ["push", "origin", "HEAD:refs/heads/main"]
        );
        assert_eq!(
            app_repo_remote("http://localhost:3000/", "app_1").unwrap(),
            "http://localhost:3000/git/apps/app_1"
        );
    }

    #[test]
    fn migrate_push_updates_a_bare_app_repo() {
        let work = temp_git_repo();
        commit_file(&work, "README", "one");
        let head = successful_git(&work, &["rev-parse", "HEAD"]).unwrap();
        let bare = temp_bare_repo();
        let app_id = "app_1";
        let api = "http://localhost:3000";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        configure_repository(&work, api, app_id, expected).unwrap();
        find_app_remote(&work, api, expected, legacy, app_id).unwrap();
        assert_eq!(
            git_config(&work, &format!("remote.{MAYPOP_REMOTE}.url"))
                .unwrap()
                .as_deref(),
            Some("http://localhost:3000/git/apps/app_1")
        );
        run_git(
            &work,
            &["remote", "set-url", MAYPOP_REMOTE, &file_url(&bare)],
        )
        .unwrap();

        push_head_to_app(&work, MAYPOP_REMOTE, api, "token").unwrap();
        assert_eq!(
            last_git_args(),
            ["push", MAYPOP_REMOTE, "HEAD:refs/heads/main"]
        );
        assert_eq!(
            successful_git(&bare, &["rev-parse", "refs/heads/main"]).unwrap(),
            head
        );
        assert!(git_config(&work, "remote.maypop.url")
            .unwrap()
            .unwrap()
            .starts_with("file://"));

        std::fs::remove_dir_all(work).unwrap();
        std::fs::remove_dir_all(bare).unwrap();
    }

    #[test]
    fn rejected_push_leaves_the_app_repo_remote() {
        let work = temp_git_repo();
        commit_file(&work, "README", "one");
        let first = successful_git(&work, &["rev-parse", "HEAD"]).unwrap();
        let bare = temp_bare_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        let app_remote = app_repo_remote(api, app_id).unwrap();
        configure_repository(&work, api, app_id, expected).unwrap();
        find_app_remote(&work, api, expected, legacy, app_id).unwrap();
        assert_eq!(
            git_config(&work, &format!("remote.{MAYPOP_REMOTE}.url"))
                .unwrap()
                .as_deref(),
            Some(app_remote.as_str())
        );
        run_git(
            &work,
            &["remote", "set-url", MAYPOP_REMOTE, &file_url(&bare)],
        )
        .unwrap();
        push_head_to_app(&work, MAYPOP_REMOTE, api, "token").unwrap();
        run_git(&work, &["checkout", "--orphan", "other"]).unwrap();
        let _ = git_output(&work, &["rm", "-rf", "."]);
        commit_file(&work, "README", "two");
        run_git(&work, &["remote", "set-url", MAYPOP_REMOTE, &app_remote]).unwrap();
        run_git(
            &work,
            &[
                "remote",
                "set-url",
                "--add",
                "--push",
                MAYPOP_REMOTE,
                &file_url(&bare),
            ],
        )
        .unwrap();

        let error = push_head_to_app(&work, MAYPOP_REMOTE, api, "token").unwrap_err();
        assert!(error.to_string().contains("git push"));
        assert_eq!(
            last_git_args(),
            ["push", MAYPOP_REMOTE, "HEAD:refs/heads/main"]
        );
        assert_eq!(
            git_config(&work, &format!("remote.{MAYPOP_REMOTE}.url"))
                .unwrap()
                .as_deref(),
            Some(app_remote.as_str())
        );
        assert_eq!(
            successful_git(&bare, &["rev-parse", "refs/heads/main"]).unwrap(),
            first
        );

        std::fs::remove_dir_all(work).unwrap();
        std::fs::remove_dir_all(bare).unwrap();
    }

    #[test]
    fn extra_fetch_url_is_not_rewritten() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        configure_repository(&directory, api, app_id, expected).unwrap();
        run_git(
            &directory,
            &[
                "remote",
                "set-url",
                "--add",
                MAYPOP_REMOTE,
                "https://github.com/someone/game.git",
            ],
        )
        .unwrap();
        let before = git_config_values(&directory, &format!("remote.{MAYPOP_REMOTE}.url")).unwrap();
        assert_eq!(before.len(), 2);

        assert_rejected_origin(&directory, api, expected, legacy, app_id);
        assert_eq!(
            git_config_values(&directory, &format!("remote.{MAYPOP_REMOTE}.url")).unwrap(),
            before
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn global_pushurl_does_not_select_the_app_remote() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        let origin = format!("{api}/git/apps/{app_id}");
        configure_repository(&directory, api, app_id, &origin).unwrap();
        let global =
            std::env::temp_dir().join(format!("maypop-cli-gitconfig-{}", uuid::Uuid::new_v4()));
        std::fs::write(
            &global,
            "[remote \"maypop\"]\n\tpushurl = https://example.com/other.git\n",
        )
        .unwrap();
        TEST_GIT_CONFIG_GLOBAL.with(|cell| *cell.borrow_mut() = Some(global.clone()));
        let pushed = git_push_urls(&directory, MAYPOP_REMOTE).unwrap();
        let error = find_app_remote(&directory, api, expected, legacy, app_id).unwrap_err();
        TEST_GIT_CONFIG_GLOBAL.with(|cell| *cell.borrow_mut() = None);
        assert_eq!(pushed, ["https://example.com/other.git"]);
        assert!(error
            .to_string()
            .contains("no Git remote points at this Maypop app"));
        assert_eq!(
            git_config(&directory, &format!("remote.{MAYPOP_REMOTE}.url"))
                .unwrap()
                .as_deref(),
            Some(origin.as_str())
        );
        assert!(
            git_config(&directory, &format!("remote.{MAYPOP_REMOTE}.pushurl"))
                .unwrap()
                .is_none()
        );

        std::fs::remove_dir_all(directory).unwrap();
        let _ = std::fs::remove_file(global);
    }

    #[test]
    fn api_origin_rejects_credentials_query_fragment_and_path() {
        assert!(api_origin("http://user@localhost:3000").is_err());
        assert!(api_origin("http://localhost:3000/?x=1").is_err());
        assert!(api_origin("http://localhost:3000/#frag").is_err());
        assert!(api_origin("http://localhost:3000/v1").is_err());
        assert_eq!(
            api_origin("http://localhost:3000/").unwrap(),
            "http://localhost:3000"
        );
    }

    #[test]
    fn publish_rejects_a_foreign_origin() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        configure_repository(&directory, api, app_id, "https://example.com/other.git").unwrap();

        assert_rejected_origin(&directory, api, expected, legacy, app_id);

        std::fs::remove_dir_all(directory).unwrap();
    }

    // A repository whose `origin` lives elsewhere (GitHub, GitLab) must still
    // publish through whichever remote points at the app.
    #[test]
    fn publish_finds_the_app_remote_beside_a_foreign_origin() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        run_git(
            &directory,
            &["remote", "add", "origin", "git@github.com:someone/game.git"],
        )
        .unwrap();
        configure_repository(&directory, api, app_id, expected).unwrap();

        assert_eq!(
            find_app_remote(&directory, api, expected, legacy, app_id).unwrap(),
            MAYPOP_REMOTE
        );
        assert_eq!(
            successful_git(&directory, &["remote", "get-url", "origin"]).unwrap(),
            "git@github.com:someone/game.git"
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_rejects_an_app_repo_path_on_an_unrelated_host() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        let foreign = format!("https://example.com/git/apps/{app_id}");
        configure_repository(&directory, api, app_id, &foreign).unwrap();

        assert_rejected_origin(&directory, api, expected, legacy, app_id);
        assert_eq!(origin_urls(&directory), (foreign.clone(), foreign));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_rejects_a_different_app_on_the_api_apps_host() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        let origin = format!("{api}/git/apps/other-app");
        configure_repository(&directory, api, app_id, &origin).unwrap();

        assert_rejected_origin(&directory, api, expected, legacy, app_id);
        assert_eq!(origin_urls(&directory), (origin.clone(), origin));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_rejects_an_api_apps_push_url_with_the_wrong_fetch_path() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        let push = format!("{api}/git/apps/{app_id}");
        configure_repository(&directory, api, app_id, "http://127.0.0.1:18080/other.git").unwrap();
        for fetch in [
            "http://127.0.0.1:18080/other.git".to_string(),
            "http://127.0.0.1:18080/git/apps/other-app".to_string(),
        ] {
            run_git(&directory, &["remote", "set-url", MAYPOP_REMOTE, &fetch]).unwrap();
            run_git(
                &directory,
                &["remote", "set-url", "--push", MAYPOP_REMOTE, &push],
            )
            .unwrap();
            assert_rejected_origin(&directory, api, expected, legacy, app_id);
            assert_eq!(origin_urls(&directory), (fetch, push.clone()));
        }

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_rejects_a_mismatched_push_url_on_an_accepted_fetch() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        let origin = format!("{api}/git/apps/{app_id}");
        configure_repository(&directory, api, app_id, &origin).unwrap();
        run_git(
            &directory,
            &[
                "remote",
                "set-url",
                "--push",
                MAYPOP_REMOTE,
                "https://example.com/other.git",
            ],
        )
        .unwrap();

        assert_rejected_origin(&directory, api, expected, legacy, app_id);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_rejects_a_mismatched_push_url_on_the_git_server_origin() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        configure_repository(&directory, api, app_id, expected).unwrap();
        for push in [
            "https://example.com/other.git".to_string(),
            format!("{api}/git/apps/{app_id}"),
        ] {
            run_git(&directory, &["remote", "set-url", MAYPOP_REMOTE, expected]).unwrap();
            run_git(
                &directory,
                &["remote", "set-url", "--push", MAYPOP_REMOTE, &push],
            )
            .unwrap();
            assert_rejected_origin(&directory, api, expected, legacy, app_id);
            assert_eq!(origin_urls(&directory), (expected.to_string(), push));
        }

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_rejects_an_extra_push_url() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let origin = format!("{api}/git/apps/{app_id}");
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        configure_repository(&directory, api, app_id, &origin).unwrap();
        run_git(
            &directory,
            &["remote", "set-url", "--push", MAYPOP_REMOTE, &origin],
        )
        .unwrap();
        run_git(
            &directory,
            &[
                "remote",
                "set-url",
                "--add",
                "--push",
                MAYPOP_REMOTE,
                "https://example.com/other.git",
            ],
        )
        .unwrap();
        let push_urls = successful_git(
            &directory,
            &["remote", "get-url", "--all", "--push", MAYPOP_REMOTE],
        )
        .unwrap();
        assert!(push_urls.lines().any(|line| line == origin));
        assert!(push_urls
            .lines()
            .any(|line| line == "https://example.com/other.git"));

        assert_rejected_origin(&directory, api, expected, legacy, app_id);
        assert_eq!(
            successful_git(&directory, &["remote", "get-url", MAYPOP_REMOTE]).unwrap(),
            origin
        );
        assert_eq!(
            successful_git(
                &directory,
                &["remote", "get-url", "--all", "--push", MAYPOP_REMOTE],
            )
            .unwrap(),
            push_urls
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_does_not_repair_legacy_fetch_when_push_url_differs() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        configure_repository(&directory, api, app_id, legacy).unwrap();
        run_git(
            &directory,
            &[
                "remote",
                "set-url",
                "--push",
                MAYPOP_REMOTE,
                "https://example.com/other.git",
            ],
        )
        .unwrap();

        assert_rejected_origin(&directory, api, expected, legacy, app_id);
        assert_eq!(
            origin_urls(&directory),
            (
                legacy.to_string(),
                "https://example.com/other.git".to_string()
            )
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_accepts_a_trailing_slash_on_the_app_repo_push_url() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let origin = format!("{api}/git/apps/{app_id}");
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        configure_repository(&directory, api, app_id, &origin).unwrap();
        run_git(
            &directory,
            &[
                "remote",
                "set-url",
                "--push",
                MAYPOP_REMOTE,
                &format!("{origin}/"),
            ],
        )
        .unwrap();

        find_app_remote(&directory, api, expected, legacy, app_id).unwrap();
        assert_eq!(
            origin_urls(&directory),
            (origin.clone(), format!("{origin}/"))
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publish_rejects_a_query_on_the_app_repo_remote() {
        let directory = temp_git_repo();
        let api = "http://localhost:3000";
        let app_id = "app_1";
        let expected = "http://localhost:3005/git/user_1/app_1";
        let legacy = "http://localhost:3000/git/user_1/app_1";
        let origin = format!("{api}/git/apps/{app_id}?x=1");
        configure_repository(&directory, api, app_id, &origin).unwrap();

        assert_rejected_origin(&directory, api, expected, legacy, app_id);
        assert_eq!(origin_urls(&directory), (origin.clone(), origin));

        std::fs::remove_dir_all(directory).unwrap();
    }

    fn example_login(api_url: &str, token: &str, git_user_id: &str) -> Credentials {
        Credentials {
            api_url: api_url.into(),
            token: token.into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
            user: CredentialUser {
                id: git_user_id.into(),
                username: git_user_id.into(),
                name: None,
                email: format!("{git_user_id}@example.com"),
                git_user_id: git_user_id.into(),
                git_server_url: format!("{api_url}/git"),
            },
        }
    }

    fn with_saved_profiles<T>(run: impl FnOnce() -> T) -> T {
        let _guard = CREDENTIAL_CONFIG.lock().unwrap();
        let directory =
            std::env::temp_dir().join(format!("maypop-cli-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        std::env::set_var("MAYPOP_CONFIG_DIR", &directory);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
        std::env::remove_var("MAYPOP_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(directory);
        result.unwrap()
    }

    #[test]
    fn credential_login_prefers_the_default_profile_for_the_app_repo_path() {
        with_saved_profiles(|| {
            let api = "http://localhost:3000";
            credentials::save("alpha", example_login(api, "token-alpha", "user_a")).unwrap();
            credentials::save("zeta", example_login(api, "token-zeta", "user_z")).unwrap();
            credentials::set_default("zeta").unwrap();

            let apps = credential_login(api, None, "git/apps/app_1", Some("app_1"))
                .unwrap()
                .unwrap();
            assert_eq!(apps.token, "token-zeta");

            let per_user = credential_login(api, None, "git/user_a/app_1", Some("app_1"))
                .unwrap()
                .unwrap();
            assert_eq!(per_user.token, "token-alpha");

            let named = credential_login(api, Some("alpha"), "git/apps/app_1", Some("app_1"))
                .unwrap()
                .unwrap();
            assert_eq!(named.token, "token-alpha");

            let missing =
                credential_login(api, Some("missing"), "git/apps/app_1", Some("app_1")).unwrap();
            assert!(missing.is_none());
        });
    }

    #[test]
    fn credential_helper_reads_the_checkout_app_id() {
        let directory = temp_git_repo();
        configure_repository(
            &directory,
            "http://localhost:3000",
            "app_1",
            "http://localhost:3000/git/apps/app_1",
        )
        .unwrap();
        let app_id = checkout_app_id(&directory).unwrap();
        assert_eq!(app_id, "app_1");
        let user = CredentialUser {
            id: "id".into(),
            username: "user".into(),
            name: None,
            email: "user@example.com".into(),
            git_user_id: "user_1".into(),
            git_server_url: "https://api.app.maypop.ai/git".into(),
        };
        assert!(credential_matches(&user, "git/apps/app_1", Some(&app_id)));
        assert!(!credential_matches(&user, "git/apps/other", Some(&app_id)));

        std::fs::remove_dir_all(directory).unwrap();
    }
}
