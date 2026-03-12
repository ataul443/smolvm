# RFC 001: Copy-on-Write Volume Mounts

**Status:** Draft
**Author:** smolvm contributors
**Created:** 2026-03-12

## Summary

Add a new `cow` (copy-on-write) mount mode to smolVM that allows containers to see the full contents of a host-mounted directory but keeps all modifications (writes, deletes, renames) local to the container. The host directory is never modified.

## Motivation

Today smolVM supports two volume mount modes:

- **`rw`** (writable) — writes propagate directly to the host via virtiofs
- **`ro`** (read-only) — writes are rejected entirely

Neither satisfies a common use case: **mount a host project directory so the container can read the source code, build artifacts, install dependencies, and modify files — without altering anything on the host.** This is essential for:

- **Sandboxed CI/CD:** Run builds against a host checkout without polluting the working tree
- **Safe experimentation:** Let an AI agent or developer try changes inside a container, inspect results, and discard them
- **Reproducible environments:** Multiple containers can mount the same host directory and each see their own isolated view
- **Security:** Prevent a compromised container from modifying host files even when it needs read access

Docker and Podman don't natively offer per-mount COW either (they rely on tmpfs overlays or external tools). smolVM can provide this as a first-class feature because it controls both the host launcher and the in-guest agent.

## Design

### User-Facing Interface

#### CLI

Extend the existing `-v` / `--volume` mount syntax with a `cow` mode:

```
# Existing modes (unchanged)
smolvm sandbox run -v /host/path:/container/path        # rw (default)
smolvm sandbox run -v /host/path:/container/path:ro      # read-only
smolvm sandbox run -v /host/path:/container/path:rw      # explicit rw

# New mode
smolvm sandbox run -v /host/path:/container/path:cow     # copy-on-write
```

#### HTTP API

Extend `MountSpec` with an optional `mode` field:

```json
{
  "source": "/Users/me/code",
  "target": "/workspace",
  "readonly": false,
  "mode": "cow"
}
```

For backward compatibility, when `mode` is absent the current behavior applies: `readonly: true` → `ro`, `readonly: false` → `rw`.

#### SDK (Node.js)

```javascript
const sandbox = await smolvm.sandbox.create({
  mounts: [{
    source: "/Users/me/code",
    target: "/workspace",
    mode: "cow"
  }]
});
```

### Architecture

The COW mount is implemented as an **overlayfs** mount at the **container level** via the OCI runtime spec, not as an agent-level mount hack. This is the cleanest approach because:

1. **Per-container isolation** — each container gets its own upper layer
2. **Automatic cleanup** — upper layer is removed when the container is deleted
3. **Leverages existing infrastructure** — crun already processes OCI mount entries as root before dropping privileges
4. **Consistent** — uses the same overlayfs mechanism that already provides COW for image layers

#### Data Flow

```
Host                          Guest VM (Alpine)                Container (crun)
────                          ────────────────                 ─────────────────

/Users/me/code ──virtiofs──▶ /mnt/virtiofs/smolvm0
                              (staging, read-only lower)
                                                               overlayfs mount:
                                                                 lowerdir = /mnt/virtiofs/smolvm0
                                                                 upperdir = /mnt/cow/<container_id>/smolvm0/upper
                                                                 workdir  = /mnt/cow/<container_id>/smolvm0/work
                                                                 merged   = /workspace (inside container)
```

#### OCI Spec Generation

Currently, `add_bind_mount()` in `oci.rs` generates:

```json
{
  "destination": "/workspace",
  "type": "bind",
  "source": "/mnt/virtiofs/smolvm0",
  "options": ["bind", "rprivate"]
}
```

For COW mounts, a new `add_overlay_mount()` method generates:

```json
{
  "destination": "/workspace",
  "type": "overlay",
  "source": "overlay",
  "options": [
    "lowerdir=/mnt/virtiofs/smolvm0",
    "upperdir=/mnt/cow/<container_id>/smolvm0/upper",
    "workdir=/mnt/cow/<container_id>/smolvm0/work"
  ]
}
```

crun executes `mount("overlay", "/workspace", "overlay", 0, "lowerdir=...,upperdir=...,workdir=...")` as root before starting the container process. This is a standard kernel overlayfs mount — no special crun features required.

### Implementation Plan

#### Layer 1: Protocol (`smolvm-protocol`)

