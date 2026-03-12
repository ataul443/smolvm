# RFC 001: MicroVM Copy-on-Write Cloning

**Status:** Draft
**Author:** smolvm contributors
**Created:** 2026-03-12

## Summary

Add the ability to **clone a microVM** using copy-on-write semantics. A parent microVM serves as an immutable base — its overlay disk, storage disk, and host-mounted directories become read-only lower layers. A child microVM is created on top with thin COW disks that only store deltas. The host filesystem is never modified. This enables instant VM forking, safe experimentation, and branching workflows.

## Motivation

Today, every smolVM microVM is independent. If you want two VMs with similar environments (same packages installed, same tools configured, same project mounted), you must set each up from scratch. There is no way to:

1. **Fork a running VM** — take a snapshot of a configured environment and spin up a copy
2. **Branch from a base** — create multiple divergent environments from a common parent (e.g., test different dependency versions)
3. **Mount host files safely** — mount a host directory into the VM so the VM can read and "modify" files without changing the host

These are essential for:

- **AI agent workflows:** An agent configures a VM (installs tools, clones repos, sets up environment), then forks multiple copies to explore different approaches in parallel — each isolated, each disposable
- **Development branching:** Set up a base dev environment once, then `smolvm microvm copy` into throwaway VMs for each feature branch or experiment
- **Reproducible debugging:** Copy a production-like VM, make changes to debug an issue, discard the copy when done
- **Safe host mounts:** Mount `/Users/me/code` into a VM where the agent can freely modify files, build, test — without any writes reaching the host. If the result is good, explicitly sync back; if not, just delete the child VM

The key insight is that **COW belongs at the VM level, not the container level.** A microVM is the unit of environment state — its overlay disk captures installed packages, its storage disk holds cached images, and its mounts provide access to host code. Cloning the VM clones all of this in O(1) via COW disk chains.

## Design

### User-Facing Interface

#### CLI

```bash
# Create and configure a base microVM
smolvm microvm create base-dev --cpus 2 --mem 2048 --net \
    -v /Users/me/project:/workspace
smolvm microvm start base-dev
smolvm microvm exec base-dev -- apk add git nodejs npm python3
smolvm microvm exec base-dev -- npm install -g typescript
smolvm microvm stop base-dev

# Clone it — instant, O(1), COW
smolvm microvm copy base-dev feature-a
smolvm microvm copy base-dev feature-b
smolvm microvm copy base-dev experiment

# Each child has its own isolated world
smolvm microvm start feature-a
smolvm microvm exec feature-a -- sh -c 'cd /workspace && git checkout feature-a && npm test'
# /Users/me/project on the host is UNCHANGED

# Children can be cloned too (chained COW)
smolvm microvm copy feature-a feature-a-debug

# Inspect what the child changed relative to parent
smolvm microvm diff feature-a

# Sync changes back to host (explicit, opt-in)
smolvm microvm sync feature-a --mount /workspace

# Discard when done — only deletes thin delta disks
smolvm microvm delete feature-a
```

#### HTTP API

```
POST /api/v1/microvms/{name}/copy
{
  "name": "feature-a",
  "resources": { "cpus": 4, "memoryMb": 4096 }  // optional overrides
}
```

Response:
```json
{
  "name": "feature-a",
  "parent": "base-dev",
  "state": "created",
  "cow_disks": {
    "overlay": "~/.local/share/smolvm/vms/feature-a/overlay.qcow2",
    "storage": "~/.local/share/smolvm/vms/feature-a/storage.qcow2"
  }
}
```

### Architecture

#### How a MicroVM's State is Stored Today

Each microVM has two block devices attached via virtio-blk:

| Device | Purpose | Default Size | Format |
|--------|---------|-------------|--------|
| `/dev/vda` | **Storage disk** — OCI layers, container overlays, manifests | 20 GB sparse raw | ext4 |
| `/dev/vdb` | **Overlay disk** — persistent rootfs changes (packages, configs) | 10 GB sparse raw | ext4 |

