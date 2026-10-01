# Changelog

All notable changes to the Maypop CLI are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [Semantic Versioning](https://semver.org/).

## Unreleased

### Fixed

- Errors print their full cause, so a rejected `maypop publish` shows the Git server's reason instead of only `Git push failed`.

## 0.5.0 - 2026-10-01

### Added

- Add `maypop reference import` to import released app source into local reference directories.

## 0.4.0 - 2026-09-29

### Changed

- `maypop init` writes a `.maypop/.gitignore` so `.maypop/local/`, where the Maypop SDK keeps its files for one machine, is never committed, without touching your `.gitignore`; `.maypop/kv-policy.json` and `.maypop/publish/` stay committed.

## 0.3.1 - 2026-09-22

### Fixed

- Push publication commits without delta compression or thin packs for compatibility with the Maypop Git server.

## 0.3.0 - 2026-09-22

### Added

- Generate image, audio, and video files with the authenticated account through `maypop ai`.

## 0.2.0 - 2026-09-22

### Added

- Connect MCP servers to an account and control which servers an app can use.
- Inspect live MCP tool documentation and invoke tools from the CLI.

## 0.1.2 - 2026-09-21

### Fixed

- Reuse app metadata from an existing `maypop.toml` during initialization.

## 0.1.1 - 2026-09-21

### Fixed

- Publish bundles through Google-hosted endpoints without failing with HTTP 411.

## 0.1.0 - 2026-09-21

### Added

- Authenticate Maypop accounts through a browser device flow.
- Initialize Git-backed Maypop applications and detect their build adapters.
- Apply app metadata from `maypop.toml`.
- Build and publish immutable app versions.
- Manage credentials for multiple Maypop profiles and environments.