Add a `MountMode` enum to the protocol:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MountMode {
    /// Direct bind mount — writes propagate to host (default).
    #[default]
    Bind,
    /// Read-only bind mount — writes are rejected.
    ReadOnly,
    /// Copy-on-write overlay — reads from host, writes stay in container.
    Cow,
}
```

Update the mount tuple format in `AgentRequest::Run`, `AgentRequest::CreateContainer`, and `AgentRequest::Exec` from `Vec<(String, String, bool)>` to a struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountEntry {
    /// Virtiofs tag (e.g., "smolvm0").
    pub tag: String,
    /// Mount path inside the container.
    pub container_path: String,
    /// Mount mode.
    pub mode: MountMode,
}
```

**Backward compatibility:** The agent should accept both the old tuple format and the new struct format during a transition period. The old `(tag, path, read_only)` tuple maps to `MountEntry { tag, container_path, mode: if read_only { ReadOnly } else { Bind } }`.

#### Layer 2: Host-side types (`smolvm` crate)

**`src/vm/config.rs`** — Add `MountMode` to `HostMount`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MountMode {
    #[default]
    Rw,
    Ro,
    Cow,
}

pub struct HostMount {
    pub source: PathBuf,
    pub target: PathBuf,
    pub read_only: bool,   // kept for backward compat
    pub mode: MountMode,   // new field, takes precedence
}
```

**`src/mount.rs`** — Update `MountBinding`, `parse_mount_spec()`, `validate_mount()`:

```rust
// Parse: host:guest[:ro|:rw|:cow]
[source, target, "cow"] => Ok(HostMount::new_cow(source, target)),
```

**`src/api/types.rs`** — Add optional `mode` field to `MountSpec` and `ContainerMountSpec`.

#### Layer 3: Guest agent (`smolvm-agent`)

**`crates/smolvm-agent/src/oci.rs`** — Add `add_overlay_mount()`:

```rust
impl OciSpec {
    pub fn add_overlay_mount(
        &mut self,
        lower_source: &str,
        destination: &str,
        upper_dir: &str,
        work_dir: &str,
    ) {
        self.mounts.push(OciMount {
            destination: destination.to_string(),
            mount_type: Some("overlay".to_string()),
            source: "overlay".to_string(),
            options: vec![
                format!("lowerdir={}", lower_source),
                format!("upperdir={}", upper_dir),
                format!("workdir={}", work_dir),
            ],
        });
    }
}
```

**`crates/smolvm-agent/src/storage.rs`** — Update `setup_volume_mounts()` and `run_command()`:

```rust
fn setup_volume_mounts(
    rootfs: &str,
    mounts: &[MountEntry],
    container_id: &str,  // needed for per-container upper dirs
) -> Result<Vec<PathBuf>> {
    for mount in mounts {
        // Step 1: Mount virtiofs at staging (unchanged)
        let virtiofs_mount = Path::new(paths::VIRTIOFS_MOUNT_ROOT).join(&mount.tag);
        mount_virtiofs_if_needed(&mount.tag, &virtiofs_mount)?;

        match mount.mode {
            MountMode::Bind | MountMode::ReadOnly => {
                // Existing behavior: bind mount into container rootfs
                bind_mount_into_rootfs(&virtiofs_mount, rootfs, &mount.container_path, mount.mode == MountMode::ReadOnly)?;
            }
            MountMode::Cow => {
                // New: create overlay dirs, bind mount will be handled by crun
                // via OCI spec overlay mount entry
                let cow_root = Path::new(paths::COW_MOUNT_ROOT)
                    .join(container_id)
                    .join(&mount.tag);
                std::fs::create_dir_all(cow_root.join("upper"))?;
                std::fs::create_dir_all(cow_root.join("work"))?;
            }
        }
    }
}
```

For COW mounts, instead of calling `spec.add_bind_mount()`, the agent calls `spec.add_overlay_mount()`:

```rust
for mount in mounts {
    let virtiofs_mount = Path::new(paths::VIRTIOFS_MOUNT_ROOT).join(&mount.tag);
    match mount.mode {
        MountMode::Bind => spec.add_bind_mount(&virtiofs_mount, &mount.container_path, false),
        MountMode::ReadOnly => spec.add_bind_mount(&virtiofs_mount, &mount.container_path, true),
        MountMode::Cow => {
            let cow_root = Path::new(paths::COW_MOUNT_ROOT)
                .join(container_id)
                .join(&mount.tag);
            spec.add_overlay_mount(
                &virtiofs_mount.to_string_lossy(),
                &mount.container_path,
                &cow_root.join("upper").to_string_lossy(),
                &cow_root.join("work").to_string_lossy(),
            );
        }
    }
}
```

**`crates/smolvm-agent/src/paths.rs`** — Add constant:

```rust
/// Root directory for COW overlay upper/work dirs.
pub const COW_MOUNT_ROOT: &str = "/mnt/cow";
```

#### Layer 4: CLI (`src/cli/`)

**`src/cli/parsers.rs`** — Extend `parse_mount_spec()` to accept `:cow`.

**`src/cli/sandbox.rs`**, **`src/cli/container.rs`** — No changes needed; they pass through the parsed mount structs.

#### Layer 5: Cleanup

When a container is deleted, the agent removes its COW upper/work directories:

```rust
fn cleanup_cow_mounts(container_id: &str) {
    let cow_dir = Path::new(paths::COW_MOUNT_ROOT).join(container_id);
    if cow_dir.exists() {
        let _ = std::fs::remove_dir_all(&cow_dir);
    }
}
```

This is called from the existing container deletion path in `container.rs`.

### Storage Location for Upper Layers

The `upperdir` and `workdir` must reside on a filesystem that supports overlayfs (ext4, xfs — not tmpfs on older kernels). Two options:

| Location | Pros | Cons |
|----------|------|------|
| **Storage disk (`/dev/vda`)** | Persistent, survives container restart, large capacity | Shared I/O with image layers |
| **Overlay disk (`/dev/vdb`)** | Separate I/O path, already used for rootfs overlay | Limited size (default 2 GiB) |

**Recommendation:** Use the **storage disk** (`/mnt/storage/cow/`). It has more capacity (default 20 GiB), is already formatted ext4, and persistence across container restarts is a useful property. The COW upper dirs are small relative to the storage disk since they only contain deltas.

The path hierarchy:

```
/mnt/storage/cow/
└── <container_id>/
    ├── smolvm0/
    │   ├── upper/    ← writes for first mounted volume
    │   └── work/     ← overlayfs internal
    └── smolvm1/
        ├── upper/    ← writes for second mounted volume
        └── work/