On boot, the agent (`main.rs:342`) mounts `/dev/vdb` as the upper layer of an overlayfs over the initramfs (virtiofs rootfs), then `pivot_root`s into it. All system-level changes (e.g., `apk add git`) persist to `/dev/vdb`.

Host directories are mounted via virtiofs and currently bind-mounted directly — writes propagate to the host.

#### How VM Cloning Works

When you run `smolvm microvm copy parent child`:

```
Parent VM (stopped)                    Child VM (new)
─────────────────                      ──────────────

overlay.raw (ext4, 10 GB)    ──────▶  overlay.qcow2 (thin, QCOW2)
  Contains: installed packages,           backing_file = parent/overlay.raw
  configs, rootfs modifications           Contains: only child's new changes

storage.raw (ext4, 20 GB)    ──────▶  storage.qcow2 (thin, QCOW2)
  Contains: OCI layers,                  backing_file = parent/storage.raw
  container state, manifests              Contains: only child's new changes

virtiofs mounts (host dirs)   ──────▶  Same virtiofs mounts, BUT
  /Users/me/project                      agent overlays them at boot:
  Currently: direct rw bind              lowerdir = virtiofs staging
                                         upperdir = /mnt/storage/cow-mounts/smolvm0/upper
                                         → writes stay in child's storage disk
```

##### Disk Layout

```
~/.local/share/smolvm/vms/
├── base-dev/                          # Parent VM
│   ├── overlay.raw                    # 10 GB sparse ext4 (packages, configs)
│   ├── overlay.raw.formatted
│   ├── storage.raw                    # 20 GB sparse ext4 (OCI layers)
│   └── storage.raw.formatted
│
├── feature-a/                         # Child VM (COW clone)
│   ├── overlay.qcow2                  # Thin QCOW2, backs to ../base-dev/overlay.raw
│   ├── storage.qcow2                  # Thin QCOW2, backs to ../base-dev/storage.raw
│   └── cow-mounts.json               # Mount overlay metadata
│
└── feature-a-debug/                   # Grandchild (chained COW)
    ├── overlay.qcow2                  # Backs to ../feature-a/overlay.qcow2
    └── storage.qcow2                  # Backs to ../feature-a/storage.qcow2
```

##### QCOW2 Backing Chains

libkrun already supports QCOW2 via `krun_add_disk2(ctx, block_id, path, 1 /* Qcow2 */, false)` and `DiskFormat::Qcow2` exists in `src/vm/config.rs:226`. QCOW2 natively supports backing files — reads that miss the child's data fall through to the parent's image. This is the same mechanism QEMU/libvirt use for VM snapshots.

Creating a QCOW2 with a backing file:

```bash
qemu-img create -f qcow2 -b /path/to/parent/overlay.raw -F raw child/overlay.qcow2
# Or for chained children:
qemu-img create -f qcow2 -b /path/to/parent/overlay.qcow2 -F qcow2 child/overlay.qcow2
```

The child disk starts at near-zero size and grows only as data is written. Reads transparently fall through the backing chain.

##### Host Mount COW via Agent-Level Overlay

For host-mounted directories (virtiofs), QCOW2 doesn't apply (they're not block devices). Instead, the **agent applies overlayfs at boot** for child VMs:

1. Host shares `/Users/me/project` via virtiofs (unchanged)
2. Agent mounts virtiofs at `/mnt/virtiofs/smolvm0` (unchanged)
3. **For child VMs:** Instead of bind-mounting, agent creates an overlayfs:
   - `lowerdir=/mnt/virtiofs/smolvm0` (host dir, read-only)
   - `upperdir=/mnt/storage/cow-mounts/smolvm0/upper` (on storage disk)
   - `workdir=/mnt/storage/cow-mounts/smolvm0/work` (on storage disk)
4. Bind-mounts the merged view at the original target path
5. Container/process sees full host dir contents, writes go to storage disk

