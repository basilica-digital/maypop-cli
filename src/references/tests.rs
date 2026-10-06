use super::*;

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn importing_a_reference_preserves_a_clean_worktree_and_an_existing_index() {
    let root = std::env::temp_dir().join(format!("maypop-reference-git-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.name", "Reference Test"]);
    git(
        &root,
        &["config", "user.email", "reference@example.invalid"],
    );
    std::fs::write(root.join("index.html"), "initial").unwrap();
    git(&root, &["add", "index.html"]);
    git(&root, &["commit", "-m", "initial"]);
    let before = git(&root, &["rev-parse", "HEAD"]);
    let id = Uuid::new_v4();
    let imported = import_snapshot(&root, id, snapshot(id, &[("src.ts", b"reference")])).unwrap();
    assert_eq!(
        git(&root, &["status", "--porcelain", "--untracked-files=all"]),
        ""
    );
    git(&root, &["check-ignore", imported.to_str().unwrap()]);
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), before);
    assert!(!root.join(".maypop/.gitignore").exists());

    std::fs::write(root.join("index.html"), "user's staged changes").unwrap();
    git(&root, &["add", "index.html"]);
    let index = git(&root, &["diff", "--cached"]);
    std::fs::write(root.join("index.html"), "user's unstaged changes").unwrap();
    let changes = git(&root, &["diff"]);
    std::fs::write(root.join(".maypop/.gitignore"), "custom-pattern\n").unwrap();
    import_snapshot(&root, id, snapshot(id, &[("src.ts", b"second reference")])).unwrap();
    assert_eq!(git(&root, &["diff", "--cached"]), index);
    assert_eq!(git(&root, &["diff"]), changes);
    assert_eq!(
        std::fs::read_to_string(root.join(".maypop/.gitignore")).unwrap(),
        "custom-pattern\n"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn references_use_the_linked_worktree_exclude_and_escape_nested_app_paths() {
    let root = std::env::temp_dir().join(format!("maypop-reference-worktree-{}", Uuid::new_v4()));
    let main = root.join("main");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-b", "main"]);
    git(&main, &["config", "user.name", "Reference Test"]);
    git(
        &main,
        &["config", "user.email", "reference@example.invalid"],
    );
    std::fs::write(main.join("index.html"), "initial").unwrap();
    git(&main, &["add", "index.html"]);
    git(&main, &["commit", "-m", "initial"]);
    let checkout = root.join("checkout");
    git(
        &main,
        &["worktree", "add", "--detach", checkout.to_str().unwrap()],
    );
    let id = Uuid::new_v4();
    import_snapshot(&checkout, id, snapshot(id, &[("src.ts", b"reference")])).unwrap();
    assert_eq!(git(&checkout, &["status", "--porcelain"]), "");
    let app = checkout.join("apps/[demo]");
    std::fs::create_dir_all(&app).unwrap();
    let path = import_snapshot(&app, id, snapshot(id, &[("src.ts", b"reference")])).unwrap();
    git(&app, &["check-ignore", path.to_str().unwrap()]);
    std::fs::write(app.join(".maypop/kv-policy.json"), "{}").unwrap();
    std::fs::create_dir(app.join(".maypop/publish")).unwrap();
    std::fs::write(app.join(".maypop/publish/index.html"), "published app").unwrap();
    let visible = git(
        &checkout,
        &["status", "--porcelain", "--untracked-files=all"],
    );
    assert!(visible.contains("kv-policy.json"));
    assert!(visible.contains("publish/index.html"));
    assert!(!visible.contains("references"));
    std::fs::remove_dir_all(root).unwrap();
}

fn snapshot(app_id: Uuid, files: &[(&str, &[u8])]) -> Snapshot {
    Snapshot {
        app_id,
        name: "Reference".into(),
        revision: 2,
        source_commit_sha: Some("a".repeat(40)),
        files: files
            .iter()
            .map(|(path, bytes)| ReferenceFile {
                path: path.to_string(),
                content: base64::engine::general_purpose::STANDARD.encode(bytes),
            })
            .collect(),
    }
}

#[test]
fn source_import_rejects_escaping_private_and_conflicting_paths() {
    let id = Uuid::new_v4();
    for path in [
        "../secret",
        "/tmp/secret",
        "a\\b",
        ".chat/session.json",
        ".maypop/local/key",
        ".env.production",
    ] {
        assert!(
            decoded_files(&snapshot(id, &[(path, b"x")])).is_err(),
            "{path}"
        );
    }
    assert!(decoded_files(&snapshot(id, &[("src", b"x"), ("src/App.tsx", b"y")])).is_err());
    assert!(decoded_files(&snapshot(id, &[("src.ts", b"x"), ("src.ts", b"y")])).is_err());
}

#[test]
fn legacy_import_reads_editable_embedded_source_and_rejects_unsafe_paths() {
    let id = Uuid::new_v4();
    let html = br#"<script type="application/json" id="__studio-workspace-files__">{"index.html":"original","src/App.tsx":"component","logo.png":"data:image/png;base64,AP8="}</script>"#;
    let mut source = snapshot(id, &[("index.html", html)]);
    source.source_commit_sha = None;
    let files = decoded_files(&source).unwrap();
    assert_eq!(files["src/App.tsx"], b"component");
    assert_eq!(files["index.html"], b"original");
    assert_eq!(files["logo.png"], [0, 255]);
    source.files[0].content = base64::engine::general_purpose::STANDARD.encode(
        br#"<script id="__studio-workspace-files__">{"index.html":"ok","../escape":"bad"}</script>"#);
    assert!(decoded_files(&source).is_err());
}

#[test]
fn importing_binary_source_keeps_each_snapshot_separate_and_git_ignored() {
    let root = std::env::temp_dir().join(format!("maypop-reference-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let id = Uuid::new_v4();
    let first = import_snapshot(&root, id, snapshot(id, &[("image.png", &[0, 255])])).unwrap();
    let second = import_snapshot(&root, id, snapshot(id, &[("src.ts", b"changed")])).unwrap();
    assert_ne!(first, second);
    assert_eq!(
        std::fs::read(root.join(first).join("image.png")).unwrap(),
        [0, 255]
    );
    assert!(std::fs::read_to_string(root.join(".maypop/.gitignore"))
        .unwrap()
        .contains("/local/"));
    assert!(import_snapshot(&root, Uuid::new_v4(), snapshot(id, &[("x", b"x")])).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn importing_never_follows_a_link_out_of_the_app() {
    let root = std::env::temp_dir().join(format!("maypop-reference-{}", Uuid::new_v4()));
    std::fs::create_dir_all(root.join("outside")).unwrap();
    std::fs::create_dir_all(root.join("app/.maypop")).unwrap();
    std::os::unix::fs::symlink(root.join("outside"), root.join("app/.maypop/local")).unwrap();
    let id = Uuid::new_v4();
    assert!(import_snapshot(&root.join("app"), id, snapshot(id, &[("src.ts", b"x")])).is_err());
    assert_eq!(std::fs::read_dir(root.join("outside")).unwrap().count(), 0);
    std::fs::remove_dir_all(root).unwrap();
}
