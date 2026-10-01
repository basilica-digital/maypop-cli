//! Command definitions for Maypop product and administrative workflows.

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "maypop")]
#[command(version)]
#[command(about = "Build and manage Maypop apps", long_about = None)]
pub(crate) struct Cli {
    /// Base URL override for the Maypop backend
    #[arg(short, long, env = "MAYPOP_URL", global = true)]
    pub(crate) url: Option<String>,

    /// Named profile to use instead of the configured default
    #[arg(long, env = "MAYPOP_PROFILE", global = true)]
    pub(crate) profile: Option<String>,

    /// Access token override; defaults to the token saved by `maypop auth`
    #[arg(short, long, env = "MAYPOP_TOKEN", global = true)]
    pub(crate) token: Option<String>,

    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
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
pub(crate) enum AppCommands {
    /// Apply the [app] table from maypop.toml to Maypop
    Apply,
}

#[derive(Subcommand)]
pub(crate) enum ProfileCommands {
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
pub(crate) enum AiCommands {
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
pub(crate) enum ImageTier {
    #[default]
    Fast,
    Quality,
}

impl ImageTier {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Quality => "quality",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum AudioFormat {
    Mp3,
    Wav,
}

impl AudioFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum VideoModel {
    #[default]
    Fast,
    Quality,
}

impl VideoModel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Quality => "quality",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum VideoResolution {
    #[value(name = "480p")]
    P480,
    #[default]
    #[value(name = "720p")]
    P720,
}

impl VideoResolution {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::P480 => "480p",
            Self::P720 => "720p",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum VideoRatio {
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
    pub(crate) fn as_str(self) -> &'static str {
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
pub(crate) enum McpCommands {
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
