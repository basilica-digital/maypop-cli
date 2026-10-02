//! Browser approval flow used by `maypop auth`.

use crate::credentials::{CredentialUser, Credentials};
use crate::http;
use crate::user_commands::successful_json;
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartRequest<'a> {
    device_name: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenRequest<'a> {
    device_code: &'a str,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum TokenResponse {
    Pending {
        interval: u64,
    },
    Expired,
    Complete {
        token: String,
        #[serde(rename = "expiresAt")]
        expires_at: String,
        user: CredentialUser,
    },
}

/// Authenticate in the browser and return a credential ready for persistence.
pub(crate) async fn authenticate(
    client: &Client,
    api_url: &str,
    device_name: &str,
    no_browser: bool,
) -> Result<Credentials> {
    let api_url = api_url.trim_end_matches('/');
    let response = http::json(
        client.post(format!("{api_url}/cli/auth/start")),
        &StartRequest { device_name },
    )?
    .send()
    .await
    .context("could not start Maypop authentication")?;
    let start = successful_json::<StartResponse>(response).await?;

    println!("Authorize this device in Maypop:");
    println!("  {}", start.verification_uri);
    println!("  Code: {}", start.user_code);
    if !no_browser {
        match open_browser(&start.verification_uri_complete) {
            Ok(()) => println!("\nOpened your browser. Waiting for approval…"),
            Err(error) => eprintln!("\nCould not open a browser ({error}). Open the URL above."),
        }
    } else {
        println!("\nWaiting for approval…");
    }

    let deadline = Instant::now() + Duration::from_secs(start.expires_in);
    let mut interval = start.interval.max(1);
    loop {
        if Instant::now() >= deadline {
            bail!("authentication expired; run `maypop auth` again");
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let response = http::json(
            client
                .post(format!("{api_url}/cli/auth/token"))
                .timeout(Duration::from_secs(30)),
            &TokenRequest {
                device_code: &start.device_code,
            },
        )?
        .send()
        .await
        .context("lost connection while waiting for Maypop approval")?;
        match successful_json::<TokenResponse>(response).await? {
            TokenResponse::Pending {
                interval: server_interval,
            } => interval = server_interval.max(1),
            TokenResponse::Expired => {
                bail!("authentication expired; run `maypop auth` again")
            }
            TokenResponse::Complete {
                token,
                expires_at,
                user,
            } => {
                return Ok(Credentials {
                    api_url: api_url.into(),
                    token,
                    expires_at,
                    user,
                });
            }
        }
    }
}

pub(crate) fn default_device_name() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok();
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok();
    match (user, host) {
        (Some(user), Some(host)) => format!("{user}@{host}"),
        (_, Some(host)) => host,
        _ => "Maypop CLI".into(),
    }
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");

    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("browser launcher failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_name_has_a_stable_fallback() {
        assert!(!default_device_name().trim().is_empty());
    }

    #[test]
    fn complete_response_deserializes_the_wire_format() {
        let response: TokenResponse = serde_json::from_value(serde_json::json!({
            "status": "complete",
            "token": "mpat_secret",
            "expiresAt": "2027-01-01T00:00:00Z",
            "user": {
                "id": "00000000-0000-0000-0000-000000000001",
                "username": "gustavo",
                "name": "Gustavo",
                "email": "gustavo@example.com",
                "gitUserId": "user_gustavo",
                "gitServerUrl": "https://api.app.maypop.ai/git"
            }
        }))
        .unwrap();
        assert!(matches!(response, TokenResponse::Complete { .. }));
    }

    /// Answer each request on `listener` with the next canned status and body.
    fn serve(listener: std::net::TcpListener, replies: Vec<(&'static str, &'static str)>) {
        use std::io::{BufRead, BufReader, Read, Write};
        std::thread::spawn(move || {
            for (status, body) in replies {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                reader.read_exact(&mut vec![0; length]).unwrap();
                write!(
                    reader.get_mut(),
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
    }

    #[tokio::test]
    async fn a_plan_refusal_stops_the_poll_with_a_billing_link() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let api_url = format!("http://{}", listener.local_addr().unwrap());
        serve(
            listener,
            vec![
                (
                    "200 OK",
                    r#"{"deviceCode":"mda1.x.y","userCode":"ZELL-JC3J","verificationUri":"https://app.maypop.ai/cli/auth","verificationUriComplete":"https://app.maypop.ai/cli/auth?code=ZELLJC3J","expiresIn":600,"interval":1}"#,
                ),
                (
                    "402 Payment Required",
                    r#"{"code":"plan_required","error":"The Maypop CLI is included with Pro.","billingUrl":"https://app.maypop.ai/user/user_1?tab=billing"}"#,
                ),
            ],
        );

        let Err(error) = authenticate(&Client::new(), &api_url, "test", true).await else {
            panic!("a plan refusal ends authentication");
        };
        assert_eq!(
            error.to_string(),
            "The Maypop CLI needs a Pro plan. Upgrade at https://app.maypop.ai/user/user_1?tab=billing"
        );
    }
}