```

### Constraints and Limitations

1. **Kernel requirement:** The guest kernel (embedded via libkrunfw) must support overlayfs. This is standard in Linux 4.x+ and is already used by smolVM for image layer overlays — no new requirement.

2. **File ownership:** overlayfs preserves uid/gid from the lower layer. Files created in the upper layer get the uid/gid of the container process. This matches standard container behavior.

3. **Hard links across layers:** overlayfs does not support hard links between files that exist in different layers (lower vs upper). This is a known overlayfs limitation and is unlikely to matter in practice.

4. **inotify:** inotify events on the lower layer (host changes) are not visible through the overlay. The container sees a snapshot of the host directory at mount time. Host-side changes to existing files will be visible on read (overlayfs checks lower layer on cache miss), but new files added on the host after mount may not appear until the overlay dentry cache expires.

5. **xattr support:** Depends on the upper layer filesystem. ext4 supports xattrs, so this should work.

6. **Nested overlayfs:** The container rootfs is already an overlayfs (image layers). Mounting another overlayfs inside it works on Linux 5.11+ (nested overlay support). The libkrunfw kernel version should be verified. If nested overlay is not supported, the agent can fall back to bind-mounting the merged view from a pre-mounted overlayfs (done at agent level instead of crun level).

### Wire Protocol Compatibility

The protocol change (tuple → `MountEntry` struct) is backward-incompatible at the message level. To handle mixed-version host/agent:

- **New host + old agent:** The host sends the new `MountEntry` format. The old agent will fail to deserialize `MountMode::Cow` entries. Since the agent runs inside the VM and is bundled with the rootfs, host and agent versions are always in sync. **No compatibility issue in practice.**

- **Serialization:** `MountEntry` with `mode: "bind"` / `mode: "readonly"` is a clean superset of the old boolean. The old tuple format `(tag, path, read_only)` can be dropped immediately since the agent binary is always deployed alongside the host binary.

### Testing Strategy

#### Unit Tests

- `oci.rs`: Test `add_overlay_mount()` generates correct OCI spec JSON
- `parsers.rs`: Test `parse_mount_spec()` accepts `:cow` and rejects invalid modes
- `mount.rs`: Test `MountBinding` with `MountMode::Cow`
- `protocol`: Test `MountEntry` serialization/deserialization

#### Integration Tests

Add to `tests/test_sandbox.sh`:

```bash
# Test COW mount: write in container, verify host unchanged
echo "original" > /tmp/test-cow/file.txt
smolvm sandbox run -v /tmp/test-cow:/workspace:cow alpine -- \
    sh -c 'echo "modified" > /workspace/file.txt && cat /workspace/file.txt'
