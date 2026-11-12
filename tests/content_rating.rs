//! CLI content rating requests and effective-rating output.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

#[test]
fn apply_sends_the_declaration_and_shows_the_rating_in_force() {
    let directory = std::env::temp_dir().join(format!("maypop-rating-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let git = |args: &[&str]| {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&directory)
            .status()
            .unwrap()
            .success());
    };
    git(&["init", "-q"]);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    git(&["config", "maypop.app-id", "app-id"]);
    git(&[
        "config",
        "maypop.api-url",
        &format!("http://{}", listener.local_addr().unwrap()),
    ]);
    std::fs::write(
        directory.join("maypop.toml"),
        "[app]\ncontent_rating = \"general\"\n",
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let n = stream.read(&mut buffer).unwrap();
            bytes.extend_from_slice(&buffer[..n]);
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
            assert_ne!(n, 0);
        }
        let request = String::from_utf8(bytes).unwrap();
        let body = r#"{"id":"app-id","name":"Example","description":null,"visibility":"private","linkAccess":"request","allowRemixing":true,"contentRating":"mature","tags":[],"thumbnailCid":null,"latestVersion":0,"stableVersion":0}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        request
    });
    let output = Command::new(env!("CARGO_BIN_EXE_maypop"))
        .args(["app", "apply"])
        .current_dir(&directory)
        .env("MAYPOP_TOKEN", "test-token")
        .output()
        .unwrap();
    let request = server.join().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(request.starts_with("PUT /cli/apps/app-id "));
    let body = request.split_once("\r\n\r\n").unwrap().1;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).unwrap(),
        serde_json::json!({"contentRating":"general"})
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Content rating: mature"));
}

fn response_info(rating: Option<&str>) -> String {
    let mut app = serde_json::json!({"id":"app-id","name":"Example","description":null,"visibility":"private","linkAccess":"request","allowRemixing":true,"tags":[],"thumbnailCid":null,"latestVersion":0,"stableVersion":0});
    if let Some(rating) = rating {
        app["contentRating"] = rating.into();
    }
    app.to_string()
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let n = stream.read(&mut buffer).unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8(bytes).unwrap()
}

fn respond(stream: &mut std::net::TcpStream, body: &str) {
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
}

#[test]
fn legacy_server_rejects_apply_with_a_rating_but_info_remains_readable() {
    for (command, config, rating, success) in [
        (
            vec!["app", "apply"],
            "[app]\ncontent_rating = \"mature\"\n",
            None,
            false,
        ),
        (
            vec!["app", "apply"],
            "[app]\ncontent_rating = \"mature\"\n",
            Some("unknown"),
            false,
        ),
        (
            vec!["app", "apply"],
            "[app]\nname = \"Example\"\n",
            None,
            true,
        ),
        (vec!["info"], "[app]\n", None, true),
    ] {
        let directory =
            std::env::temp_dir().join(format!("maypop-legacy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&directory)
                .status()
                .unwrap()
                .success())
        };
        git(&["init", "-q"]);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        git(&["config", "maypop.app-id", "app-id"]);
        git(&[
            "config",
            "maypop.api-url",
            &format!("http://{}", listener.local_addr().unwrap()),
        ]);
        std::fs::write(directory.join("maypop.toml"), config).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            respond(&mut stream, &response_info(rating));
            request
        });
        let output = Command::new(env!("CARGO_BIN_EXE_maypop"))
            .args(command)
            .current_dir(&directory)
            .env("MAYPOP_TOKEN", "test-token")
            .output()
            .unwrap();
        let request = server.join().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if !success {
            assert!(request.contains("\"contentRating\":\"mature\""));
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("did not acknowledge content rating support"));
        }
    }
}

#[test]
#[cfg(unix)]
fn publish_sends_rating_and_stops_when_the_server_does_not_acknowledge_it() {
    use std::os::unix::fs::PermissionsExt;
    for (declaration, expected, effective) in [
        (Some("mature"), "mature", None),
        (None, "general", None),
        (Some("mature"), "mature", Some("general")),
    ] {
        let directory =
            std::env::temp_dir().join(format!("maypop-publish-rating-{}", uuid::Uuid::new_v4()));
        let repository = directory.join("repo");
        let bare = directory.join("remote.git");
        std::fs::create_dir_all(&repository).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&repository)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        assert!(Command::new("git")
            .args(["init", "--bare", "-q"])
            .arg(&bare)
            .status()
            .unwrap()
            .success());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let remote = format!("{api}/git/apps/app-id");
        git(&["config", "maypop.app-id", "app-id"]);
        git(&["config", "maypop.api-url", &api]);
        git(&["remote", "add", "maypop", &remote]);
        let app = declaration
            .map(|r| format!("[app]\ncontent_rating = \"{r}\"\n"))
            .unwrap_or_default();
        std::fs::write(
            repository.join("maypop.toml"),
            format!("{app}[build]\nframework = \"static\"\n"),
        )
        .unwrap();
        std::fs::write(repository.join("index.html"), "test").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "fixture"]);
        let sha = git(&["rev-parse", "HEAD"]);
        let expected_sha = sha.clone();
        let bin = directory.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let git_wrapper = bin.join("git");
        std::fs::write(&git_wrapper, "#!/bin/sh\nif [ \"$3\" = push ]; then exec \"$TEST_REAL_GIT\" -C \"$2\" push \"$TEST_BARE\" \"$5\"; fi\nexec \"$TEST_REAL_GIT\" \"$@\"\n").unwrap();
        std::fs::set_permissions(&git_wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let real_git = Command::new("which").arg("git").output().unwrap();
        let real_git = String::from_utf8(real_git.stdout)
            .unwrap()
            .trim()
            .to_string();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            listener.set_nonblocking(true).unwrap();
            for index in 0..if effective.is_some() { 5 } else { 4 } {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && std::time::Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                        Err(_) => return requests,
                    }
                };
                let request = read_request(&mut stream);
                let body = match index {
                    0 => serde_json::json!({"id":"user-id","username":"test","email":"test@example.com","gitUserId":"user-id","gitServerUrl":format!("{api}/git")}).to_string(),
                    1 => r#"{"bundleId":"bundle","files":[]}"#.to_string(),
                    2 => String::new(),
                    3 => response_info(effective),
                    4 => serde_json::json!({"appId":"app-id","n":1,"sourceCommitSha":expected_sha}).to_string(),
                    _ => unreachable!(),
                };
                respond(&mut stream, &body);
                requests.push(request);
            }
            requests
        });
        let output = Command::new(env!("CARGO_BIN_EXE_maypop"))
            .arg("publish")
            .current_dir(&repository)
            .env("MAYPOP_TOKEN", "test-token")
            .env("TEST_REAL_GIT", real_git)
            .env("TEST_BARE", &bare)
            .env(
                "PATH",
                std::env::join_paths(
                    std::iter::once(bin)
                        .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
                )
                .unwrap(),
            )
            .output()
            .unwrap();
        let requests = server.join().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(
            output.status.success(),
            effective.is_some(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            requests.len() >= 4,
            "requests: {}, stderr: {}",
            requests.len(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(requests[3].starts_with("PUT /cli/apps/app-id "));
        let payload = requests[3].split_once("\r\n\r\n").unwrap().1;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(payload).unwrap(),
            serde_json::json!({"contentRating":expected})
        );
        if effective.is_none() {
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("did not acknowledge content rating support"));
            assert_eq!(requests.len(), 4);
        } else {
            assert!(requests[4].starts_with("POST /cli/apps/app-id/publish "));
        }
    }
}
