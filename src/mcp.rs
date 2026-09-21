//! Authenticated MCP connection and app-link management.

use crate::http as http_request;
use crate::user_commands::{app_connection, client, required_token, successful_json};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UserIntegration {
    id: String,
    user_id: String,
    name: String,
    url: String,
    auth_kind: String,
    needs_reauth: bool,
    enabled: bool,
    toolkit_slug: Option<String>,
    logo_url: Option<String>,
    account: Option<String>,
    created_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppIntegration {
    id: String,
    app_id: String,
    name: String,
    auth_kind: String,
    needs_reauth: bool,
    toolkit_slug: Option<String>,
    logo_url: Option<String>,
    account: Option<String>,
    linked_by: String,
}

trait IntegrationIdentity {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
}

impl IntegrationIdentity for UserIntegration {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl IntegrationIdentity for AppIntegration {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// List the MCP servers in the authenticated user's integration vault.
pub(crate) async fn list(
    api_url: &str,
    profile: Option<&str>,
    explicit_token: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let http = account_client(api_url, profile, explicit_token)?;
    let integrations = list_personal(&http, api_url).await?;
    print_personal(&integrations, json_output)
}

/// Connect and verify a custom MCP server using secrets sourced from the environment.
pub(crate) async fn connect(
    api_url: &str,
    profile: Option<&str>,
    explicit_token: Option<&str>,
    name: &str,
    server_url: &str,
    header_env: &[String],
) -> Result<()> {
    let http = account_client(api_url, profile, explicit_token)?;
    let headers = read_headers(header_env)?;
    let response = http_request::json(
        http.post(format!("{}/me/integrations", trim_url(api_url))),
        &json!({ "name": name, "url": server_url, "headers": headers }),
    )?
    .send()
    .await
    .context("could not connect the MCP server")?;
    let integration = successful_json::<UserIntegration>(response).await?;
    println!("Connected {} ({}).", integration.name, integration.id);
    println!(
        "Link it to the current app with `maypop mcp link {}`.",
        integration.id
    );
    Ok(())
}

/// Disconnect a personal MCP server and remove its app links.
pub(crate) async fn disconnect(
    api_url: &str,
    profile: Option<&str>,
    explicit_token: Option<&str>,
    selector: &str,
) -> Result<()> {
    let http = account_client(api_url, profile, explicit_token)?;
    let integrations = list_personal(&http, api_url).await?;
    let integration = select(&integrations, selector)?;
    let response = http_request::empty(http.delete(format!(
        "{}/me/integrations/{}",
        trim_url(api_url),
        integration.id
    )))
    .send()
    .await
    .context("could not disconnect the MCP server")?;
    successful_json::<Value>(response).await?;
    println!("Disconnected {} ({}).", integration.name, integration.id);
    Ok(())
}

/// List MCP servers linked to the app in the current repository.
pub(crate) async fn linked(
    profile: Option<&str>,
    explicit_token: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let connection = app_connection(profile, explicit_token)?;
    let integrations =
        list_linked(&connection.http, &connection.api_url, &connection.app_id).await?;
    print_linked(&integrations, json_output)
}

/// Link one of the authenticated user's MCP servers to the current app.
pub(crate) async fn link(
    profile: Option<&str>,
    explicit_token: Option<&str>,
    selector: &str,
) -> Result<()> {
    let connection = app_connection(profile, explicit_token)?;
    let integrations = list_personal(&connection.http, &connection.api_url).await?;
    let integration = select(&integrations, selector)?;
    let response = http_request::json(
        connection.http.post(format!(
            "{}/apps/{}/integrations",
            trim_url(&connection.api_url),
            connection.app_id
        )),
        &json!({ "userIntegrationId": integration.id }),
    )?
    .send()
    .await
    .context("could not link the MCP server")?;
    let linked = successful_json::<AppIntegration>(response).await?;
    println!("Linked {} ({}) to this app.", linked.name, linked.id);
    Ok(())
}

/// Unlink an MCP server from the current app without disconnecting the account connection.
pub(crate) async fn unlink(
    profile: Option<&str>,
    explicit_token: Option<&str>,
    selector: &str,
) -> Result<()> {
    let connection = app_connection(profile, explicit_token)?;
    let integrations =
        list_linked(&connection.http, &connection.api_url, &connection.app_id).await?;
    let integration = select(&integrations, selector)?;
    let response = http_request::empty(connection.http.delete(format!(
        "{}/apps/{}/integrations/{}",
        trim_url(&connection.api_url),
        connection.app_id,
        integration.id
    )))
    .send()
    .await
    .context("could not unlink the MCP server")?;
    successful_json::<Value>(response).await?;
    println!(
        "Unlinked {} ({}) from this app.",
        integration.name, integration.id
    );
    Ok(())
}

fn account_client(
    api_url: &str,
    profile: Option<&str>,
    explicit_token: Option<&str>,
) -> Result<reqwest::Client> {
    let token = required_token(api_url, profile, explicit_token)?;
    client(Some(&token))
}

async fn list_personal(http: &reqwest::Client, api_url: &str) -> Result<Vec<UserIntegration>> {
    let response = http
        .get(format!("{}/me/integrations", trim_url(api_url)))
        .send()
        .await
        .context("could not list MCP servers")?;
    successful_json(response).await
}

async fn list_linked(
    http: &reqwest::Client,
    api_url: &str,
    app_id: &str,
) -> Result<Vec<AppIntegration>> {
    let response = http
        .get(format!("{}/apps/{app_id}/integrations", trim_url(api_url)))
        .send()
        .await
        .context("could not list this app's MCP servers")?;
    successful_json(response).await
}

fn select<'a, T: IntegrationIdentity>(items: &'a [T], selector: &str) -> Result<&'a T> {
    if let Some(item) = items.iter().find(|item| item.id() == selector) {
        return Ok(item);
    }
    let named = items
        .iter()
        .filter(|item| item.name().eq_ignore_ascii_case(selector))
        .collect::<Vec<_>>();
    match named.as_slice() {
        [item] => Ok(*item),
        [] => bail!("no MCP server matches `{selector}`"),
        _ => bail!("more than one MCP server is named `{selector}`; use its ID"),
    }
}

fn read_headers(bindings: &[String]) -> Result<BTreeMap<String, String>> {
    bindings
        .iter()
        .try_fold(BTreeMap::new(), |mut headers, binding| {
            let (header, variable) = parse_header_binding(binding)?;
            let value = std::env::var(variable)
                .with_context(|| format!("environment variable `{variable}` is not set"))?;
            if headers.insert(header.to_string(), value).is_some() {
                bail!("header `{header}` is configured more than once");
            }
            Ok(headers)
        })
}

fn parse_header_binding(binding: &str) -> Result<(&str, &str)> {
    let Some((header, variable)) = binding.split_once('=') else {
        bail!("header binding `{binding}` must use HEADER=ENV_VAR");
    };
    if header.trim().is_empty() || variable.trim().is_empty() {
        bail!("header binding `{binding}` must use non-empty names");
    }
    Ok((header.trim(), variable.trim()))
}

fn print_personal(integrations: &[UserIntegration], json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(integrations)?);
        return Ok(());
    }
    if integrations.is_empty() {
        println!("No MCP servers connected.");
        return Ok(());
    }
    for integration in integrations {
        let status = if integration.needs_reauth {
            "needs reauthentication"
        } else if integration.enabled {
            "ready"
        } else {
            "disabled"
        };
        println!(
            "{}\t{}\t{}\t{}",
            integration.id, integration.name, integration.auth_kind, status
        );
    }
    Ok(())
}

