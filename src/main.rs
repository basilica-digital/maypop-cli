mod ai;
mod auth;
mod bundle_upload;
mod credentials;
mod http;
mod mcp;
mod project;
mod user_commands;

#[cfg(feature = "admin")]
use anyhow::bail;
use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
#[cfg(feature = "admin")]
use reqwest::header;
use reqwest::Client;
#[cfg(feature = "admin")]
use serde_json::json;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "maypop")]
#[command(version)]
#[command(about = "Build and manage Maypop apps", long_about = None)]
struct Cli {
    /// Base URL override for the Maypop backend
    #[arg(short, long, env = "MAYPOP_URL", global = true)]
    url: Option<String>,

    /// Named profile to use instead of the configured default
    #[arg(long, env = "MAYPOP_PROFILE", global = true)]
    profile: Option<String>,

    /// Access token override; defaults to the token saved by `maypop auth`
    #[arg(short, long, env = "MAYPOP_TOKEN", global = true)]
    token: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Sign in to Maypop through your browser
    Auth {
        /// Print the approval URL without opening a browser
        #[arg(long)]
        no_browser: bool,
        /// Name shown on the browser approval page
        #[arg(long)]
        device_name: Option<String>,
    },
    /// Create an app and configure its Git remote and framework adapter
    Init {
        /// Directory to initialize
        #[arg(default_value = ".")]
        path: PathBuf,
        /// App name; overrides maypop.toml and otherwise defaults to the directory name
        #[arg(short, long)]
        name: Option<String>,
        /// App description; overrides maypop.toml
        #[arg(short, long)]
        description: Option<String>,
        /// Initial app visibility; overrides maypop.toml and otherwise defaults to private
        #[arg(long, value_parser = ["private", "unlisted", "public"])]
        visibility: Option<String>,
    },
    /// Build the app and publish it with the current Git HEAD
    Publish {
        /// Optional release note for this version
        #[arg(long)]
        changelog: Option<String>,
    },
    /// Show the current app
    Info,
    /// Manage the app connected to the current repository
    App {
        #[command(subcommand)]
        command: AppCommands,
    },
    /// Show backend health and the authenticated account
    Status,
    /// Generate media with the authenticated Maypop account
    Ai {
        #[command(subcommand)]
        command: AiCommands,
    },
    /// Connect MCP servers and manage their access to apps
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },
    /// List profiles and select the default
    Profile {
        #[command(subcommand)]
        command: ProfileCommands,
    },
    /// Git credential-helper protocol; configured automatically by `maypop init`
    #[command(hide = true)]
    GitCredential { operation: String },
    #[cfg(feature = "admin")]
    /// Check the liveness probe of the backend
    Health,
    #[cfg(feature = "admin")]
    /// Get a signed PUT URL for a direct-to-GCS upload
    Presign {
        /// The Content ID (SHA-256 hex string) of the file
        cid: String,
    },
    #[cfg(feature = "admin")]
    /// Pull data from the Replicache sync engine
    Pull {
        /// The Replicache client cookie (defaults to null for a fresh sync)
        #[arg(short, long)]
        cookie: Option<String>,
    },
    #[cfg(feature = "admin")]
    /// Explore public apps in the catalog
    Explore {
        /// Search substring match against name and description
        #[arg(short, long)]
        query: Option<String>,
        /// Page size limit (defaults to 24 on the server)
        #[arg(short, long)]
        limit: Option<u64>,
    },
    #[cfg(feature = "admin")]
    /// Explore public apps using the custom ranking endpoint
    ExploreCustom {
        /// Search substring match against name and description
        #[arg(short, long)]
        query: Option<String>,
        /// Page number to fetch (0-indexed)
        #[arg(short, long)]
        page: Option<u64>,
        /// Page size limit
        #[arg(short, long)]
        limit: Option<u64>,
        /// Request debug information
        #[arg(short, long)]
        debug: bool,
    },
    #[cfg(feature = "admin")]
    /// Create a new app via the Replicache push endpoint
    CreateApp {
        /// The name of the app
        #[arg(short, long)]
        name: String,
        /// The description of the app
        #[arg(short, long)]
        description: Option<String>,
        /// The content CID (bundle ID). If omitted, uploads a dummy bundle automatically!
        #[arg(short = 'c', long)]
        content_cid: Option<String>,
        /// Visibility: "public" or "private"
        #[arg(short, long, default_value = "private")]
        visibility: String,
        /// Optional UUID for the new app (auto-generated if omitted)
        #[arg(long)]
        id: Option<String>,
        /// Path to a local directory to upload as the bundle content
        #[arg(long)]
        bundle_path: Option<String>,
        /// Entry point file within the bundle (defaults to index.html)
        #[arg(long, default_value = "index.html")]
        bundle_entry: String,
    },
    #[cfg(feature = "admin")]
    /// Assess the quality of a public app
    Quality {
        /// The app ID (UUID)
        id: String,
        /// Force re-grading, ignoring any cached score
        #[arg(short, long)]
        recompute: bool,
    },
    #[cfg(feature = "admin")]
    /// Audit the visual quality of a public app.
    ///
    /// Renders the app's current bundle to a screenshot on the server and
    /// runs a vision model over it, returning a structured UI/UX design
    /// critique (per-principle scores, justifications, and improvements).
    VisualQuality {
        /// The app ID (UUID)
        id: String,
        /// Viewport width in CSS pixels (default 1280)
        #[arg(long)]
        width: Option<u32>,
        /// Viewport height in CSS pixels (default 800)
        #[arg(long)]
        height: Option<u32>,
        /// Audit the full scrollable page, not just the viewport
        #[arg(long)]
        full_page: bool,
        /// Force re-grading, ignoring any cached audit
        #[arg(short, long)]
        recompute: bool,
        /// Vision model to grade with: a tier name ("fast"/"smart") or a raw
        /// provider model id. Defaults to the "smart" tier server-side.
        #[arg(short, long)]
        model: Option<String>,
    },
    #[cfg(feature = "admin")]
    /// Save a PNG screenshot of a public app.
    ///
    /// Renders the app's current bundle on the server (static preview —
    /// app-session data won't load) and writes the resulting PNG to disk.
    Screenshot {
        /// The app ID (UUID)
        id: String,
        /// Output path for the PNG (defaults to <id>.png)
        #[arg(short, long)]
        output: Option<String>,
        /// Viewport width in CSS pixels (default 1280)
        #[arg(long)]
        width: Option<u32>,
        /// Viewport height in CSS pixels (default 800)
        #[arg(long)]
        height: Option<u32>,
        /// Capture the full scrollable page, not just the viewport
        #[arg(long)]
        full_page: bool,
    },
    #[cfg(feature = "admin")]
    /// Upload a local directory as a bundle to GCS
    UploadBundle {
        /// Path to the local directory to upload
        #[arg(short, long)]
        path: String,
        /// Entry point file within the bundle (defaults to index.html)
        #[arg(short, long, default_value = "index.html")]
        entry: String,
    },
}