# Output should show "modified" (container sees its write)

# Verify host file is unchanged
[ "$(cat /tmp/test-cow/file.txt)" = "original" ] || echo "FAIL: host was modified"

# Test COW mount: new files in container don't appear on host
smolvm sandbox run -v /tmp/test-cow:/workspace:cow alpine -- \
    touch /workspace/new-file.txt
[ ! -f /tmp/test-cow/new-file.txt ] || echo "FAIL: new file leaked to host"

# Test COW mount: deleting files in container doesn't affect host
smolvm sandbox run -v /tmp/test-cow:/workspace:cow alpine -- \
    rm /workspace/file.txt
[ -f /tmp/test-cow/file.txt ] || echo "FAIL: host file was deleted"
```

#### Edge Case Tests

- COW mount with empty host directory
- COW mount with deeply nested directory structures
- COW mount with symlinks in the host directory
- COW mount with special files (sockets, FIFOs) — should be excluded
- Multiple containers with COW mounts to the same host directory (isolation test)
- Container restart with COW mount (upper layer persists if same container ID)
- Disk space exhaustion on the storage disk during COW writes

### Migration and Rollout

1. **Phase 1:** Implement `MountMode::Cow` end-to-end, gated behind the `:cow` CLI flag. Existing `:ro` and `:rw` behavior is unchanged. No default behavior changes.

2. **Phase 2:** Add `mode` field to HTTP API `MountSpec`. Old API clients that omit `mode` get the existing `readonly`-based behavior.

3. **Phase 3 (future, optional):** Consider making `cow` the default for sandbox mounts where the user hasn't specified a mode. This would be a breaking change and needs separate discussion.

### Alternatives Considered

#### 1. Agent-level overlayfs (before crun)

Mount the overlayfs at the agent level in `setup_volume_mounts()` and bind-mount the merged dir into the container.

**Rejected because:**
- All containers share the same upper layer (no per-container isolation)
- Cleanup requires the agent to track which overlays belong to which container
- Adds complexity to the agent mount code path

#### 2. tmpfs upper layer

Use a tmpfs-backed upper layer instead of the storage disk.

**Rejected because:**
- Consumes VM RAM for file writes
- Lost on any container restart
- tmpfs doesn't support overlayfs upperdir on older kernels

#### 3. FUSE-based COW (e.g., unionfs-fuse)

Run a userspace filesystem to intercept writes.

**Rejected because:**
- Adds a runtime dependency (FUSE binary)
- Significantly slower than kernel overlayfs
- More complex failure modes

#### 4. Client-side snapshot + rsync

Copy the host directory into the VM and mount the copy.

**Rejected because:**
- O(n) in directory size at startup (could be gigabytes)
- Wastes disk space (full copy, not just deltas)
- Not practical for large codebases

## Unresolved Questions

1. **Should COW upper layers persist across container restarts?** Current design says yes (stored on persistent storage disk). This means stopping and restarting a container preserves its writes. If ephemeral behavior is preferred, upper layers could be stored on tmpfs or cleaned on container stop.

2. **Should there be a way to "commit" COW changes back to the host?** A future `smolvm sandbox commit-mounts` command could rsync the upper layer back to the host. This is out of scope for this RFC but worth noting as a possible follow-up.

3. **Nested overlayfs kernel version.** The libkrunfw embedded kernel version needs to be checked for nested overlay support (Linux 5.11+). If not supported, the fallback is agent-level overlay mounting with per-container dirs (slightly less clean but functionally equivalent).

4. **Quota / size limits for COW upper layers.** Should there be a configurable limit on how much data a container can write to its COW upper layer? This could prevent a single container from filling the storage disk. Could be implemented as a separate ext4 loopback image per container, but adds complexity.
