//! Git credential helper origin checks, run through the built binary.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const API: &str = "https://api.example.test";
const APP_PATH: &str = "git/apps/11111111-1111-4111-8111-111111111111";

fn temp_dir(prefix: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

/// Run `maypop git-credential get` for one request and return its stdout.
fn credential_get(
    config: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    host: &str,
    path: &str,
) -> String {
    let cwd = temp_dir("maypop-credential-cwd");
    let mut command = Command::new(env!("CARGO_BIN_EXE_maypop"));
    command
        .args(args)
        .args(["git-credential", "get"])
        .current_dir(&cwd)
        .env("MAYPOP_CONFIG_DIR", config)
        .env_remove("MAYPOP_URL")
        .env_remove("MAYPOP_TOKEN")
        .env_remove("MAYPOP_PROFILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    write!(
        child.stdin.take().unwrap(),
        "protocol=https\nhost={host}\npath={path}\n\n"
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    std::fs::remove_dir_all(cwd).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

// The helper answers only for the origin of the API it is configured for
// (scheme, host and port): an environment token goes to MAYPOP_URL's origin
// and a saved login to its own, never to whatever host Git asks about.
#[test]
fn helper_returns_credentials_only_for_the_configured_api_origin() {
    let config = temp_dir("maypop-credential-config");
    let pair = [("MAYPOP_URL", API), ("MAYPOP_TOKEN", "env-token")];

    let configured = credential_get(&config, &[], &pair, "api.example.test", APP_PATH);
    assert!(configured.contains("password=env-token\n"), "{configured}");
    for host in ["untrusted.example", "api.example.test:8443"] {
        let foreign = credential_get(&config, &[], &pair, host, APP_PATH);
        assert!(!foreign.contains("password="), "{host}: {foreign}");
    }

    let profiles = format!(
        r#"{{"version":1,"defaultProfile":"default","profiles":{{"default":{{"apiUrl":"{API}","token":"saved-token","expiresAt":"2099-01-01T00:00:00Z","user":{{"id":"id","username":"user","name":null,"email":"user@example.com","gitUserId":"user_1","gitServerUrl":"{API}/git"}}}}}}}}"#
    );
    let file = config.join("profiles.json");
    std::fs::write(&file, profiles).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let saved_path = "git/user_1/game";
    let saved = credential_get(&config, &[], &[], "api.example.test", saved_path);
    assert!(saved.contains("password=saved-token\n"), "{saved}");
    let foreign = credential_get(&config, &[], &[], "untrusted.example", saved_path);
    assert!(!foreign.contains("password="), "{foreign}");
    let flagged = credential_get(
        &config,
        &["--url", API],
        &[],
        "untrusted.example",
        saved_path,
    );
    assert!(!flagged.contains("password="), "{flagged}");

    std::fs::remove_dir_all(config).unwrap();
}
