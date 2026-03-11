# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is the **Zota fork** of smolVM (`ataul443/smolvm`, forked from `smol-machines/smolvm`). smolVM is an OCI-native microVM runtime for macOS and Linux providing microVM isolation (<200ms boot) using libkrun + Hypervisor.framework (macOS) / KVM (Linux), with an embedded Linux kernel via libkrunfw.

### Zota-specific changes (on `zotavm/main` branch)

- **Alpine 3.23** base rootfs (upgraded from 3.19, required for Claude Code musl binary)
- **Claude Code** pre-installed in the agent rootfs via `curl -fsSL https://claude.ai/install.sh | bash`
- **`zota` user** — non-root user (no sudo), owns global npm/pnpm dirs so `npm install -g` works without root
- **Dev-essential packages** added to rootfs: git, curl, bash, nodejs, npm, python3, build-base, openssh-client, ripgrep, coreutils, findutils, diffutils, nano, wget, and more
- **Release workflow** (`.github/workflows/release.yml`) triggered on `zotavm-v*` tags
- `main` branch is kept in sync with upstream for merging; all Zota changes live on `zotavm/main`

## Build & Development Commands

```bash
# Prerequisites: Rust, git-lfs, Docker, e2fsprogs, LLVM (macOS: brew install llvm)
# macOS setup: brew install git-lfs e2fsprogs llvm && git lfs install && git lfs pull

# Set PATH for keg-only homebrew packages
export PATH="/opt/homebrew/opt/e2fsprogs/bin:/opt/homebrew/opt/e2fsprogs/sbin:/opt/homebrew/opt/llvm/bin:$PATH"

# Build agent rootfs (Alpine 3.23 + Claude Code + dev tools)
# Uses --privileged Docker for chroot with network access
./scripts/build-agent-rootfs.sh

# Full distribution build (requires agent rootfs built first)
./scripts/build-dist.sh

# Build with local libkrun changes from ../libkrun
./scripts/build-dist.sh --with-local-libkrun

# Quick cargo build (needs LIBRARY_PATH and git-lfs pulled)
LIBRARY_PATH=$PWD/lib cargo build --release

# Unit tests only (no VM/Hypervisor required)
LIBRARY_PATH=$PWD/lib DYLD_LIBRARY_PATH=$PWD/lib cargo test --lib

# All integration tests (requires Hypervisor.framework or KVM)
./tests/run_all.sh

# Single test suite
./tests/test_cli.sh
./tests/test_sandbox.sh
./tests/test_microvm.sh
./tests/test_container.sh
./tests/test_api.sh
./tests/test_pack.sh
./tests/test_smolfile.sh

# Formatting and linting
cargo fmt --all -- --check
LIBRARY_PATH=$PWD/lib cargo clippy --all-targets -- -D warnings

# Rebuild agent quickly
./scripts/rebuild-agent.sh

# Build agent for specific target (static musl binary)
cargo build --release --target x86_64-unknown-linux-musl -p smolvm-agent
cargo build --release --target aarch64-unknown-linux-musl -p smolvm-agent
```

## Releasing

Tags use `zotavm-v*` prefix to distinguish from upstream releases:
```bash
git tag zotavm-v0.1.0
git push origin zotavm-v0.1.0
```
This triggers `.github/workflows/release.yml` which builds the dist tarball on macOS ARM64 and creates a GitHub Release.

## Syncing with upstream

```bash
git remote add upstream git@github.com:smol-machines/smolvm.git  # one-time
git checkout main
git fetch upstream
git merge upstream/main
git push origin main
git checkout zotavm/main
git merge main
```

## Architecture

### Workspace Crates

