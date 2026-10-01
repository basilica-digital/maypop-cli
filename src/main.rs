mod ai;
mod auth;
mod bundle_upload;
mod commands;
mod credentials;
mod http;
mod mcp;
mod project;
mod references;
mod user_commands;

#[cfg(feature = "admin")]
use anyhow::bail;
use anyhow::Result;
use clap::Parser;
use commands::*;
#[cfg(feature = "admin")]
use reqwest::header;
use reqwest::Client;
#[cfg(feature = "admin")]
use serde_json::json;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(e) = run().await {
        eprintln!("Error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn run() -> Result<()> {
    let Cli {
        url,
        profile,
        token,
        command,
    } = Cli::parse();
    let profile_name = credentials::selected_profile(profile.as_deref())?;
    match command {
        Commands::Auth {
            no_browser,
            device_name,
        } => {
            let api_url = credentials::api_url_for(&profile_name, url.as_deref())?;
            let client = Client::new();
            let device_name = device_name.unwrap_or_else(auth::default_device_name);
            let credentials =
                auth::authenticate(&client, &api_url, &device_name, no_browser).await?;
            let username = credentials.user.username.clone();
            let path = credentials::save(&profile_name, credentials)?;
            println!("\nSigned in as @{username}.");
            println!("Profile: {profile_name}");
            println!("Credentials saved to {}", path.display());
            Ok(())
        }
        Commands::Init {
            path,
            name,
            description,
            visibility,
        } => {
            let api_url = credentials::api_url_for(&profile_name, url.as_deref())?;
            let selected_profile =
                endpoint_profile(profile.as_deref(), url.as_deref(), &profile_name);
            user_commands::init(
                &api_url,
                token.as_deref(),
                selected_profile,
                &path,
                name.as_deref(),
                description.as_deref(),
                visibility.as_deref(),
            )
            .await
        }
        Commands::Reference {
            command:
                ReferenceCommands::Import {
                    app_id,
                    path,
                    stdin,
                },
        } => {
            let api_url = credentials::api_url_for(&profile_name, url.as_deref())?;
            let selected_profile =
                endpoint_profile(profile.as_deref(), url.as_deref(), &profile_name);
            references::import(
                &api_url,
                selected_profile,
                token.as_deref(),
                app_id,
                &path,
                stdin,
            )
            .await
        }
        Commands::Publish { changelog } => {
            user_commands::publish(profile.as_deref(), token.as_deref(), changelog.as_deref()).await
        }
        Commands::Info => user_commands::info(profile.as_deref(), token.as_deref()).await,
        Commands::App {
            command: AppCommands::Apply,
        } => user_commands::apply(profile.as_deref(), token.as_deref()).await,
        Commands::Status => {
            let api_url = credentials::api_url_for(&profile_name, url.as_deref())?;
            let selected_profile =
                endpoint_profile(profile.as_deref(), url.as_deref(), &profile_name);
            user_commands::status(&api_url, selected_profile, token.as_deref()).await
        }
        Commands::Ai { command } => {
            let api_url = credentials::api_url_for(&profile_name, url.as_deref())?;
            let selected_profile =
                endpoint_profile(profile.as_deref(), url.as_deref(), &profile_name);
            match command {
                AiCommands::Image {
                    prompt,
                    output,
                    tier,
                    size,
                    force,
                } => {
                    ai::generate_image(
                        &api_url,
                        selected_profile,
                        token.as_deref(),
                        &prompt,
                        &output,
                        tier.as_str(),
                        &size,
                        force,
                    )
                    .await
                }
                AiCommands::Audio {
                    prompt,
                    output,
                    format,
                    force,
                } => {
                    ai::generate_audio(
                        &api_url,
                        selected_profile,
                        token.as_deref(),
                        &prompt,
                        &output,
                        format.map(AudioFormat::as_str),
                        force,
                    )
                    .await
                }
                AiCommands::Video {
                    prompt,
                    output,
                    model,
                    duration,
                    resolution,
                    ratio,
                    generate_audio,
                    seed,
                    force,
                } => {
                    ai::generate_video(
                        &api_url,
                        selected_profile,
                        token.as_deref(),
                        &prompt,
                        &output,
                        model.as_str(),
                        duration,
                        resolution.as_str(),
                        ratio.as_str(),
                        generate_audio,
                        seed,
                        force,
                    )
                    .await
                }
            }
        }
        Commands::Mcp { command } => match command {
            McpCommands::List { json } => {
                let api_url = credentials::api_url_for(&profile_name, url.as_deref())?;
                let selected_profile =
                    endpoint_profile(profile.as_deref(), url.as_deref(), &profile_name);
                mcp::list(&api_url, selected_profile, token.as_deref(), json).await
            }
            McpCommands::Connect {
                name,
                url: server_url,
                header_env,
            } => {
                let api_url = credentials::api_url_for(&profile_name, url.as_deref())?;
                let selected_profile =
                    endpoint_profile(profile.as_deref(), url.as_deref(), &profile_name);
                mcp::connect(
                    &api_url,
                    selected_profile,
                    token.as_deref(),
                    &name,
                    &server_url,
                    &header_env,
                )
                .await
            }
            McpCommands::Disconnect { integration } => {
                let api_url = credentials::api_url_for(&profile_name, url.as_deref())?;
                let selected_profile =
                    endpoint_profile(profile.as_deref(), url.as_deref(), &profile_name);
                mcp::disconnect(&api_url, selected_profile, token.as_deref(), &integration).await
            }
            McpCommands::Tools {
                integration,
                app,
                json,
            } => {
                if app {
                    mcp::app_tools(profile.as_deref(), token.as_deref(), &integration, json).await
                } else {
                    let api_url = credentials::api_url_for(&profile_name, url.as_deref())?;
                    let selected_profile =
                        endpoint_profile(profile.as_deref(), url.as_deref(), &profile_name);
                    mcp::tools(
                        &api_url,
                        selected_profile,
                        token.as_deref(),
                        &integration,
                        json,
                    )
                    .await
                }
            }
            McpCommands::Call {
                integration,
                tool,
                arguments,
                app,
            } => {
                if app {
                    mcp::app_call(
                        profile.as_deref(),
                        token.as_deref(),
                        &integration,
                        &tool,
                        &arguments,
                    )
                    .await
                } else {
                    let api_url = credentials::api_url_for(&profile_name, url.as_deref())?;
                    let selected_profile =
                        endpoint_profile(profile.as_deref(), url.as_deref(), &profile_name);
                    mcp::call(
                        &api_url,
                        selected_profile,
                        token.as_deref(),
                        &integration,
                        &tool,
                        &arguments,
                    )
                    .await
                }
            }
            McpCommands::Linked { json } => {
                mcp::linked(profile.as_deref(), token.as_deref(), json).await
            }
            McpCommands::Link { integration } => {
                mcp::link(profile.as_deref(), token.as_deref(), &integration).await
            }
            McpCommands::Unlink { integration } => {
                mcp::unlink(profile.as_deref(), token.as_deref(), &integration).await
            }
        },
        Commands::Profile {
            command: ProfileCommands::List,
        } => list_profiles(),
        Commands::Profile {
            command: ProfileCommands::SetDefault { name },
        } => {
            let path = credentials::set_default(&name)?;
            println!("Default profile: {name}");
            println!("Credentials saved to {}", path.display());
            Ok(())
        }
        Commands::GitCredential { operation } => {
            let api_url = credentials::api_url_for(&profile_name, url.as_deref())?;
            user_commands::git_credential(&operation, &api_url, profile.as_deref())
        }
        #[cfg(feature = "admin")]
        command => {
            let api_url = credentials::api_url_for(&profile_name, url.as_deref())?;
            let selected_profile =
                endpoint_profile(profile.as_deref(), url.as_deref(), &profile_name);
            run_admin(&api_url, selected_profile, token, command).await
        }
    }
}

fn endpoint_profile<'a>(
    explicit_profile: Option<&'a str>,
    explicit_url: Option<&str>,
    default_profile: &'a str,
) -> Option<&'a str> {
    explicit_profile.or_else(|| explicit_url.is_none().then_some(default_profile))
}

