# Changelog

All notable changes to the Maypop CLI are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [Semantic Versioning](https://semver.org/).

## Unreleased

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
