//! Local persistence for named Maypop CLI profiles.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) const DEFAULT_API_URL: &str = "https://api.app.maypop.ai";
const FALLBACK_PROFILE: &str = "default";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Credentials {
    pub api_url: String,
    pub token: String,
    pub expires_at: String,
    pub user: CredentialUser,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CredentialUser {
    pub id: String,
    pub username: String,
    pub name: Option<String>,
    pub email: String,
    #[serde(default)]
    pub git_user_id: String,
    #[serde(default)]
    pub git_server_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CredentialStore {
    version: u8,
    default_profile: String,
    profiles: BTreeMap<String, Credentials>,
}

#[derive(Debug)]
pub(crate) struct SavedProfile {
    pub name: String,
    pub credentials: Credentials,
    pub is_default: bool,
}

/// Return the credential path, honoring an explicit test/automation override.
pub(crate) fn path() -> Result<PathBuf> {
    if let Some(directory) = std::env::var_os("MAYPOP_CONFIG_DIR") {
        return Ok(PathBuf::from(directory).join("profiles.json"));
    }
    if let Some(directory) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(directory)
            .join("maypop")
            .join("profiles.json"));
    }
    if cfg!(windows) {
        if let Some(directory) = std::env::var_os("APPDATA") {
            return Ok(PathBuf::from(directory)
                .join("maypop")
                .join("profiles.json"));
        }
    }
    let Some(home) = std::env::var_os("HOME") else {
        bail!("cannot find a config directory; set MAYPOP_CONFIG_DIR");
    };
    Ok(PathBuf::from(home)
        .join(".config")
        .join("maypop")
        .join("profiles.json"))
}

/// Resolve the selected profile name, falling back to the configured default.
pub(crate) fn selected_profile(explicit: Option<&str>) -> Result<String> {
    if let Some(name) = explicit {
        validate_profile_name(name)?;
        return Ok(name.to_string());
    }
    Ok(load_store(&path()?)?
        .map(|store| store.default_profile)
        .unwrap_or_else(|| FALLBACK_PROFILE.into()))
}

/// Resolve an API URL from an override or a saved profile.
pub(crate) fn api_url_for(profile: &str, explicit_url: Option<&str>) -> Result<String> {
    if let Some(url) = explicit_url {
        return Ok(normalize_url(url).to_string());
    }
    if let Some(credentials) = profile_credentials(profile)? {
        return Ok(normalize_url(&credentials.api_url).to_string());
    }
    if profile == FALLBACK_PROFILE {
        return Ok(DEFAULT_API_URL.into());
    }
    bail!(
        "profile `{profile}` does not exist; authenticate it with `maypop --profile {profile} --url <url> auth`"
    )
}

/// Read the saved token for an API origin, preferring an explicitly named profile.
pub(crate) fn token_for(api_url: &str, profile: Option<&str>) -> Result<Option<String>> {
    Ok(load_for(api_url, profile)?.map(|credentials| credentials.token))
}

/// Read credentials for an API origin, preferring an explicitly named profile.
pub(crate) fn load_for(api_url: &str, profile: Option<&str>) -> Result<Option<Credentials>> {
    let Some(store) = load_store(&path()?)? else {
        return Ok(None);
    };
    select_credentials(&store, api_url, profile)
}

fn select_credentials(
    store: &CredentialStore,
    api_url: &str,
    profile: Option<&str>,
) -> Result<Option<Credentials>> {
    if let Some(name) = profile {
        validate_profile_name(name)?;
        let Some(credentials) = store.profiles.get(name).cloned() else {
            return Ok(None);
        };
        if normalize_url(&credentials.api_url) != normalize_url(api_url) {
            bail!(
                "profile `{name}` uses {}, but this app uses {}",
                normalize_url(&credentials.api_url),
                normalize_url(api_url)
            );
        }
        return Ok(Some(credentials));
    }
    if let Some(credentials) = store.profiles.get(&store.default_profile) {
        if normalize_url(&credentials.api_url) == normalize_url(api_url) {
            return Ok(Some(credentials.clone()));
        }
    }
    Ok(store
        .profiles
        .values()
        .find(|credentials| normalize_url(&credentials.api_url) == normalize_url(api_url))
        .cloned())
}

/// Read every saved credential for an API origin.
pub(crate) fn all_for(api_url: &str) -> Result<Vec<Credentials>> {
    Ok(load_store(&path()?)?
        .into_iter()
        .flat_map(|store| store.profiles.into_values())
        .filter(|credentials| normalize_url(&credentials.api_url) == normalize_url(api_url))
        .collect())
}

/// Persist a login under a profile, retaining every other saved profile.
pub(crate) fn save(profile: &str, credentials: Credentials) -> Result<PathBuf> {
    validate_profile_name(profile)?;
    let path = path()?;
    save_profile_at(&path, profile, credentials)?;
    Ok(path)
}

/// List saved profiles in name order and identify the configured default.
pub(crate) fn profiles() -> Result<Vec<SavedProfile>> {
    let Some(store) = load_store(&path()?)? else {
        return Ok(Vec::new());
    };
    let default_profile = store.default_profile;
    Ok(store
        .profiles
        .into_iter()
        .map(|(name, credentials)| SavedProfile {
            is_default: name == default_profile,
            name,
            credentials,
        })
        .collect())
}