#[derive(Subcommand)]
enum AppCommands {
    /// Apply the [app] table from maypop.toml to Maypop
    Apply,
}

#[derive(Subcommand)]
enum ProfileCommands {
    /// List saved profiles
    List,
    /// Select the default profile used when `--profile` is omitted
    #[command(alias = "use")]
    SetDefault {
        /// Existing profile name
        name: String,
    },
}

#[derive(Subcommand)]
enum AiCommands {
    /// Generate a PNG image
    Image {
        /// Description of the image to generate
        #[arg(long)]
        prompt: String,
        /// Destination PNG path
        #[arg(short, long)]
        output: PathBuf,
        /// Generation tier
        #[arg(long, value_enum, default_value = "fast")]
        tier: ImageTier,
        /// Resolution preset or WIDTHxHEIGHT pixels
        #[arg(long, default_value = "2K")]
        size: String,
        /// Replace an existing destination file
        #[arg(long)]
        force: bool,
    },
    /// Generate an MP3 or WAV audio file
    Audio {
        /// Description of the audio to generate
        #[arg(long)]
        prompt: String,
        /// Destination .mp3 or .wav path
        #[arg(short, long)]
        output: PathBuf,
        /// Audio format; defaults to the destination extension
        #[arg(long, value_enum)]
        format: Option<AudioFormat>,
        /// Replace an existing destination file
        #[arg(long)]
        force: bool,
    },
    /// Generate an MP4 video
    Video {
        /// Description of the scene, motion, camera, and sound
        #[arg(long)]
        prompt: String,
        /// Destination MP4 path
        #[arg(short, long)]
        output: PathBuf,
        /// Generation model
        #[arg(long, value_enum, default_value = "fast")]
        model: VideoModel,
        /// Duration in seconds; fast supports up to 15, quality up to 30
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(4..=30))]
        duration: u32,
        /// Output resolution
        #[arg(long, value_enum, default_value = "720p")]
        resolution: VideoResolution,
        /// Output aspect ratio
        #[arg(long, value_enum, default_value = "16:9")]
        ratio: VideoRatio,
        /// Include synchronized audio
        #[arg(long)]
        generate_audio: bool,
        /// Optional deterministic provider seed
        #[arg(long)]
        seed: Option<i32>,
        /// Replace an existing destination file
        #[arg(long)]
        force: bool,
    },
}