This is done at the **VM level** (during agent `init_volume_mounts()`), not at the container level, so every process in the VM — containers, direct exec, init scripts — all see the same COW view.

The agent knows it's a child VM via an environment variable set by the host launcher:

```
SMOLVM_COW_MOUNTS=1
```

### Implementation Plan

#### Phase 1: QCOW2 Child Disk Creation (Host Side)

**`src/storage.rs`** — Add QCOW2 creation with backing file:

```rust
/// Create a QCOW2 disk image with a backing file for COW cloning.
pub fn create_qcow2_with_backing(
    child_path: &Path,
    backing_path: &Path,
    backing_format: DiskFormat,  // Raw or Qcow2
) -> Result<()> {
    let backing_fmt = match backing_format {
        DiskFormat::Raw => "raw",
        DiskFormat::Qcow2 => "qcow2",
    };

    let output = std::process::Command::new("qemu-img")
        .args([
            "create",
            "-f", "qcow2",
            "-b", &backing_path.to_string_lossy(),
            "-F", backing_fmt,
            &child_path.to_string_lossy(),
        ])
        .output()
        .map_err(|e| Error::storage("create qcow2", e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::storage("create qcow2 with backing", stderr.to_string()));
    }

    Ok(())
}
```

**Dependency:** `qemu-img` must be available on the host. On macOS: `brew install qemu`. On Linux: `apt install qemu-utils`. This is a build/dev dependency only — the created QCOW2 files are consumed by libkrun which has native QCOW2 support.

#### Phase 2: VM Record Lineage (Config)

**`src/config.rs`** — Extend `VmRecord` with parent tracking:

```rust
pub struct VmRecord {
    // ... existing fields ...

    /// Parent VM name (if this is a COW clone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,

    /// Disk format for this VM's disks.
    #[serde(default)]
    pub disk_format: DiskFormat,  // Raw for base VMs, Qcow2 for clones

    /// Whether host mounts should be overlaid (COW) instead of direct bind.
    #[serde(default)]
    pub cow_mounts: bool,
}
```

#### Phase 3: Copy Command (CLI + Logic)

**`src/cli/microvm.rs`** — Add `Copy` subcommand:

```rust
/// Copy a microVM to create a new COW clone.
///
/// The child VM shares the parent's disk state via QCOW2 backing files.
/// Only new writes consume disk space. Host mounts are overlaid so
/// writes stay in the child VM.
///
/// Examples:
///   smolvm microvm copy base-dev feature-a
///   smolvm microvm copy base-dev experiment --cpus 4 --mem 4096
#[derive(Args, Debug)]
pub struct CopyCmd {
    /// Source microVM to copy from
    #[arg(value_name = "SOURCE")]
    pub source: String,

    /// Name for the new microVM
    #[arg(value_name = "NAME")]
    pub name: String,

    /// Override CPU count (default: inherit from parent)
    #[arg(long, value_name = "N")]
    pub cpus: Option<u8>,

    /// Override memory in MiB (default: inherit from parent)
    #[arg(long, value_name = "MiB")]
    pub mem: Option<u32>,
}
```

**`src/cli/vm_common.rs`** — Add `copy_vm()`:

