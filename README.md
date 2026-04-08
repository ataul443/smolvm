<p align="center">
  <img src="assets/logo.png" alt="smol machines" width="80">
</p>

<p align="center">
  <a href="https://discord.gg/E5r8rEWY9J"><img src="https://img.shields.io/badge/Discord-Join-5865F2?logo=discord&logoColor=white" alt="Discord"></a>
  <a href="https://github.com/smol-machines/smolvm/releases"><img src="https://img.shields.io/github/v/release/smol-machines/smolvm?label=Release" alt="Release"></a>
  <a href="https://github.com/smol-machines/smolvm/blob/main/LICENSE"><img src="https://img.shields.io/badge/License-Apache_2.0-blue.svg" alt="License"></a>
</p>

smolvm
======

Ship and run software with isolation by default.

This is a CLI tool that lets you:
1. Manage and run custom Linux virtual machines locally with: sub-second cold start, cross-platform (macOS, Linux), elastic memory usage.
2. Pack a stateful virtual machine into a single file (.smolmachine) to rehydrate on any supported platform.

Install
-------

```bash
# install (macOS + Linux)
curl -sSL https://smolmachines.com/install.sh | bash

# for coding agents — install + discover all commands
curl -sSL https://smolmachines.com/install.sh | bash && smolvm --help
```