fn print_linked(integrations: &[AppIntegration], json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(integrations)?);
        return Ok(());
    }
    if integrations.is_empty() {
        println!("No MCP servers linked to this app.");
        return Ok(());
    }
    for integration in integrations {
        let status = if integration.needs_reauth {
            "needs reauthentication"
        } else {
            "ready"
        };
        println!(
            "{}\t{}\t{}\t{}",
            integration.id, integration.name, integration.auth_kind, status
        );
    }
    Ok(())
}

fn trim_url(url: &str) -> &str {
    url.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn integration(id: &str, name: &str) -> AppIntegration {
        AppIntegration {
            id: id.into(),
            app_id: "app".into(),
            name: name.into(),
            auth_kind: "headers".into(),
            needs_reauth: false,
            toolkit_slug: None,
            logo_url: None,
            account: None,
            linked_by: "user".into(),
        }
    }

    #[test]
    fn integration_selectors_prefer_ids_and_accept_unique_names() {
        let items = [
            integration("alpha", "Search"),
            integration("search", "Other"),
        ];

        assert_eq!(select(&items, "search").unwrap().name, "Other");
        assert_eq!(select(&items, "SEARCH").unwrap().id, "alpha");
    }

    #[test]
    fn duplicate_names_require_an_id() {
        let items = [integration("one", "Search"), integration("two", "search")];

        assert!(select(&items, "SEARCH")
            .unwrap_err()
            .to_string()
            .contains("use its ID"));
    }

    #[test]
    fn header_bindings_require_two_names() {
        assert_eq!(
            parse_header_binding(" Authorization = MCP_TOKEN ").unwrap(),
            ("Authorization", "MCP_TOKEN")
        );
        assert!(parse_header_binding("Authorization").is_err());
        assert!(parse_header_binding("=MCP_TOKEN").is_err());
    }
}