```rust
pub fn copy_vm(kind: VmKind, source: &str, name: &str, overrides: CopyOverrides) -> Result<()> {
    let mut config = SmolvmConfig::load()?;

    // 1. Validate source exists and is stopped
    let parent = config.get_vm(source)
        .ok_or_else(|| Error::vm_not_found(source))?;
    if parent.actual_state() == RecordState::Running {
        return Err(Error::invalid_state("parent VM must be stopped before copying"));
    }

    // 2. Validate target doesn't exist
    if config.get_vm(name).is_some() {
        return Err(Error::vm_creation(format!("VM '{}' already exists", name)));
    }

    // 3. Determine parent disk format and paths
    let parent_dir = vm_data_dir(source);
    let child_dir = vm_data_dir(name);
    std::fs::create_dir_all(&child_dir)?;

    let parent_overlay = parent_dir.join(if parent.disk_format == DiskFormat::Qcow2 {
        "overlay.qcow2"
    } else {
        OVERLAY_DISK_FILENAME  // "overlay.raw"
    });
    let parent_storage = parent_dir.join(if parent.disk_format == DiskFormat::Qcow2 {
        "storage.qcow2"
    } else {
        STORAGE_DISK_FILENAME  // "storage.raw"
    });

    // 4. Create QCOW2 children with backing files
    create_qcow2_with_backing(
        &child_dir.join("overlay.qcow2"),
        &parent_overlay,
        parent.disk_format,
    )?;
    create_qcow2_with_backing(
        &child_dir.join("storage.qcow2"),
        &parent_storage,
        parent.disk_format,
    )?;

    // 5. Create child VmRecord
    let child_record = VmRecord {
        name: name.to_string(),
        parent: Some(source.to_string()),
        disk_format: DiskFormat::Qcow2,
        cow_mounts: true,
        // Inherit from parent, with optional overrides
        cpus: overrides.cpus.unwrap_or(parent.cpus),
        mem: overrides.mem.unwrap_or(parent.mem),
        mounts: parent.mounts.clone(),
        ports: parent.ports.clone(),
        network: parent.network,
        ..VmRecord::default_from_name(name)
    };

    config.insert_vm(name.to_string(), child_record)?;

    println!("Created '{}' (clone of '{}')", name, source);
    println!("  overlay: {}/overlay.qcow2", child_dir.display());
    println!("  storage: {}/storage.qcow2", child_dir.display());
    Ok(())
}
```

#### Phase 4: Launch Child VMs with QCOW2 Disks

**`src/agent/launcher.rs`** — When launching a child VM, use QCOW2 format:

The existing `launch_agent_vm()` already attaches overlay and storage disks as virtio-blk devices. The change is:

```rust
// Before (always raw):
let overlay_disk = DiskConfig::new("overlay", overlay_path).format(DiskFormat::Raw);

// After (format-aware):
let overlay_disk = DiskConfig::new("overlay", overlay_path).format(record.disk_format);
let storage_disk = DiskConfig::new("storage", storage_path).format(record.disk_format);
```

libkrun handles QCOW2 natively via `krun_add_disk2(ctx, id, path, 1 /* Qcow2 */, false)`.

#### Phase 5: Agent-Level Mount COW

**`crates/smolvm-agent/src/main.rs`** — Update `init_volume_mounts()` path:

When `SMOLVM_COW_MOUNTS=1` is set (passed via env by host launcher for child VMs):

```rust
fn init_volume_mounts_cow(mounts: Vec<(String, String, bool)>) {
    for (tag, guest_path, _read_only) in &mounts {
        // 1. Mount virtiofs at staging (same as today)
        let virtiofs_mount = Path::new(paths::VIRTIOFS_MOUNT_ROOT).join(tag);
        mount_virtiofs(&tag, &virtiofs_mount);

        // 2. Create overlay dirs on storage disk
        let cow_dir = Path::new("/mnt/storage/cow-mounts").join(tag);
        let upper = cow_dir.join("upper");
        let work = cow_dir.join("work");
        let merged = cow_dir.join("merged");
        std::fs::create_dir_all(&upper).ok();
        std::fs::create_dir_all(&work).ok();
        std::fs::create_dir_all(&merged).ok();

        // 3. Mount overlayfs
        let opts = format!(
            "lowerdir={},upperdir={},workdir={}",
            virtiofs_mount.display(), upper.display(), work.display()
        );
        // mount -t overlay overlay -o $opts $merged
        mount_overlay(&opts, &merged);

        // 4. Bind-mount merged view at the guest target path
        bind_mount(&merged, Path::new(guest_path));
    }
}
```

This happens at boot, before the vsock listener starts, so all subsequent operations (container runs, exec, etc.) see the COW view of host directories.

#### Phase 6: Parent Protection

When a parent VM has children, its disks are part of a QCOW2 backing chain and **must not be modified**. Protections:

