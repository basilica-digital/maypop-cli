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