/// Make an existing profile the default for commands without `--profile`.
pub(crate) fn set_default(profile: &str) -> Result<PathBuf> {
    validate_profile_name(profile)?;
    let path = path()?;
    set_default_at(&path, profile)?;
    Ok(path)
}

fn profile_credentials(profile: &str) -> Result<Option<Credentials>> {
    validate_profile_name(profile)?;
    Ok(load_store(&path()?)?.and_then(|store| store.profiles.get(profile).cloned()))
}

fn validate_profile_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("profile names may contain only letters, numbers, `-`, and `_`");
    }
    Ok(())
}

fn load_store(path: &Path) -> Result<Option<CredentialStore>> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read profiles from {}", path.display()));
        }
    };
    let store = serde_json::from_str::<CredentialStore>(&raw)
        .with_context(|| format!("invalid profiles file at {}", path.display()))?;
    if store.version != 1 {
        bail!(
            "profiles file at {} has unsupported version {}",
            path.display(),
            store.version
        );
    }
    if !store.profiles.contains_key(&store.default_profile) {
        bail!(
            "profiles file at {} selects missing default profile `{}`",
            path.display(),
            store.default_profile
        );
    }
    Ok(Some(store))
}

fn save_profile_at(path: &Path, profile: &str, credentials: Credentials) -> Result<()> {
    let mut store = load_store(path)?.unwrap_or_else(|| CredentialStore {
        version: 1,
        default_profile: profile.into(),
        profiles: BTreeMap::new(),
    });
    store.version = 1;
    store.profiles.insert(profile.into(), credentials);
    save_at(path, &store)
}

fn set_default_at(path: &Path, profile: &str) -> Result<()> {
    let mut store = load_store(path)?.context("no Maypop profiles are configured")?;
    if !store.profiles.contains_key(profile) {
        bail!("profile `{profile}` does not exist; run `maypop profile list`");
    }
    store.default_profile = profile.into();
    save_at(path, &store)
}

fn save_at(path: &Path, store: &CredentialStore) -> Result<()> {
    let directory = path
        .parent()
        .context("profiles path has no parent directory")?;
    fs::create_dir_all(directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;

    let temporary = path.with_extension("json.tmp");
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("failed to create {}", temporary.display()))?;
    serde_json::to_writer_pretty(&mut file, store)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn normalize_url(url: &str) -> &str {
    url.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_credentials(api_url: &str, username: &str) -> Credentials {
        Credentials {
            api_url: api_url.into(),
            token: format!("mpat_{username}"),
            expires_at: "2027-01-01T00:00:00Z".into(),
            user: CredentialUser {
                id: "00000000-0000-0000-0000-000000000001".into(),
                username: username.into(),
                name: Some("Gustavo".into()),
                email: "gustavo@example.com".into(),
                git_user_id: "user_gustavo".into(),
                git_server_url: format!("{api_url}/git"),
            },
        }
    }

    fn temporary_path() -> (PathBuf, PathBuf) {
        let directory = std::env::temp_dir().join(format!("maypop-cli-{}", uuid::Uuid::new_v4()));
        let path = directory.join("profiles.json");
        (directory, path)
    }

    #[test]
    fn profiles_roundtrip_and_the_default_can_change() {
        let (directory, path) = temporary_path();
        save_profile_at(
            &path,
            "local",
            example_credentials("http://localhost:3000", "local-user"),
        )
        .unwrap();
        save_profile_at(
            &path,
            "prod",
            example_credentials(DEFAULT_API_URL, "prod-user"),
        )
        .unwrap();
        set_default_at(&path, "prod").unwrap();

        let store = load_store(&path).unwrap().unwrap();
        assert_eq!(store.default_profile, "prod");
        assert_eq!(store.profiles.len(), 2);
        assert_eq!(store.profiles["local"].token, "mpat_local-user");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn profile_names_are_shell_and_config_safe() {
        for valid in ["local", "dev-2", "prod_eu"] {
            assert!(validate_profile_name(valid).is_ok());
        }
        for invalid in ["", "two words", "../prod", "dev.example"] {
            assert!(validate_profile_name(invalid).is_err());
        }
    }

    #[test]
    fn credential_selection_prefers_the_default_then_an_explicit_profile() {
        let store = CredentialStore {
            version: 1,
            default_profile: "prod".into(),
            profiles: BTreeMap::from([
                (
                    "dev".into(),
                    example_credentials("https://api.dev.maypop.ai", "dev-user"),
                ),
                (
                    "prod".into(),
                    example_credentials(DEFAULT_API_URL, "prod-user"),
                ),
            ]),
        };

        let production = select_credentials(&store, DEFAULT_API_URL, None)
            .unwrap()
            .unwrap();
        assert_eq!(production.user.username, "prod-user");
        let development = select_credentials(&store, "https://api.dev.maypop.ai/", Some("dev"))
            .unwrap()
            .unwrap();
        assert_eq!(development.user.username, "dev-user");
        assert!(select_credentials(&store, DEFAULT_API_URL, Some("dev")).is_err());
    }
}
