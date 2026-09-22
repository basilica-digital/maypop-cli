//! Authenticated media generation commands.

use crate::http as http_request;
use crate::user_commands::{client, required_token, successful_json};
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::Path;

#[derive(Deserialize)]
struct ImageResponse {
    data: Vec<ImageData>,
}

#[derive(Deserialize)]
struct ImageData {
    b64_json: Option<String>,
}

#[derive(Deserialize)]
struct AudioResponse {
    audio: String,
}

#[derive(Deserialize)]
struct VideoResponse {
    video: String,
}

/// Generate one image and write its decoded PNG bytes to disk.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn generate_image(
    api_url: &str,
    profile: Option<&str>,
    explicit_token: Option<&str>,
    prompt: &str,
    output: &Path,
    tier: &str,
    size: &str,
    force: bool,
) -> Result<()> {
    validate_prompt(prompt)?;
    prepare_output(output, &["png"], force)?;
    let http = account_client(api_url, profile, explicit_token)?;
    let response = http_request::json(
        http.post(format!("{}/ai/images/generations", trim_url(api_url))),
        &json!({
            "prompt": prompt.trim(),
            "tier": tier,
            "size": size,
            "n": 1,
            "response_format": "b64_json",
            "watermark": false,
        }),
    )?
    .send()
    .await
    .context("could not generate the image")?;
    let generated = successful_json::<ImageResponse>(response).await?;
    let encoded = generated
        .data
        .into_iter()
        .next()
        .and_then(|image| image.b64_json)
        .context("Maypop returned no image data")?;
    write_base64(output, &encoded, "image")
}

/// Generate audio and write its decoded MP3 or WAV bytes to disk.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn generate_audio(
    api_url: &str,
    profile: Option<&str>,
    explicit_token: Option<&str>,
    prompt: &str,
    output: &Path,
    requested_format: Option<&str>,
    force: bool,
) -> Result<()> {
    validate_prompt(prompt)?;
    let extension = output_extension(output)?;
    if !matches!(extension.as_str(), "mp3" | "wav") {
        bail!("audio output must end in .mp3 or .wav");
    }
    let format = requested_format.unwrap_or(&extension);
    if format != extension {
        bail!("audio format `{format}` does not match the .{extension} output path");
    }
    prepare_output(output, &["mp3", "wav"], force)?;
    let http = account_client(api_url, profile, explicit_token)?;
    let response = http_request::json(
        http.post(format!("{}/ai/audio/generations", trim_url(api_url))),
        &json!({
            "prompt": prompt.trim(),
            "audio_config": { "format": format },
        }),
    )?
    .send()
    .await
    .context("could not generate the audio")?;
    let generated = successful_json::<AudioResponse>(response).await?;
    write_base64(output, &generated.audio, "audio")
}

/// Generate a video and write its decoded MP4 bytes to disk.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn generate_video(
    api_url: &str,
    profile: Option<&str>,
    explicit_token: Option<&str>,
    prompt: &str,
    output: &Path,
    model: &str,
    duration: u32,
    resolution: &str,
    ratio: &str,
    generate_audio: bool,
    seed: Option<i32>,
    force: bool,
) -> Result<()> {
    validate_prompt(prompt)?;
    if model == "fast" && duration > 15 {
        bail!("fast video generation supports at most 15 seconds; use `--model quality`");
    }
    prepare_output(output, &["mp4"], force)?;
    let http = account_client(api_url, profile, explicit_token)?;
    let mut payload = Map::from_iter([
        ("prompt".into(), Value::String(prompt.trim().into())),
        ("model".into(), Value::String(model.into())),
        ("duration".into(), Value::from(duration)),
        ("resolution".into(), Value::String(resolution.into())),
        ("ratio".into(), Value::String(ratio.into())),
        ("generate_audio".into(), Value::Bool(generate_audio)),
    ]);
    if let Some(seed) = seed {
        payload.insert("seed".into(), Value::from(seed));
    }
    let response = http_request::json(
        http.post(format!("{}/ai/videos/generations", trim_url(api_url))),
        &Value::Object(payload),
    )?
    .send()
    .await
    .context("could not generate the video")?;
    let generated = successful_json::<VideoResponse>(response).await?;
    write_base64(output, &generated.video, "video")
}

fn account_client(
    api_url: &str,
    profile: Option<&str>,
    explicit_token: Option<&str>,
) -> Result<reqwest::Client> {
    let token = required_token(api_url, profile, explicit_token)?;
    client(Some(&token))
}

fn validate_prompt(prompt: &str) -> Result<()> {
    if prompt.trim().is_empty() {
        bail!("prompt cannot be empty");
    }
    Ok(())
}

fn prepare_output(path: &Path, extensions: &[&str], force: bool) -> Result<()> {
    let extension = output_extension(path)?;
    if !extensions.contains(&extension.as_str()) {
        bail!(
            "output must use one of these extensions: {}",
            extensions
                .iter()
                .map(|extension| format!(".{extension}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if path.exists() && !force {
        bail!(
            "{} already exists; pass `--force` to replace it",
            path.display()
        );
    }
    if path.is_dir() {
        bail!("{} is a directory", path.display());
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    Ok(())
}

fn output_extension(path: &Path) -> Result<String> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .with_context(|| format!("{} has no valid file extension", path.display()))
}

fn write_base64(path: &Path, encoded: &str, kind: &str) -> Result<()> {
    let bytes = STANDARD
        .decode(encoded)
        .with_context(|| format!("Maypop returned invalid base64 {kind} data"))?;
    fs::write(path, &bytes).with_context(|| format!("could not write {}", path.display()))?;
    println!("Wrote {} bytes to {}", bytes.len(), path.display());
    Ok(())
}

fn trim_url(url: &str) -> &str {
    url.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_validation_refuses_overwrites_and_wrong_extensions() {
        let directory = std::env::temp_dir().join(format!("maypop-ai-{}", uuid::Uuid::new_v4()));
        let existing = directory.join("image.png");
        fs::create_dir_all(&directory).unwrap();
        fs::write(&existing, b"old").unwrap();

        assert!(prepare_output(&existing, &["png"], false)
            .unwrap_err()
            .to_string()
            .contains("--force"));
        assert!(prepare_output(&existing, &["png"], true).is_ok());
        assert!(prepare_output(&directory.join("image.jpg"), &["png"], false).is_err());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn base64_writer_decodes_binary_media() {
        let directory = std::env::temp_dir().join(format!("maypop-ai-{}", uuid::Uuid::new_v4()));
        let output = directory.join("sound.mp3");
        prepare_output(&output, &["mp3"], false).unwrap();

        write_base64(&output, "AAECAw==", "audio").unwrap();
        assert_eq!(fs::read(&output).unwrap(), [0, 1, 2, 3]);

        fs::remove_dir_all(directory).unwrap();
    }
}