**`src/cli/vm_common.rs`** — Prevent starting a parent that has children:

```rust
pub fn start_vm_named(kind: VmKind, name: &str) -> Result<()> {
    let config = SmolvmConfig::load()?;

    // Check if this VM is a parent of any other VM
    let has_children = config.list_vms()
        .any(|(_, record)| record.parent.as_deref() == Some(name));

    if has_children {
        return Err(Error::invalid_state(
            format!("VM '{}' has child clones — starting it would corrupt their backing chain. \
                     Delete children first, or create a new copy to work from.", name)
        ));
    }

    // ... existing start logic
}
```

**`src/cli/vm_common.rs`** — Prevent deleting a parent with children:

```rust
pub fn delete_vm(kind: VmKind, name: &str, force: bool, opts: DeleteVmOptions) -> Result<()> {
    let config = SmolvmConfig::load()?;

    let children: Vec<_> = config.list_vms()
        .filter(|(_, r)| r.parent.as_deref() == Some(name))
        .map(|(n, _)| n.clone())
        .collect();

    if !children.is_empty() {
        return Err(Error::invalid_state(
            format!("VM '{}' is parent of: {}. Delete children first.",
                name, children.join(", "))
        ));
    }

    // ... existing delete logic
}
```

### Diff and Sync Commands (Future Phase)

#### `smolvm microvm diff`

Shows what the child VM changed relative to its parent. For QCOW2 disks, this can be done by mounting the child's overlay disk and listing the upper layer. For host mount COW, list files in the `cow-mounts/<tag>/upper/` directory.

```bash
$ smolvm microvm diff feature-a
Modified files (overlay disk):
  /usr/lib/node_modules/typescript/...  (apk add)
  /etc/apk/world                        (package list)

Modified files (mount /workspace):
  /workspace/src/index.ts               (modified)
  /workspace/package-lock.json          (modified)
  /workspace/node_modules/              (new directory)
```

#### `smolvm microvm sync`

Explicitly copies changes from the child's COW mount back to the host:

```bash
$ smolvm microvm sync feature-a --mount /workspace
Syncing /workspace changes to /Users/me/project...
  Modified: src/index.ts
  Modified: package-lock.json
  New: node_modules/ (skipped, in .gitignore)
Synced 2 files.
```

This uses the `upperdir` contents — only files that were actually written by the child.

### Constraints and Limitations

1. **Parent must be stopped during copy.** QCOW2 backing files must be consistent. If the parent's ext4 filesystem has uncommitted journal entries, the child would see a corrupt filesystem. Stopping the parent ensures a clean state. (Future: support live snapshots by flushing the filesystem first.)

2. **Parent is frozen after copy.** Starting a parent would modify its disks, invalidating children's backing references. The parent effectively becomes a read-only "image" once it has children. This is the standard QCOW2 backing chain contract.

3. **Chain depth.** Deep QCOW2 chains (A → B → C → D → ...) add read latency as each miss traverses the chain. Recommended limit: 5-10 levels. A future `smolvm microvm flatten` command could collapse the chain by merging layers.

4. **`qemu-img` dependency.** Required on the host for creating QCOW2 images. libkrun reads QCOW2 natively, but doesn't create them. This is a standard tool available via `brew install qemu` / `apt install qemu-utils`.

5. **Disk space accounting.** Child QCOW2 disks are thin — they only consume space for written blocks. But the parent's raw disks still consume their full sparse-allocated size. `smolvm microvm ls` should show both the child's actual usage and the backing chain's total.

6. **Host mount COW is at the VM level.** All processes inside the child VM see the same overlaid view of host mounts. This is intentional — the VM is the isolation boundary, not individual containers within it.

7. **inotify on COW mounts.** Host-side file changes to the virtiofs lower layer are visible on read (overlayfs defers to lower layer on cache miss), but inotify events from the host do not propagate through the overlay.

### Alternatives Considered

#### 1. Container-level COW mounts via OCI spec

