---
# casq-3sjb
title: Support symlinks in tree storage
status: completed
type: feature
priority: normal
created_at: 2026-05-21T21:55:17Z
updated_at: 2026-05-21T22:05:31Z
---

Add symlink support to casq so that `casq put <dir-containing-symlinks>` succeeds and round-trips correctly via materialize.

## Background
- Currently `walk.rs:128` calls `entry_path.metadata()?` which follows symlinks; the `is_symlink()` branch at line 148 is dead code. Broken symlinks fail at the metadata() call with NotFound, masking the real reason.
- README/CLAUDE.md document symlinks as "not supported" with no rationale beyond MVP scope.
- This is a pre-release project so no backward-compat constraint.

## Design (see docs/decisions/0001-symlink-storage.md)
- New `EntryType::Symlink = 3` in tree entries.
- Symlink target stored as a regular blob (the bytes of the target path). Tree entry hash references that blob; entry type discriminator says "interpret as symlink."
- Walk uses `symlink_metadata()` (no follow); top-level `add_path(symlink)` keeps following (tar precedent).
- Materialize creates the symlink via `std::os::unix::fs::symlink`.
- No GC change needed — Symlink entries reference blob hashes via the existing tree walk.

## Acceptance
- [ ] EntryType::Symlink variant + encode/decode round-trips (unit + proptest)
- [ ] add_directory walks symlinks without erroring, stores target as blob
- [ ] materialize recreates the symlink with correct target
- [ ] Broken symlinks inside a tree are stored as-is (target string captured even if dangling)
- [ ] GC reachability includes symlink target blobs (covered by existing walk)
- [ ] list / metadata commands render symlinks distinctly
- [ ] casq put ~ (or any tree containing symlinks) succeeds
- [ ] Documentation updated: README.md (root + casq_core + casq), CLAUDE.md, ADR