#[derive(Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum ImageTier {
    #[default]
    Fast,
    Quality,
}

impl ImageTier {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Quality => "quality",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum AudioFormat {
    Mp3,
    Wav,
}

impl AudioFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum VideoModel {
    #[default]
    Fast,
    Quality,
}

impl VideoModel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Quality => "quality",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum VideoResolution {
    #[value(name = "480p")]
    P480,
    #[default]
    #[value(name = "720p")]
    P720,
}

impl VideoResolution {
    fn as_str(self) -> &'static str {
        match self {
            Self::P480 => "480p",
            Self::P720 => "720p",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum VideoRatio {
    #[default]
    #[value(name = "16:9")]
    SixteenNine,
    #[value(name = "9:16")]
    NineSixteen,
    #[value(name = "4:3")]
    FourThree,
    #[value(name = "3:4")]
    ThreeFour,
    #[value(name = "1:1")]
    OneOne,
    #[value(name = "21:9")]
    TwentyOneNine,
}

impl VideoRatio {
    fn as_str(self) -> &'static str {
        match self {
            Self::SixteenNine => "16:9",
            Self::NineSixteen => "9:16",
            Self::FourThree => "4:3",
            Self::ThreeFour => "3:4",
            Self::OneOne => "1:1",
            Self::TwentyOneNine => "21:9",
        }
    }
}

#[derive(Subcommand)]
enum McpCommands {
    /// List MCP servers connected to your account
    List {
        /// Print the API response as JSON
        #[arg(long)]
        json: bool,
    },
    /// Connect a custom MCP server to your account
    Connect {
        /// Account-local name for the server
        name: String,
        /// HTTPS MCP endpoint
        url: String,
        /// Read an HTTP header from an environment variable (HEADER=ENV_VAR)
        #[arg(long = "header-env", value_name = "HEADER=ENV_VAR")]
        header_env: Vec<String>,
    },
    /// Disconnect an MCP server from your account
    Disconnect {
        /// Integration ID or unique name
        integration: String,
    },
    /// Show live tool documentation and input schemas
    #[command(visible_alias = "docs")]
    Tools {
        /// Integration ID or unique name
        integration: String,
        /// Test through the current app's linked integration
        #[arg(long)]
        app: bool,
        /// Print the unmodified MCP tool-list result as JSON
        #[arg(long)]
        json: bool,
    },
    /// Invoke a tool and print its raw MCP result
    Call {
        /// Integration ID or unique name
        integration: String,
        /// Tool name from `maypop mcp tools`
        tool: String,
        /// JSON object matching the tool's input schema
        #[arg(long, default_value = "{}", value_name = "JSON")]
        arguments: String,
        /// Test through the current app's linked integration
        #[arg(long)]
        app: bool,
    },
    /// List MCP servers linked to the current app
    Linked {
        /// Print the API response as JSON
        #[arg(long)]
        json: bool,
    },
    /// Give the current app access to one of your MCP servers
    Link {
        /// Integration ID or unique name
        integration: String,
    },
    /// Remove the current app's access to an MCP server
    Unlink {
        /// Integration ID or unique name
        integration: String,
    },
}

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
            ["auth", "init", "publish", "info", "app", "status", "ai", "mcp", "profile"]
        );
        #[cfg(feature = "admin")]
        for required in [
            "auth", "init", "publish", "info", "app", "status", "ai", "mcp", "profile",
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
