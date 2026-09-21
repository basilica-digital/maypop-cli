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

The default binary has seven product commands:

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
- `maypop profile` lists saved profiles and selects the default.

The credential helper reads the matching profile and only answers for that
account's path on the configured Maypop Git host. The token is not embedded in
the remote URL or `.git/config`; ordinary `git pull` and `git push` work after
`maypop init`.

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

The API tells the CLI which Git origin to use. The full local stack advertises
`http://localhost:3005/git`; deployments normally advertise the API gateway's
same-origin `/git` route. Set backend `GIT_SERVER_BASE_URL` when exposing a
different Git origin.

Use `--no-browser` to print the approval URL without opening it. Set
`MAYPOP_CONFIG_DIR` to move the profiles file.

## Authenticated SDK development

The Vite, Rsbuild, and Next.js integrations in
`@basilica-digital/maypop-sdk` can use the CLI login for authenticated local
development. Their developer-local `.maypop/dev.json` selects hybrid or
connected mode and may name a profile. The SDK asks the CLI to mint a
short-lived session for the app recorded in the repository; app access is
checked by Maypop, and the saved CLI credential is never handed to browser
code.

When no profile is named, the same rules as repository commands apply: the
repository's API URL selects a matching credential, with `MAYPOP_PROFILE`
available when more than one account uses that endpoint.

Internal diagnostics and administrative operations are excluded from normal
builds. Maintainers can compile them explicitly with:

```sh
cargo install --locked --path . --features admin
```

## Release

The package version in `Cargo.toml` is authoritative. To release `0.2.0`:

1. Set `version = "0.2.0"` in `Cargo.toml`.
2. Run `cargo check` to update the package entry in `Cargo.lock`.
3. Move the relevant entries in `CHANGELOG.md` under a dated `0.2.0` heading.
4. Commit those files with `chore: release v0.2.0`.
5. Create an annotated `v0.2.0` tag and push both `main` and the tag.

The tag is the release trigger. GitHub Actions rejects it unless it exactly
matches the package version, runs the tests, builds every release archive and
checksum, and creates the GitHub Release with generated notes.

## License

Licensed under the [Apache License 2.0](LICENSE).