Add `"type": "overlay"` to the OCI config.json so crun mounts an overlay per container.

**Rejected because:**
- COW at the container level doesn't capture VM-level state (installed packages, system configs)
- Doesn't support the "copy a whole environment" use case
- Multiple containers in the same VM would have different views of the same mount
- Doesn't compose with `smolvm microvm exec` (which runs outside containers)

#### 2. Raw disk copy + overlayfs (no QCOW2)

Copy parent disks as full raw images and use overlayfs inside the guest.

**Rejected because:**
- O(n) copy time and disk usage (a 20 GB storage disk requires a 20 GB copy)
- QCOW2 backing chains provide O(1) creation and minimal space usage
- libkrun already supports QCOW2 natively

#### 3. Btrfs/ZFS snapshots

Use filesystem-level snapshots for the disk images.

**Rejected because:**
- Requires specific host filesystem (Btrfs/ZFS) — not available on APFS (macOS) or standard ext4 (Linux)
- QCOW2 is filesystem-agnostic and works everywhere

#### 4. LVM thin provisioning

Use LVM thin volumes for COW disk cloning.

**Rejected because:**
- Requires root/LVM setup on the host
- Overkill for development use cases
- Not available on macOS

### Testing Strategy

#### Unit Tests

- `storage.rs`: Test `create_qcow2_with_backing()` creates valid QCOW2 files
- `config.rs`: Test `VmRecord` with parent/disk_format serialization roundtrip
- `config.rs`: Test parent protection (has_children check)

#### Integration Tests (`tests/test_microvm.sh`)

```bash
# Test basic copy
smolvm microvm create parent --cpus 1 --mem 512
smolvm microvm start parent
smolvm microvm exec parent -- sh -c 'echo "hello" > /tmp/parent-file'
smolvm microvm stop parent

smolvm microvm copy parent child
smolvm microvm start child
smolvm microvm exec child -- cat /tmp/parent-file  # Should print "hello"
smolvm microvm exec child -- sh -c 'echo "modified" > /tmp/parent-file'
smolvm microvm stop child

# Parent's file should be unchanged (QCOW2 COW)
smolvm microvm start parent  # ERROR: has children
smolvm microvm delete child
smolvm microvm start parent
smolvm microvm exec parent -- cat /tmp/parent-file  # Should print "hello"
smolvm microvm stop parent

# Test host mount COW
mkdir -p /tmp/test-cow && echo "original" > /tmp/test-cow/file.txt
smolvm microvm create base -v /tmp/test-cow:/workspace
smolvm microvm start base && smolvm microvm stop base
smolvm microvm copy base child-cow
smolvm microvm start child-cow
smolvm microvm exec child-cow -- sh -c 'echo "changed" > /workspace/file.txt'
smolvm microvm stop child-cow
# Host file must be unchanged
[ "$(cat /tmp/test-cow/file.txt)" = "original" ] || echo "FAIL"
```

## Unresolved Questions

1. **Live copy (without stopping parent).** Could flush the guest filesystem via `sync` + `fsfreeze` over vsock before snapshotting. This is more complex but would enable forking running VMs. Defer to a follow-up RFC.

2. **`smolvm microvm flatten` to collapse QCOW2 chains.** When a chain gets deep, `qemu-img commit` or `qemu-img rebase` can merge layers. Should this be manual or automatic?

3. **Should `smolvm microvm copy` work on running VMs with a warning?** Some users may prefer convenience over perfect consistency. A `--force` flag could skip the stopped-state check with a warning about potential filesystem inconsistency.

4. **Quota limits per child.** Should child QCOW2 disks have a configurable maximum size to prevent unbounded growth? QCOW2 supports preallocation limits but not hard caps natively. Could be enforced by monitoring file size.

5. **How to handle `smolvm microvm sync` for non-mount changes.** Syncing host mount COW back is straightforward (copy upperdir). But should we also support extracting rootfs changes (installed packages) from the overlay disk? This would enable "exporting" a configured environment.