fn list_profiles() -> Result<()> {
    let profiles = credentials::profiles()?;
    if profiles.is_empty() {
        println!("No profiles configured. Run `maypop auth` to create one.");
        return Ok(());
    }
    for profile in profiles {
        let default = if profile.is_default { " (default)" } else { "" };
        println!(
            "{}{default}\t{}\t@{}",
            profile.name, profile.credentials.api_url, profile.credentials.user.username
        );
    }
    Ok(())
}

#[cfg(feature = "admin")]
async fn run_admin(
    url: &str,
    profile: Option<&str>,
    token_override: Option<String>,
    command: Commands,
) -> Result<()> {
    let token = match token_override {
        Some(token) => Some(token),
        None => credentials::token_for(url, profile)?,
    };
    let mut headers = header::HeaderMap::new();
    if let Some(token) = token {
        let auth_value = format!("Bearer {}", token);
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&auth_value)?,
        );
    }

    let client = Client::builder().default_headers(headers).build()?;

    match command {
        Commands::Health => {
            let res = client.get(format!("{url}/health")).send().await?;
            println!("Status: {}", res.status());
            println!("{}", res.text().await?);
        }
        Commands::Presign { cid } => {
            let payload = json!({ "cid": cid });
            let res = client
                .post(format!("{url}/uploads/presign"))
                .json(&payload)
                .send()
                .await?;
            println!("Status: {}", res.status());
            println!("{}", res.text().await?);
        }
        Commands::Pull { cookie } => {
            let cookie_val: serde_json::Value = cookie.map_or(json!(null), |c| {
                serde_json::from_str(&c).unwrap_or(json!(c))
            });

            let payload = json!({
                "profileID": "maypop-cli-profile",
                "clientID": "maypop-cli-client",
                "cookie": cookie_val,
                "pullVersion": 1
            });
            let res = client
                .post(format!("{url}/replicache/pull"))
                .json(&payload)
                .send()
                .await?;
            println!("Status: {}", res.status());
            // Pretty-print the JSON response
            if let Ok(json_res) = res.json::<serde_json::Value>().await {
                println!("{}", serde_json::to_string_pretty(&json_res)?);
            }
        }
        Commands::Explore { query, limit } => {
            let mut req = client.get(format!("{url}/apps/explore"));

            if let Some(q) = query {
                req = req.query(&[("q", q)]);
            }
            if let Some(l) = limit {
                req = req.query(&[("limit", l.to_string())]);
            }

            let res = req.send().await?;
            println!("Status: {}", res.status());
            if let Ok(json_res) = res.json::<serde_json::Value>().await {
                println!("{}", serde_json::to_string_pretty(&json_res)?);
            }
        }
        Commands::ExploreCustom {
            query,
            page,
            limit,
            debug,
        } => {
            let mut req = client.get(format!("{url}/apps/custom"));

            if let Some(q) = query {
                req = req.query(&[("q", q)]);
            }
            if let Some(p) = page {
                req = req.query(&[("page", p.to_string())]);
            }
            if let Some(l) = limit {
                req = req.query(&[("limit", l.to_string())]);
            }
            if debug {
                req = req.query(&[("debug", "true")]);
            }

            let res = req.send().await?;
            println!("Status: {}", res.status());
            if let Ok(json_res) = res.json::<serde_json::Value>().await {
                println!("{}", serde_json::to_string_pretty(&json_res)?);
            }
        }
        Commands::CreateApp {
            name,
            description,
            content_cid,
            visibility,
            id,
            bundle_path,
            bundle_entry,
        } => {
            let actual_cid = if let Some(cid) = content_cid {
                cid
            } else if let Some(path) = bundle_path {
                bundle_upload::upload_directory(
                    &client,
                    url,
                    std::path::Path::new(&path),
                    &bundle_entry,
                    bundle_upload::Routing::Spa,
                )
                .await?
            } else {
                bail!("content CID or bundle path required");
            };

            let app_id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let client_group_id = uuid::Uuid::new_v4().to_string();
            let client_id = uuid::Uuid::new_v4().to_string();

            let payload = json!({
                "pushVersion": 1,
                "profileID": "maypop-cli-profile",
                "clientGroupID": client_group_id,
                "mutations": [
                    {
                        "clientID": client_id,
                        "id": 1, // Optimistic mutation ID
                        "name": "createApp",
                        "args": {
                            "id": app_id,
                            "name": name,
                            "description": description,
                            "contentCid": actual_cid,
                            "visibility": visibility
                        }
                    }
                ]
            });
            let res = client
                .post(format!("{url}/replicache/push"))
                .json(&payload)
                .send()
                .await?;

            if !res.status().is_success() {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                bail!("server returned status {status}\n{body}\n\nHint: run `maypop auth` first");
            }
            println!("Status: {}", res.status());
            println!("Created App ID: {}", app_id);
        }
        Commands::Quality { id, recompute } => {
            let mut req = client.get(format!("{url}/apps/{id}/quality"));

            if recompute {
                req = req.query(&[("recompute", "true")]);
            }

            let res = req.send().await?;
            println!("Status: {}", res.status());
            if let Ok(json_res) = res.json::<serde_json::Value>().await {
                println!("{}", serde_json::to_string_pretty(&json_res)?);
            }
        }
        Commands::VisualQuality {
            id,
            width,
            height,
            full_page,
            recompute,
            model,
        } => {
            let mut req = client.get(format!("{url}/apps/{id}/visual-quality"));
            if let Some(w) = width {
                req = req.query(&[("width", w.to_string())]);
            }
            if let Some(h) = height {
                req = req.query(&[("height", h.to_string())]);
            }
            if full_page {
                req = req.query(&[("fullPage", "true")]);
            }
            if recompute {
                req = req.query(&[("recompute", "true")]);
            }
            if let Some(m) = model {
                req = req.query(&[("model", m)]);
            }

            let res = req.send().await?;
            println!("Status: {}", res.status());
            if let Ok(json_res) = res.json::<serde_json::Value>().await {
                println!("{}", serde_json::to_string_pretty(&json_res)?);
            }
        }
        Commands::Screenshot {
            id,
            output,
            width,
            height,
            full_page,
        } => {
            let mut req = client.get(format!("{url}/apps/{id}/screenshot"));
            if let Some(w) = width {
                req = req.query(&[("width", w.to_string())]);
            }
            if let Some(h) = height {
                req = req.query(&[("height", h.to_string())]);
            }
            if full_page {
                req = req.query(&[("fullPage", "true")]);
            }

            let res = req.send().await?;
            if !res.status().is_success() {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                bail!("server returned status {status}\n{body}\n\nHint: run `maypop auth` first");
            }
            println!("Status: {}", res.status());

            let path = output.unwrap_or_else(|| format!("{id}.png"));
            let bytes = res.bytes().await?;
            std::fs::write(&path, &bytes)?;
            println!("Wrote {} bytes to {}", bytes.len(), path);
        }
        Commands::UploadBundle { path, entry } => {
            let bundle_id = bundle_upload::upload_directory(
                &client,
                url,
                std::path::Path::new(&path),
                &entry,
                bundle_upload::Routing::Spa,
            )
            .await?;
            println!("Successfully uploaded and confirmed bundle!");
            println!("Bundle ID: {}", bundle_id);
        }
        _ => unreachable!("public commands are handled before admin dispatch"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[test]
    fn default_binary_exposes_only_the_product_workflows() {
        let command = Cli::command();
        assert_eq!(command.get_version(), Some(env!("CARGO_PKG_VERSION")));
        let visible = command
            .get_subcommands()
            .filter(|subcommand| !subcommand.is_hide_set())
            .map(|subcommand| subcommand.get_name())
            .filter(|name| *name != "help")
            .collect::<Vec<_>>();

        #[cfg(not(feature = "admin"))]
        assert_eq!(
            visible,
            [
                "auth",
                "init",
                "reference",
                "publish",
                "info",
                "app",
                "status",
                "ai",
                "mcp",
                "profile"
            ]
        );
        #[cfg(feature = "admin")]
        for required in [
            "auth",
            "init",
            "reference",
            "publish",
            "info",
            "app",
            "status",
            "ai",
            "mcp",
            "profile",
        ] {
            assert!(visible.contains(&required));
        }
    }

    #[test]
    fn global_profile_and_url_are_accepted_after_auth() {
        let cli = Cli::try_parse_from([
            "maypop",
            "auth",
            "--profile",
            "local",
            "--url",
            "http://localhost:3000",
            "--no-browser",
        ])
        .unwrap();

        assert_eq!(cli.profile.as_deref(), Some("local"));
        assert_eq!(cli.url.as_deref(), Some("http://localhost:3000"));
        assert!(matches!(
            cli.command,
            Commands::Auth {
                no_browser: true,
                ..
            }
        ));
    }

    #[test]
    fn init_metadata_flags_are_optional_overrides() {
        let cli = Cli::try_parse_from(["maypop", "init"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Init {
                name: None,
                description: None,
                visibility: None,
                ..
            }
        ));

        let cli = Cli::try_parse_from([
            "maypop",
            "init",
            "--name",
            "CLI app",
            "--description",
            "CLI description",
            "--visibility",
            "public",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Init {
                name: Some(name),
                description: Some(description),
                visibility: Some(visibility),
                ..
            } if name == "CLI app" && description == "CLI description" && visibility == "public"
        ));
    }

    #[test]
    fn mcp_tools_and_calls_accept_app_scoped_agent_options() {
        let tools =
            Cli::try_parse_from(["maypop", "mcp", "docs", "search", "--app", "--json"]).unwrap();
        assert!(matches!(
            tools.command,
            Commands::Mcp {
                command: McpCommands::Tools {
                    integration,
                    app: true,
                    json: true,
                }
            } if integration == "search"
        ));

        let call = Cli::try_parse_from([
            "maypop",
            "mcp",
            "call",
            "search",
            "web_search",
            "--app",
            "--arguments",
            r#"{"query":"Maypop SDK"}"#,
        ])
        .unwrap();
        assert!(matches!(
            call.command,
            Commands::Mcp {
                command: McpCommands::Call {
                    integration,
                    tool,
                    arguments,
                    app: true,
                }
            } if integration == "search"
                && tool == "web_search"
                && arguments == r#"{"query":"Maypop SDK"}"#
        ));
    }

    #[test]
    fn ai_media_commands_parse_agent_facing_options() {
        let image = Cli::try_parse_from([
            "maypop",
            "ai",
            "image",
            "--prompt",
            "A paper garden",
            "--output",
            "Images/hero.png",
            "--tier",
            "quality",
            "--size",
            "2048x1536",
        ])
        .unwrap();
        assert!(matches!(
            image.command,
            Commands::Ai {
                command: AiCommands::Image {
                    prompt,
                    output,
                    tier,
                    size,
                    force: false,
                }
            } if prompt == "A paper garden"
                && output == std::path::Path::new("Images/hero.png")
                && tier == ImageTier::Quality
                && size == "2048x1536"
        ));

        let video = Cli::try_parse_from([
            "maypop",
            "ai",
            "video",
            "--prompt",
            "Clouds moving over a city",
            "--output",
            "Video/intro.mp4",
            "--model",
            "quality",
            "--duration",
            "20",
            "--ratio",
            "21:9",
            "--generate-audio",
        ])
        .unwrap();
        assert!(matches!(
            video.command,
            Commands::Ai {
                command: AiCommands::Video {
                    model,
                    duration: 20,
                    ratio,
                    generate_audio: true,
                    ..
                }
            } if model == VideoModel::Quality && ratio == VideoRatio::TwentyOneNine
        ));
    }
}
