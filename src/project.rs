//! Project configuration and framework-specific production build adapters.

use crate::bundle_upload::Routing;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use toml_edit::{value, Array, DocumentMut, Item, Table};

const CONFIG_FILE: &str = "maypop.toml";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Framework {
    Auto,
    Vite,
    Next,
    Rsbuild,
    Static,
}

impl Framework {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "vite" => Ok(Self::Vite),
            "next" => Ok(Self::Next),
            "rsbuild" => Ok(Self::Rsbuild),
            "static" => Ok(Self::Static),
            _ => bail!("unknown build.framework {value:?} in maypop.toml"),
        }
    }
}

impl fmt::Display for Framework {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::Vite => "vite",
            Self::Next => "next",
            Self::Rsbuild => "rsbuild",
            Self::Static => "static",
        })
    }
}

#[derive(Debug)]
struct ProjectConfig {
    build: BuildConfig,
}

/// App metadata that can be applied explicitly with `maypop app apply`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AppConfig {
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) visibility: Option<String>,
    pub(crate) link_access: Option<String>,
    pub(crate) allow_remixing: Option<bool>,
    pub(crate) tags: Option<Vec<String>>,
    pub(crate) thumbnail: Option<PathBuf>,
}

#[derive(Debug)]
struct BuildConfig {
    framework: Framework,
    command: Option<Vec<String>>,
    output: Option<PathBuf>,
    entry: Option<String>,
}

/// A resolved framework build ready to execute and upload.
pub(crate) struct BuildPlan {
    pub(crate) framework: String,
    pub(crate) output_directory: PathBuf,
    pub(crate) entry: String,
    pub(crate) routing: Routing,
}

/// Create `maypop.toml` without replacing existing project configuration.
pub(crate) fn create_config(repository: &Path, app: Option<&AppConfig>) -> Result<PathBuf> {
    let path = repository.join(CONFIG_FILE);
    if path.exists() {
        load_config(repository)?;
        let mut document = read_document(&path)?;
        if document.get("app").is_none() {
            if let Some(app) = app {
                document["app"] = Item::Table(app_table(app));
                std::fs::write(&path, document.to_string())
                    .with_context(|| format!("could not update {}", path.display()))?;
            }
        } else {
            parse_app_config(&document)?;
        }
        return Ok(path);
    }
    let framework = detect_framework(repository)?.unwrap_or(Framework::Auto);
    let mut document = DocumentMut::new();
    if let Some(app) = app {
        document["app"] = Item::Table(app_table(app));
    }
    let mut build = Table::new();
    build["framework"] = value(framework.to_string());
    document["build"] = Item::Table(build);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("could not create {}", path.display()))?;
    file.write_all(document.to_string().as_bytes())?;
    Ok(path)
}

/// Read the optional declarative app metadata from `maypop.toml`.
pub(crate) fn app_config(repository: &Path) -> Result<Option<AppConfig>> {
    let path = repository.join(CONFIG_FILE);
    let document = read_document(&path)?;
    parse_app_config(&document)
}

/// Resolve the configured adapter, run its build, and validate its output.
pub(crate) fn build(repository: &Path) -> Result<BuildPlan> {
    let config = load_config(repository)?;
    let framework = match config.build.framework {
        Framework::Auto => detect_framework(repository)?.context(
            "could not detect Vite, Next.js, or Rsbuild; set build.framework in maypop.toml",
        )?,
        framework => framework,
    };
    let defaults = adapter(framework, repository)?;
    let command = config.build.command.or(defaults.command);
    if command.as_ref().is_some_and(Vec::is_empty) {
        bail!("build.command in maypop.toml cannot be empty");
    }
    let output = config.build.output.unwrap_or(defaults.output);
    require_safe_relative_path(&output, "build.output")?;
    let entry = config.build.entry.unwrap_or_else(|| "index.html".into());
    require_safe_relative_path(Path::new(&entry), "build.entry")?;

    if let Some(command) = &command {
        let (program, args) = command
            .split_first()
            .context("build.command in maypop.toml cannot be empty")?;
        println!("Building with {}...", command.join(" "));
        let status = Command::new(program)
            .args(args)
            .current_dir(repository)
            .status()
            .with_context(|| format!("could not run {program}"))?;
        if !status.success() {
            bail!("{} build failed with {status}", framework);
        }
    }

    let output_directory = repository.join(&output);
    if !output_directory.is_dir() {
        if framework == Framework::Next && repository.join(".next").exists() {
            bail!(
                "Next.js produced .next but no {}; set output: \"export\" in next.config and run publish again",
                output.display()
            );
        }
        bail!(
            "{} build output {} does not exist; override build.output in maypop.toml if needed",
            framework,
            output.display()
        );
    }
    let root = std::fs::canonicalize(repository)?;
    let output_directory = std::fs::canonicalize(&output_directory)?;
    if !output_directory.starts_with(&root) {
        bail!("build.output must stay inside the Git repository");
    }
    if !output_directory.join(&entry).is_file() {
        bail!(
            "{} build output does not contain {}",
            output_directory.display(),
            entry
        );
    }

    Ok(BuildPlan {
        framework: framework.to_string(),
        output_directory,
        entry,
        routing: defaults.routing,
    })
}

