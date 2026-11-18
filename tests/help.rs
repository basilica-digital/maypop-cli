//! Help output for secret-carrying environment variables.

use std::process::Command;

// Regression: `--help` printed `[env: MAYPOP_TOKEN=<value>]`, leaking the token
// into anything help output was pasted into.
#[test]
fn help_names_maypop_token_without_printing_its_value() {
    let secret = "help-output-secret-token";
    for args in [&["--help"][..], &["auth", "--help"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_maypop"))
            .args(args)
            .env("MAYPOP_TOKEN", secret)
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("MAYPOP_TOKEN"), "{stdout}");
        assert!(!stdout.contains(secret), "{stdout}");
    }
}
