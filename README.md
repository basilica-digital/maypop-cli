# Maypop CLI

Rust command-line client for building and publishing Maypop apps with Git.

## Install

Each `v*` GitHub release contains a prebuilt `maypop` executable and SHA-256
checksum for:

- Linux x86-64.
- macOS Apple Silicon and Intel.
- Windows x86-64.

Download the archive for your platform from the
[latest GitHub release](https://github.com/basilica-digital/maypop-cli/releases/latest),
verify the adjacent `.sha256` file, extract it, and place `maypop` (or
`maypop.exe`) somewhere on `PATH`.

To install directly from the source repository instead:

```sh
cargo install --locked --git https://github.com/basilica-digital/maypop-cli
```

Confirm the installed version:

```sh
maypop --version
```

## Quick start

```sh
maypop auth
mkdir my-app && cd my-app
maypop init
# add files, then use Git normally
git add . && git commit -m "Initial app"
maypop publish
```

Authentication opens the matching Maypop web app, where Clerk handles account
sign-in and the user approves the terminal explicitly. The CLI polls with a
one-time device secret, receives a 90-day Maypop access token, and saves it in
an owner-only profiles file. Active CLI credentials appear in account settings,
where the user can revoke any device immediately.

The default binary has nine product commands:

- `maypop auth` signs in through the browser.
- `maypop init` creates an unpublished app, initializes the current directory
  as a Git repository when needed, adds the Maypop Git remote, and installs a
  repository-local credential helper. It reuses initial app metadata from an
  existing `maypop.toml`, or creates one with the detected framework adapter.
- `maypop publish` requires a clean worktree, builds the app, pushes the current
  Git `HEAD`, uploads the adapter's static output directly to object storage,
  and promotes that artifact as the app's next immutable version.
- `maypop info` shows the app connected to the current repository without
  changing it.
- `maypop app apply` explicitly applies the fields in `[app]`, uploading a
  configured thumbnail first when necessary.
- `maypop status` reports backend health and the authenticated username/email.
- `maypop ai` generates image, audio, and video files with the authenticated
  account.
- `maypop mcp` connects MCP servers to your account and controls which ones the
  current app can use.
- `maypop profile` lists saved profiles and selects the default.

The credential helper reads the matching profile and only answers for that
account's path on the configured Maypop Git host. The token is not embedded in
the remote URL or `.git/config`. Use `maypop publish` for release pushes; it
applies the Git pack compatibility settings currently required by the Maypop
Git server.

## Build adapters

The generated configuration records app metadata and selects a framework
adapter:

```toml
[app]
name = "My app"
description = "What this app does"
visibility = "private"
link_access = "request"
allow_remixing = true
tags = []
# thumbnail = "assets/thumbnail.png"

[build]
framework = "vite"
```

Edit `[app]`, then run `maypop app apply` to apply it. Omitted fields are
left unchanged on Maypop. Supported reach values are `private`, `unlisted`, or
`public` for `visibility`, and `request`, `view`, or `use` for `link_access`.
The optional thumbnail must be an image inside the repository. Publishing does
not update app metadata implicitly.

When this file exists before `maypop init`, its name, description, and
visibility seed the new remote app. Explicit `--name`, `--description`, and
`--visibility` arguments override the corresponding configured values. Without
either source, the name defaults to the directory and visibility to `private`.

Vite and Rsbuild run the package manager's `build` script and upload `dist/`
with SPA document fallback. Next.js runs the same script and uploads its `out/`
static export with route-file resolution. Next projects must set
`output: "export"` in `next.config`. A project can override any adapter default:

```toml
[build]
framework = "next"
command = ["pnpm", "run", "build"]
output = "web-output"
entry = "index.html"
```

Supported framework values are `auto`, `vite`, `next`, `rsbuild`, and `static`.
`auto` detects dependencies from `package.json`. Package-manager detection uses
the `packageManager` field first, then the lockfile. The backend validates the
complete manifest and confirms that the declared entry was uploaded before the
bundle can back a version; build bytes upload directly rather than passing
through the API process as an archive.

## Profiles

Each profile stores its own API URL, access token, expiry, and account. Create
the profiles you need by naming them during authentication:

```sh
maypop --profile local --url http://localhost:3000 auth
maypop --profile dev --url https://api.dev.maypop.ai auth
maypop --profile prod --url https://api.app.maypop.ai auth
```

The first profile becomes the default. List profiles and change the default
with:

```sh
maypop profile list
maypop profile set-default prod
```

Pass `--profile dev`, or set `MAYPOP_PROFILE=dev`, to override the default for
one command. `MAYPOP_URL` and `MAYPOP_TOKEN` remain one-process overrides.
Repositories keep their API URL in local Git configuration, so `publish`,
`info`, and `app apply` automatically find a profile for that URL even when a
different profile is the default. An explicit `--profile` must match the
repository's API URL.

The production API is used when the initial `default` profile is created with
plain `maypop auth`.

## AI media generation

Use the account selected by `maypop auth` to generate media from any directory;
these commands do not need an initialized app or mint an app session:

```sh
maypop ai image \
  --prompt "A full-bleed paper-cut garden, warm neutral palette, no text" \
  --output Images/hero.png \
  --size 2048x1536

maypop ai audio \
  --prompt "A gentle 12-second marimba loop with soft room ambience" \
  --output Audio/theme.mp3

maypop ai video \
  --prompt "Slow dolly through a paper garden at sunrise, leaves moving in the breeze" \
  --output Video/intro.mp4 \
  --duration 8 \
  --ratio 16:9 \
  --generate-audio
```

Image generation supports `--tier fast|quality` and a resolution preset or
`WIDTHxHEIGHT` `--size`. Audio output must end in `.mp3` or `.wav`; its format
defaults to that extension. Video generation supports `--model fast|quality`,
4–30 seconds, `480p` or `720p`, and the documented aspect-ratio choices. Fast
video is limited to 15 seconds. Run each subcommand with `--help` for the exact
options.

Every invocation can consume AI credits. The CLI sends one generation request
and never automatically retries it. Existing files are preserved unless
`--force` is passed, and parent directories are created automatically.

## MCP servers

Connect a custom HTTPS MCP server to the account selected by `maypop auth`.
Authentication headers are read from environment variables so their values do
not appear in shell history:

```sh
export SEARCH_MCP_AUTH="Bearer ..."
maypop mcp connect search https://mcp.example.com \
  --header-env Authorization=SEARCH_MCP_AUTH
maypop mcp list
```

Connections belong to your account. Apps get access only after you explicitly
link a connection from inside a repository initialized by `maypop init`:

```sh
maypop mcp link search
maypop mcp linked
maypop mcp unlink search
```

IDs can be used anywhere a connection name is accepted. A name must be unique
when used as a selector. `maypop mcp disconnect search` removes the account
connection and all of its app links. Use `--json` with `list` or `linked` for
machine-readable output.

Read the server's live tool descriptions and JSON input schemas before calling
one:

```sh
maypop mcp tools search
maypop mcp tools search --json
maypop mcp call search web_search \
  --arguments '{"query":"Maypop SDK"}'
```

`maypop mcp docs` is an alias for `maypop mcp tools`. By default these commands
test the personal connection directly. Add `--app` from an initialized app
repository to exercise the app-linked route and its permissions instead:

```sh
maypop mcp tools search --app --json
maypop mcp call search web_search --app \
  --arguments '{"query":"Maypop SDK"}'
```

Tool calls print the raw MCP result as formatted JSON, including `content`,
`structuredContent`, and `isError` when the server provides them. This makes the
commands suitable for both manual diagnosis and agent-driven verification. The
command exits unsuccessfully when the MCP result reports `isError: true`.

OAuth and managed provider connections still start in Maypop account settings;
once connected there, they appear in `maypop mcp list` and can be linked with
the CLI. The SDK's connected mode can use every linked MCP server. Hybrid mode
can opt into the same real servers with `"mcp"` in `.maypop/dev.json`'s
`remoteCapabilities`.

The API tells the CLI which Git origin to use. The full local stack advertises
`http://localhost:3005/git`; deployments normally advertise the API gateway's
same-origin `/git` route. Set backend `GIT_SERVER_BASE_URL` when exposing a
different Git origin.

Use `--no-browser` to print the approval URL without opening it. Set
`MAYPOP_CONFIG_DIR` to move the profiles file.

Internal diagnostics and administrative operations are excluded from normal
builds. Maintainers can compile them explicitly with:

```sh
cargo install --locked --path . --features admin
```

## Release

The package version in `Cargo.toml` is authoritative. To release `0.3.1`:

1. Set `version = "0.3.1"` in `Cargo.toml`.
2. Run `cargo check` to update the package entry in `Cargo.lock`.
3. Move the relevant entries in `CHANGELOG.md` under a dated `0.3.1` heading.
4. Commit those files with `chore: release v0.3.1`.
5. Create an annotated `v0.3.1` tag and push both `main` and the tag.

The tag is the release trigger. GitHub Actions rejects it unless it exactly
matches the package version, runs the tests, builds every release archive and
checksum, and creates the GitHub Release with generated notes.

## License

Licensed under the [Apache License 2.0](LICENSE).