struct AdapterDefaults {
    command: Option<Vec<String>>,
    output: PathBuf,
    routing: Routing,
}

fn adapter(framework: Framework, repository: &Path) -> Result<AdapterDefaults> {
    let build_command = || package_manager(repository).build_command();
    Ok(match framework {
        Framework::Vite | Framework::Rsbuild => AdapterDefaults {
            command: Some(build_command()),
            output: PathBuf::from("dist"),
            routing: Routing::Spa,
        },
        Framework::Next => AdapterDefaults {
            command: Some(build_command()),
            output: PathBuf::from("out"),
            routing: Routing::StaticPages,
        },
        Framework::Static => AdapterDefaults {
            command: None,
            output: PathBuf::from("."),
            routing: Routing::Spa,
        },
        Framework::Auto => bail!("auto must be resolved before selecting an adapter"),
    })
}

fn load_config(repository: &Path) -> Result<ProjectConfig> {
    let path = repository.join(CONFIG_FILE);
    let document = read_document(&path)?;
    let build = document
        .get("build")
        .and_then(toml_edit::Item::as_table)
        .context("maypop.toml must contain a [build] table")?;
    let framework = build
        .get("framework")
        .and_then(toml_edit::Item::as_str)
        .context("maypop.toml build.framework must be a string")?;
    let command = build
        .get("command")
        .map(|item| {
            let values = item
                .as_array()
                .context("maypop.toml build.command must be an array of strings")?;
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .context("maypop.toml build.command must contain only strings")
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    let string_path = |name: &str| -> Result<Option<String>> {
        build
            .get(name)
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .with_context(|| format!("maypop.toml build.{name} must be a string"))
            })
            .transpose()
    };
    Ok(ProjectConfig {
        build: BuildConfig {
            framework: Framework::parse(framework)?,
            command,
            output: string_path("output")?.map(PathBuf::from),
            entry: string_path("entry")?,
        },
    })
}

fn read_document(path: &Path) -> Result<DocumentMut> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}; run `maypop init`", path.display()))?;
    raw.parse::<DocumentMut>()
        .with_context(|| format!("invalid {}", path.display()))
}

fn app_table(app: &AppConfig) -> Table {
    let mut table = Table::new();
    if let Some(name) = &app.name {
        table["name"] = value(name);
    }
    if let Some(description) = &app.description {
        table["description"] = value(description);
    }
    if let Some(visibility) = &app.visibility {
        table["visibility"] = value(visibility);
    }
    if let Some(link_access) = &app.link_access {
        table["link_access"] = value(link_access);
    }
    if let Some(allow_remixing) = app.allow_remixing {
        table["allow_remixing"] = value(allow_remixing);
    }
    if let Some(tags) = &app.tags {
        let mut array = Array::new();
        for tag in tags {
            array.push(tag);
        }
        table["tags"] = value(array);
    }
    if let Some(thumbnail) = &app.thumbnail {
        table["thumbnail"] = value(thumbnail.to_string_lossy().as_ref());
    }
    table
}