- **smolvm** (root) — CLI binary + library. The main runtime.
- **smolvm-protocol** (`crates/smolvm-protocol`) — Wire protocol for host-guest communication over vsock. JSON messages with 4-byte big-endian length headers.
- **smolvm-agent** (`crates/smolvm-agent`) — Guest agent binary. Runs inside the VM (Alpine Linux musl). Handles OCI operations, exec, container lifecycle.
- **smolvm-pack** (`crates/smolvm-pack`) — Single-binary packaging logic (embed VM+rootfs into a self-extracting executable).
- **smolvm-napi** (`crates/smolvm-napi`) — Node.js native bindings via napi-rs.

### Source Layout (`src/`)

- **cli/** — Clap-based CLI with subcommands: `sandbox`, `microvm`, `container`, `serve`, `pack`, `config`
- **vm/** — VM backend abstraction (`VmBackend` trait, `VmHandle`, `VmConfig`). Wraps libkrun FFI.
- **api/** — HTTP REST API (Axum + OpenAPI/utoipa). Runs via `smolvm serve`.
- **agent/** — Host-side agent client. Manages guest agent lifecycle over vsock.
- **platform/** — OS/arch abstraction. `macos.rs` (Hypervisor.framework, Rosetta 2), `linux.rs` (KVM). `VmExecutor` trait.
- **config.rs** — Persistent VM configuration (redb database)
- **db.rs** — redb-based state persistence
- **registry.rs** — OCI image pulling and layer caching
- **storage.rs** — ext4 disk image management
- **mount.rs** — virtiofs directory mounts
- **process.rs** — Child process lifecycle

### Agent Rootfs (`scripts/build-agent-rootfs.sh`)

The VM boots an Alpine 3.23 minirootfs with:
- **PID 1**: `/sbin/init` symlinked to `/usr/local/bin/smolvm-agent`
- **Crane** v0.19.0 for OCI image operations
- **Claude Code** installed under `/home/zota/.local/bin/claude`
- **User**: `zota` (non-root, no sudo, owns global package dirs)
- **Packages**: jq, e2fsprogs, crun, util-linux, libcap, git, curl, bash, nodejs, npm, python3, build-base, openssh-client, ripgrep, coreutils, findutils, diffutils, and more
- Build uses `--privileged` Docker to bind-mount `/proc`, `/sys`, `/dev` into the rootfs chroot for network access during npm/curl installs

### Communication Protocol (vsock)

Host-guest communication uses JSON over vsock with 4-byte length-prefixed frames:
- Port 5000: workload control
- Port 5001: log streaming
- Port 6000: agent control
- CID 2 = host, CID 3 = guest
- Max frame: 32 MB, layer chunks: 16 MB

### Embedded SDKs (`sdks/`)

- **node/** — Primary embedded SDK. Bundles libkrun/libkrunfw with platform-specific npm packages (darwin-arm64, darwin-x64, linux-arm64-gnu, linux-x64-gnu).

## Key Build Details

- **libkrun linking**: Controlled by `build.rs`. Checks env vars in order: `LIBKRUN_BUILD` -> `LIBKRUN_BUNDLE` -> `LIBKRUN_STATIC` -> `LIBKRUN_DIR` -> bundled `lib/` -> pkg-config -> common paths. On macOS, uses weak linking (`-Wl,-weak-lkrun`) for packed binary mode.
- **git-lfs**: Required. `lib/` contains pre-built dylibs tracked via LFS. Build will fail with LFS pointers.
- **macOS code signing**: Binary must have `com.apple.security.hypervisor` entitlement.
- **Agent**: Cross-compiled as a static musl binary. Built with `release-small` profile (size-optimized, `panic=abort`).
- **Formatting**: `rustfmt.toml` sets `max_width = 100`.
- **Alpine 3.23 requirement**: Claude Code's musl binary needs `posix_getdents` which is only available in musl >= 1.2.5 (Alpine 3.23+). Alpine 3.21 and earlier will fail.

## Troubleshooting

Database lock errors ("Database already open"):
```bash
pkill -f "smolvm serve"
pkill -f "smolvm-bin microvm start"
```

Integration tests require Hypervisor.framework (macOS) or KVM (Linux) — CI only runs unit tests.