Or download from [GitHub Releases](https://github.com/smol-machines/smolvm/releases).

Quick Start
-----------

```bash
# run a command in an ephemeral VM (cleaned up after exit)
smolvm machine run --net --image alpine -- sh -c "echo 'Hello world from a microVM' && uname -a"

# interactive shell
smolvm machine run --net -it --image alpine -- /bin/sh
# inside the VM: apk add sl && sl && exit
```

Use This For
------------

**Sandbox untrusted code** — run untrusted programs in a hardware-isolated VM. Host filesystem, network, and credentials are separated by a hypervisor boundary.

```bash
# network is off by default — untrusted code can't phone home
smolvm machine run --image alpine -- nslookup example.com
# fails — no network access

# lock down egress — only allow specific hosts
smolvm machine run --net --image alpine --allow-host registry.npmjs.org -- wget -q -O /dev/null https://registry.npmjs.org
# works — allowed host

smolvm machine run --net --image alpine --allow-host registry.npmjs.org -- wget -q -O /dev/null https://google.com
# fails — not in allow list
```

**Pack into portable executables** — turn any workload into a self-contained binary. All dependencies are pre-baked — no install step, no runtime downloads, boots in <200ms.

```bash
smolvm pack create --image python:3.12-alpine -o ./python312
./python312 run -- python3 --version
# Python 3.12.x — isolated, no pyenv/venv/conda needed
```

**Persistent machines for development** — create, stop, start. Installed packages survive restarts.

```bash
smolvm machine create --net myvm
smolvm machine start --name myvm
smolvm machine exec --name myvm -- apk add sl
smolvm machine exec --name myvm -it -- /bin/sh
# inside: sl, ls, uname -a — type 'exit' to leave
smolvm machine stop --name myvm
```

**Use git and SSH without exposing keys** — forward your host SSH agent into the VM. Private keys never enter the guest — the hypervisor enforces this. Requires an SSH agent running on your host (`ssh-add -l` to check).

```bash
smolvm machine run --ssh-agent --net --image alpine -- sh -c "apk add -q openssh-client && ssh-add -l"
# lists your host keys, but they can't be extracted from inside the VM

smolvm machine exec --name myvm -- git clone git@github.com:org/private-repo.git
```

**Declare environments with a Smolfile** — reproducible VM config in a simple TOML file.

```toml
image = "python:3.12-alpine"
net = true

[network]
allow_hosts = ["api.stripe.com", "db.example.com"]

[dev]
init = ["pip install -r requirements.txt"]
volumes = ["./src:/app"]

[auth]
ssh_agent = true
```

```bash
smolvm machine create myvm -s Smolfile
smolvm machine start --name myvm
```

More examples: [python](https://github.com/smol-machines/smolvm/tree/main/examples/python-app) · [node](https://github.com/smol-machines/smolvm/tree/main/examples/node-app) · [doom](https://github.com/smol-machines/smolvm/tree/main/examples/doom-web)

How It Works
------------

Each workload gets real hardware isolation — its own kernel on [Hypervisor.framework](https://developer.apple.com/documentation/hypervisor) (macOS) or KVM (Linux). [libkrun](https://github.com/containers/libkrun) VMM with custom kernel: [libkrunfw](https://github.com/smol-machines/libkrunfw). Pack it into a `.smolmachine` and it runs anywhere the host architecture matches, with zero dependencies.

Images use the [OCI](https://opencontainers.org/) format — the same open standard Docker uses. Any image on Docker Hub, ghcr.io, or other OCI registries can be pulled and booted as a microVM. No Docker daemon required.

Defaults: 4 vCPUs, 8 GiB RAM. Memory is elastic via virtio balloon — the host only commits what the guest actually uses and reclaims the rest automatically. vCPU threads sleep in the hypervisor when idle, so over-provisioning has near-zero cost. Override with `--cpus` and `--mem`.

Comparison
----------

|                     | smolvm | Containers | Colima | QEMU | Firecracker | Kata |
|---------------------|--------|------------|--------|------|-------------|------|
| Isolation           | VM per workload | Namespace (shared kernel) | Namespace (1 VM) | Separate VM | Separate VM | VM per container |
| Boot time           | <200ms | ~100ms | ~seconds | ~15-30s | <125ms | ~500ms |
| Architecture        | Library (libkrun) | Daemon | Daemon (in VM) | Process | Process | Runtime stack |
| Per-workload VMs    | Yes | No | No (shared) | Yes | Yes | Yes |
| macOS native        | Yes | Via Docker VM | Yes (krunkit) | Yes | No | No |
| Embeddable SDK      | Yes | No | No | No | No | No |
| Portable artifacts  | `.smolmachine` | Images (need daemon) | No | No | No | No |

Platform Support
----------------

| Host | Guest | Requirements |
|------|-------|-------------|
| macOS Apple Silicon | arm64 Linux | macOS 11+ |
| macOS Intel | x86_64 Linux | macOS 11+ (untested) |
| Linux x86_64 | x86_64 Linux | KVM (`/dev/kvm`) |
| Linux aarch64 | aarch64 Linux | KVM (`/dev/kvm`) |

Multi-VM Features
-----------------

**Fork a machine** — create a new VM from an existing one with copy-on-write disk semantics. The forked machine inherits all installed packages and files without duplicating the full disk.

```bash
smolvm machine create base --net --cpus 2 --mem 4096
smolvm machine start -n base
smolvm machine exec -n base -- apk add python3 nodejs
smolvm machine stop -n base

# Fork into independent workers (CoW — near-instant on btrfs/APFS)
smolvm machine fork --source base --name worker-1
smolvm machine fork --source base --name worker-2
smolvm machine start -n worker-1
smolvm machine start -n worker-2
```

**Inter-VM networking** — group machines together so they can communicate via host port forwarding.

```bash
smolvm machine create server --group myapp --net -p 19000:9000
smolvm machine create client --group myapp --net
smolvm machine start -n server
smolvm machine start -n client

# List peers and their port mappings
smolvm machine peers --group myapp

# Auto-allocate a host port to bridge two VMs
smolvm machine connect client server --port 9000
```

**Copy files between VMs** — transfer files between two running machines.

```bash
smolvm machine cp vm-a:/data/output.json vm-b:/data/input.json
```

Build from Source
-----------------

### Prerequisites

| Dependency | Linux | macOS |
|-----------|-------|-------|
| Rust | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` | same |
| git-lfs | `sudo apt install git-lfs` | `brew install git-lfs` |
| e2fsprogs | `sudo apt install e2fsprogs` | `brew install e2fsprogs` |
| Docker | `sudo apt install docker.io` | Docker Desktop |
| KVM | `/dev/kvm` must be accessible | N/A (uses Hypervisor.framework) |
| musl target | `rustup target add x86_64-unknown-linux-musl` | `rustup target add aarch64-unknown-linux-musl` |

### Step 1: Clone and pull LFS objects

```bash
git clone https://github.com/ataul443/smolvm.git
cd smolvm
git lfs install && git lfs pull
```

### Step 2: Build the agent rootfs

The agent rootfs is an Alpine Linux filesystem that runs inside each VM. It requires Docker and `sudo` for package installation.

```bash
# First, build the static musl agent binary (as your user, not sudo)
rustup target add x86_64-unknown-linux-musl  # or aarch64- on ARM
cargo build --profile release-small --target x86_64-unknown-linux-musl -p smolvm-agent

# Then build the rootfs (needs sudo for apk package install)
sudo AGENT_BINARY=$PWD/target/x86_64-unknown-linux-musl/release-small/smolvm-agent \
  ./scripts/build-agent-rootfs.sh --no-build-agent
```

### Step 3: Build smolvm

```bash
# Linux — use the linux-x86_64 libs
LIBRARY_PATH=$PWD/lib/linux-x86_64 cargo build --release

# macOS — use the root lib/ directory
# export PATH="/opt/homebrew/opt/e2fsprogs/bin:/opt/homebrew/opt/llvm/bin:$PATH"
# LIBRARY_PATH=$PWD/lib cargo build --release
```

### Step 4: Install the rootfs and binary

```bash
# Copy rootfs to smolvm's data directory
mkdir -p ~/.local/share/smolvm
cp -r target/agent-rootfs ~/.local/share/smolvm/agent-rootfs

# Add smolvm to your PATH
export PATH="$PWD/target/release:$PATH"
export LD_LIBRARY_PATH="$PWD/lib/linux-x86_64:$LD_LIBRARY_PATH"  # Linux
# export DYLD_LIBRARY_PATH="$PWD/lib:$DYLD_LIBRARY_PATH"  # macOS
```

### Step 5: Verify

```bash
smolvm machine create test1 --cpus 2 --mem 2048
smolvm machine start -n test1
smolvm machine exec -n test1 -- echo "Hello from smolvm!"
smolvm machine stop -n test1
smolvm machine delete -n test1
```

### Troubleshooting

| Problem | Fix |
|---------|-----|
| `unable to find library -lkrun` | Use `LIBRARY_PATH=$PWD/lib/linux-x86_64` (Linux) or `$PWD/lib` (macOS) |
| `Database already open` | `pkill -f "smolvm serve"; pkill -f "smolvm-bin"` |
| `/dev/kvm` not accessible | `sudo chmod 666 /dev/kvm` or add user to `kvm` group |
| `agent-rootfs` not found | Run the rootfs build (Step 2) and copy to `~/.local/share/smolvm/` |
| `apk.static: Use --usermode` | The rootfs build needs `sudo` — don't run it as a regular user |
| `curl: (23) Failure writing` | Clean stale files: `sudo rm -rf /tmp/apk-static* target/agent-rootfs` and retry |
| `Cannot build smolvm-agent` | Build the agent separately first, then pass `AGENT_BINARY=... --no-build-agent` |
| Fork fails "is running" | Stop the source VM first: `smolvm machine stop -n <name>` |

Known Limitations
-----------------

* Network is opt-in (`--net` on `machine create`). TCP/UDP only, no ICMP.
* Volume mounts: directories only (no single files).
* macOS: binary must be signed with Hypervisor.framework entitlements.
* `--ssh-agent` requires an SSH agent running on the host (`SSH_AUTH_SOCK` must be set).
* GPU support is currently being worked on [in a separate branch](https://github.com/smol-machines/smolvm/tree/binbin-gpu-support).
* VM fork requires the source machine to be stopped (for disk consistency).
* Inter-VM networking uses host port forwarding via TSI — not direct veth/bridge.

Development
-----------

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

[Apache-2.0](LICENSE) · made by [@binsquare](https://github.com/BinSquare) · [twitter](https://x.com/binsquares) · [github](https://github.com/smol-machines/smolvm)
