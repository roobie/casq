---
id: casq::adr-0001-symlink-storage
description: How casq stores POSIX symbolic links in trees
tags: [adr, storage, symlinks, tree-format]
created: 2026-05-21
status: active
---

# 0001 — Symlink storage in trees

## Status

Accepted

## Context

`casq` is a content-addressed file store. Before this decision, only two
tree entry types existed — `Blob` (file) and `Tree` (directory). Symbolic
links were explicitly listed as a non-feature, and `add_directory` in
`casq_core/src/walk.rs` would either:

1. **Silently follow** them, because `entry_path.metadata()?` follows
   symlinks. The intended `is_symlink()` branch was therefore dead code
   that never fired for *valid* symlinks-to-files.
2. **Fail with `NotFound`** for *broken* symlinks, before reaching the
   explicit symlink check, with no indication that a dangling link was
   the actual cause.

Concrete trigger: running `casq put ~` died on a stale dotfile-manager
test symlink. The user-visible message was `Failed to add path:
/home/jani` with the underlying error chain dropped by the CLI's
top-level error renderer.

Symlinks are pervasive in real Unix home directories (dotfile managers,
`.cache` redirects, broken test stubs). A backup-style CAS that cannot
ingest them is severely hobbled. Pre-release status means we have a free
hand on object format.

## Decision

Add a third tree entry type, `Symlink`, and store symlink targets as
ordinary blobs.

### Tree entry

```rust
EntryType::Blob    = 1
EntryType::Tree    = 2
EntryType::Symlink = 3   // NEW
```

A `Symlink` tree entry has:
- `entry_type = Symlink`
- `mode = 0o120000` (POSIX `S_IFLNK`, matches git)
- `hash = blake3(target_bytes)` — the target string is stored as a
  regular blob in the object store
- `name = the symlink's filename within its parent directory`

The tree wire format does **not** change. Only a new value of an
existing 1-byte type discriminator.

### Target bytes, not target string

Unix paths are arbitrary byte sequences, not guaranteed UTF-8. The
symlink target is stored as the raw bytes returned by `read_link`
(captured via `OsStrExt::as_bytes` on Unix). The blob is the symlink's
content, byte-for-byte; we do not validate, normalize, or canonicalize
the target.

This matches git's symlink object representation.

### Walk semantics (`add_directory`)

Switch `entry_path.metadata()?` to `entry_path.symlink_metadata()?`,
which never follows symlinks. The three branches in `add_directory`
become:

| File type    | Action                                          |
| ------------ | ----------------------------------------------- |
| `is_file()`  | Put as blob, add `Blob` tree entry (unchanged). |
| `is_dir()`   | Recurse, add `Tree` tree entry (unchanged).     |
| `is_symlink()` | `read_link` → bytes → blob → `Symlink` entry. |

Broken symlinks are still stored — we capture the target string verbatim
without dereferencing.

### Top-level `add_path(path)` keeps following

When `path` itself is a symlink (e.g. `casq put my-link`), the existing
behavior is preserved: follow the link and store the target's content
(blob or tree). Rationale:

- There is no enclosing tree at the root, so a `Symlink` entry would
  have no parent to live in.
- This matches `tar` without `-p` (default: dereference at the root,
  preserve within).
- The user's intent in `casq put my-link` is almost always "store what
  this points at," not "store a one-character symlink as a CAS object."

### Materialize

`materialize_tree` gets a third arm:

```rust
EntryType::Symlink => {
    let target = self.get_blob(&entry.hash)?;
    std::os::unix::fs::symlink(OsStr::from_bytes(&target), &entry_path)?;
}
```

No `set_file_mode` call — symlink permissions on Linux are not
meaningful (the kernel ignores them; `lchmod` is not implemented on
most filesystems).

### Garbage collection

No change to `gc.rs`. The mark phase walks all tree entries by hash;
a `Symlink` entry's hash points at its target blob, so the blob is
marked reachable through the existing recursion. Confirmed by
inspection of `mark_object` in `casq_core/src/gc.rs`.

## Alternatives considered

### A. Inline target in the tree entry

Store the target string directly in the tree entry instead of as a
separate blob. Rejected because:

- Tree entry wire format would become variable-length and lose the
  fixed-size invariant for `mode` + `hash` + small `name`.
- Symlink targets can exceed 255 bytes (the current name length cap).
  We'd need a second length field, drifting the format.
- No deduplication of identical symlink targets across a tree.
- The `mode` field is wasted anyway (always `0o120000`); using `hash`
  as a blob pointer is the natural CAS move.

### B. Skip symlinks silently

Walk past symlinks with a warning, store nothing. Rejected because it
silently loses information from the source tree; a materialize would
not be a faithful reconstruction.

### C. Refuse to ingest trees containing symlinks

The status quo. Rejected because real Unix homes are full of symlinks,
making the tool unusable for its primary use case.

## Consequences

**Positive**
- `casq put` now succeeds on real-world directories.
- Fixes the latent bug where `metadata()` masked broken-symlink errors.
- Round-trip fidelity for backups that include symlinks.
- Identical symlink targets are deduplicated for free (CAS).
- No format break; tree wire format is unchanged (new discriminator value
  in an existing byte).

**Negative**
- One additional blob per *unique* symlink target. Small overhead given
  CAS dedup.
- Materializing a tree with a symlink targeting `/etc/passwd` (or
  containing `../` traversal) creates a working link. This is the same
  posture as `tar`/`git`; we document it rather than try to sandbox.
  Users who materialize untrusted trees should do so into a sandbox.
- Windows symlink behavior is out of scope for this change (project
  is Unix-focused; non-Unix builds will return an error if asked to
  materialize a `Symlink` entry).

## Implementation pointers

- `casq_core/src/tree.rs` — add `EntryType::Symlink`, `file_modes::SYMLINK`.
- `casq_core/src/walk.rs` — switch to `symlink_metadata()`, add symlink
  branch.
- `casq_core/src/store.rs::materialize_tree` — add `Symlink` arm.
- `casq/src/main.rs` — `list`/`metadata` rendering for symlink entries.
- `casq_core/src/gc.rs` — no change (verified).