fn parse_app_config(document: &DocumentMut) -> Result<Option<AppConfig>> {
    let Some(app) = document.get("app") else {
        return Ok(None);
    };
    let app = app
        .as_table()
        .context("maypop.toml [app] must be a table")?;
    let string = |name: &str| -> Result<Option<String>> {
        app.get(name)
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .with_context(|| format!("maypop.toml app.{name} must be a string"))
            })
            .transpose()
    };
    let boolean = |name: &str| -> Result<Option<bool>> {
        app.get(name)
            .map(|item| {
                item.as_bool()
                    .with_context(|| format!("maypop.toml app.{name} must be a boolean"))
            })
            .transpose()
    };
    let name = string("name")?;
    if name.as_ref().is_some_and(|name| name.trim().is_empty()) {
        bail!("maypop.toml app.name cannot be empty");
    }
    let visibility = string("visibility")?;
    if visibility
        .as_deref()
        .is_some_and(|value| !matches!(value, "private" | "unlisted" | "public"))
    {
        bail!("maypop.toml app.visibility must be private, unlisted, or public");
    }
    let link_access = string("link_access")?;
    if link_access
        .as_deref()
        .is_some_and(|value| !matches!(value, "request" | "view" | "use"))
    {
        bail!("maypop.toml app.link_access must be request, view, or use");
    }
    let tags = app
        .get("tags")
        .map(|item| {
            let values = item
                .as_array()
                .context("maypop.toml app.tags must be an array of strings")?;
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .context("maypop.toml app.tags must contain only strings")
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    let thumbnail = string("thumbnail")?.map(PathBuf::from);
    if let Some(thumbnail) = &thumbnail {
        require_safe_relative_path(thumbnail, "app.thumbnail")?;
    }
    Ok(Some(AppConfig {
        name,
        description: string("description")?,
        visibility,
        link_access,
        allow_remixing: boolean("allow_remixing")?,
        tags,
        thumbnail,
    }))
}

fn detect_framework(repository: &Path) -> Result<Option<Framework>> {
    let package_path = repository.join("package.json");
    if package_path.is_file() {
        let package: Value = serde_json::from_slice(&std::fs::read(&package_path)?)
            .with_context(|| format!("invalid {}", package_path.display()))?;
        if has_dependency(&package, "next") {
            return Ok(Some(Framework::Next));
        }
        if has_dependency(&package, "@rsbuild/core") {
            return Ok(Some(Framework::Rsbuild));
        }
        if has_dependency(&package, "vite") {
            return Ok(Some(Framework::Vite));
        }
    }
    Ok(repository
        .join("index.html")
        .is_file()
        .then_some(Framework::Static))
}

fn has_dependency(package: &Value, name: &str) -> bool {
    ["dependencies", "devDependencies"]
        .into_iter()
        .filter_map(|field| package.get(field)?.as_object())
        .any(|dependencies| dependencies.contains_key(name))
}

#[derive(Clone, Copy)]
enum PackageManager {
    Pnpm,
    Npm,
    Yarn,
    Bun,
}

impl PackageManager {
    fn build_command(self) -> Vec<String> {
        match self {
            Self::Pnpm => strings(&["pnpm", "run", "build"]),
            Self::Npm => strings(&["npm", "run", "build"]),
            Self::Yarn => strings(&["yarn", "build"]),
            Self::Bun => strings(&["bun", "run", "build"]),
        }
    }
}

fn package_manager(repository: &Path) -> PackageManager {
    if let Ok(raw) = std::fs::read(repository.join("package.json")) {
        if let Ok(package) = serde_json::from_slice::<Value>(&raw) {
            if let Some(manager) = package
                .get("packageManager")
                .and_then(Value::as_str)
                .and_then(|value| value.split('@').next())
            {
                match manager {
                    "pnpm" => return PackageManager::Pnpm,
                    "yarn" => return PackageManager::Yarn,
                    "bun" => return PackageManager::Bun,
                    "npm" => return PackageManager::Npm,
                    _ => {}
                }
            }
        }
    }
    if repository.join("pnpm-lock.yaml").is_file() {
        PackageManager::Pnpm
    } else if repository.join("bun.lock").is_file() || repository.join("bun.lockb").is_file() {
        PackageManager::Bun
    } else if repository.join("yarn.lock").is_file() {
        PackageManager::Yarn
    } else {
        PackageManager::Npm
    }
}

fn require_safe_relative_path(path: &Path, field: &str) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("{field} must be a relative path");
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!("{field} must stay inside the Git repository");
    }
    Ok(())
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framework_detection_prefers_the_framework_package() {
        let package = serde_json::json!({
            "dependencies": {"next": "16"},
            "devDependencies": {"vite": "8"}
        });
        assert!(has_dependency(&package, "next"));
        assert!(has_dependency(&package, "vite"));
        assert!(!has_dependency(&package, "@rsbuild/core"));
    }

    #[test]
    fn adapters_define_framework_specific_outputs_and_routing() {
        let repository = Path::new("/tmp/project-with-no-lockfile");
        let vite = adapter(Framework::Vite, repository).unwrap();
        assert_eq!(vite.output, Path::new("dist"));
        assert_eq!(vite.routing, Routing::Spa);

        let next = adapter(Framework::Next, repository).unwrap();
        assert_eq!(next.output, Path::new("out"));
        assert_eq!(next.routing, Routing::StaticPages);
    }

    #[test]
    fn output_paths_cannot_escape_the_repository() {
        assert!(require_safe_relative_path(Path::new("dist"), "output").is_ok());
        assert!(require_safe_relative_path(Path::new("../dist"), "output").is_err());
        assert!(require_safe_relative_path(Path::new("/tmp/dist"), "output").is_err());
    }

    #[test]
    fn init_configuration_records_the_detected_adapter() {
        let directory =
            std::env::temp_dir().join(format!("maypop-project-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("package.json"),
            r#"{"devDependencies":{"@rsbuild/core":"2"}}"#,
        )
        .unwrap();

        let app = AppConfig {
            name: Some("Rsbuild example".into()),
            description: Some("An example app".into()),
            visibility: Some("private".into()),
            link_access: Some("request".into()),
            allow_remixing: Some(true),
            tags: Some(vec!["example".into()]),
            thumbnail: None,
        };
        let path = create_config(&directory, Some(&app)).unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            concat!(
                "[app]\n",
                "name = \"Rsbuild example\"\n",
                "description = \"An example app\"\n",
                "visibility = \"private\"\n",
                "link_access = \"request\"\n",
                "allow_remixing = true\n",
                "tags = [\"example\"]\n",
                "\n[build]\n",
                "framework = \"rsbuild\"\n",
            )
        );
        assert_eq!(
            load_config(&directory).unwrap().build.framework,
            Framework::Rsbuild
        );
        assert_eq!(app_config(&directory).unwrap(), Some(app));

        std::fs::remove_dir_all(directory).unwrap();
    }
}
