# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## General rules

_ _**CRITICAL**: Documentation in all forms (files in repo, memories etc) MUST be kept updated and current as part of ANY code change. Documentation updates are NOT optional and are NOT deferred - they are completed in the SAME work session as the code implementation.

- Always prefer Serena MCP tools.

- `casq` has not yet had a stable release, so backwards compatibility is not a concern.

### Documentation Update Checklist

When implementing changes, you MUST update the relevant documentation files as part of the same work session:

1. **For ALL changes:**
   - Update `/workspace/CLAUDE.md` if project structure, architecture, or development guidelines change

2. **For new features or API changes:**
   - Update `/workspace/README.md` (main project overview)
   - Update `/workspace/casq_core/README.md` (if core library changes)
   - Update `/workspace/casq/README.md` (if CLI changes)

3. **For new tests or test infrastructure:**
   - Update `/workspace/tests/README.md` (test suite documentation)

### What Requires Documentation Updates

- ✅ **New features** - Add to feature lists in all READMEs
- ✅ **API changes** - Update API examples and signatures
- ✅ **New dependencies** - Update dependency lists in all READMEs
- ✅ **Architecture changes** - Update CLAUDE.md, NOTES.md, and relevant READMEs
- ✅ **New commands** - Update CLI documentation with examples
- ✅ **Performance characteristics** - Update performance sections
- ✅ **Object format changes** - Update format specifications
- ✅ **Test coverage changes** - Update test count and coverage sections
- ✅ **Limitations removed** - Update limitations sections
- ✅ **New modules** - Update module structure documentation

### Documentation Update Process

**DO:**
- ✅ Update documentation in the SAME session as code implementation
- ✅ Check ALL relevant README files for needed updates
- ✅ Verify version numbers, test counts, and statistics are current
- ✅ Update examples to use new features
- ✅ Remove outdated limitations when features are implemented
- ✅ Keep beans updated

### Example: Feature Implementation Flow

```
1. Implement feature code
2. Write/update tests
3. Verify tests pass
4. Update /workspace/README.md (if applicable)
5. Update /workspace/casq_core/README.md (if library changes)
6. Update /workspace/casq/README.md (if CLI changes)
7. Update /workspace/CLAUDE.md (if architecture/process changes)
8. Update /workspace/TESTING.md (if test infrastructure changes)
9. Verify all documentation is consistent
10. THEN mark work as complete
```

### Verification Before Completion

Before considering any work complete, verify:
- [ ] All relevant README files reviewed and updated
- [ ] Version numbers updated if applicable
- [ ] Test counts updated to match reality
- [ ] New features added to feature lists
- [ ] Examples updated to use new capabilities
- [ ] Removed limitations that are now implemented
- [ ] Architecture diagrams/descriptions updated if structure changed
- [ ] Dependencies lists current
- [ ] No references to "TODO" or "future work" for completed features

## Project Overview

**casq** is a content-addressed file store (CAS) - a single-binary system for storing and retrieving files and directories by their cryptographic hash. Think "minimal git object store / restic backend" but with modern compression and chunking.

Key characteristics:
- Local-only, single-user, no network support
- Content-addressed storage: files/dirs stored by hash
- Immutable objects with stable content IDs
- Tree-based directory representation
- Garbage collection for unreferenced objects
- Transparent zstd compression (files ≥ 4KB automatically compressed)
- Content-defined chunking (files ≥ 1MB split into variable chunks for incremental backups)

## Project Structure

This is a **Rust workspace** with two crates:

- `casq_core/`: Core library implementing the storage engine, hashing, compression, chunking, and object management
- `casq/`: CLI binary that provides the user interface

**Status:** Alpha level with comprehensive test coverage.

## Build and Development Commands

This project uses **mise** for task automation. All development tools and tasks are defined in `mise.toml`. Query that file directly to see which tasks are available. A new coverage task has been added to the mise tasks: `coverage_core` generates HTML and XML coverage reports for the `casq_core` crate (requires `cargo-tarpaulin` and writes reports to `target/coverage/casq_core/`).

### Direct Cargo Commands

You can also use cargo and uv directly when needed:

```bash
# Run tests for specific crate
cargo test -p casq_core
cargo test -- --nocapture

# run all python tests
uv run pytest tests/
```

## Architecture

### Storage Model

**Objects directory structure:**
```
$STORE_ROOT/
  config               # Store configuration (algorithm, version)
  journal              # Operation log (timestamp|operation|hash|path|metadata)
  objects/
    blake3-256/        # BLAKE3 hashing
      ab/
        abcd...        # Object files named by hash (may be compressed)
  refs/                # Named references to root hashes
    backup-name
```

**Object types:**
1. **Blob**: File content (automatically compressed if ≥ 4KB)
2. **Tree**: Directory structure (list of entries with mode, type, hash, name)
3. **ChunkList**: Large file split into chunks (files ≥ 1MB, enables incremental backups)

### On-Disk Format

**Object file header (16 bytes):**
```
0x00  4   "CAFS" magic
0x04  1   version (u8) = 2
0x05  1   type: 1=blob, 2=tree, 3=chunk_list
0x06  1   algo: 1=blake3-256
0x07  1   compression: 0=none, 1=zstd
0x08  8   payload_len (u64 LE) - compressed size if compressed
0x10  ... payload (possibly compressed)
```

**Tree entry format:**
```
0     1    type (u8): 1=blob, 2=tree, 3=symlink
1     4    mode (u32 LE, POSIX)
5     32   hash (raw 32-byte digest)
37    1    name_len (u8)
38    N    name bytes (UTF-8)
```

Note: tree-entry type byte and object-header type byte share values 1 and 2
(Blob, Tree) but diverge at 3. Object header 3 = ChunkList. Tree entry 3 =
Symlink (the hash points to a Blob object whose payload is the symlink's
target bytes). See docs/decisions/0001-symlink-storage.md.

**ChunkList entry format (40 bytes per chunk):**
```
0     32   chunk_hash (BLAKE3 of chunk content)
32    8    chunk_size (u64 LE)
```

### CLI Commands

See [./casq/CLI.md](./casq/CLI.md)

### CLI Output Conventions

**stdout/stderr Separation:**
- **Text mode**:
  - stdout: Command results and data in a intuituve format (hashes, file content, listings)
  - stderr: Informational messages, notification, confirmations, errors, empty state messages
- **JSON mode (`--json`)**: ONLY JSON goes to stdout. All informational messages, errors, and warnings go to stderr.

**Implementation:**
- Use `OutputWriter` abstraction (`output.write()`, `output.write_info()`, `output.write_error()`)
- Tests should strictly validate stdout/stderr separation (no lenient "or" assertions)

### Stdin Support

**casq** supports reading content directly from stdin using the `-` argument:

```bash
# Example: pipe curl output
curl https://example.org | casq put --ref-name example-dot-org@20260123 -
af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262
```

### JSON Output

**casq** supports machine-readable JSON output via the `--json` global flag, enabling scripting and automation.

**Usage:**
```bash
# All commands support --json
casq --json initialize
casq --json put myfile.txt
casq --json list
casq --json collect-garbage --dry-run

# Pipe through jq for processing
casq --json list | jq '.refs[].name'
casq --json put file.txt | jq '.object.hash'
```

**Standard Response Format:**

All JSON responses include:
- `success: bool` - Whether the operation succeeded
- `result_code: u8` - Exit code (0 for success, non-zero for errors)
- Command-specific fields (hashes, counts, paths, etc.)

**Example Outputs:**

```json
// init
{"success":true,"result_code":0,"root":"./casq-store","algorithm":"blake3-256"}

// add with reference
{"success":true,"result_code":0,"objects":[{"hash":"abc123...","path":"file.txt"}],"reference":{"name":"backup","hash":"abc123..."}}

// ls (refs)
{"success":true,"result_code":0,"type":"RefList","refs":[{"name":"backup","hash":"abc123..."}]}

// gc
{"success":true,"result_code":0,"dry_run":false,"objects_deleted":42,"bytes_freed":1048576}

// Error (stderr)
{"success":false,"result_code":1,"error":"Object not found: abc123..."}
```

**Implementation:**
- Output abstraction layer in `casq/src/output.rs`
- DTOs (Data Transfer Objects) for all command responses
- Serde serialization with custom `Hash` serializer (outputs hex string)
- Information, notifications and error messages on stderr in JSON mode
- Exit codes match `result_code` in JSON

**Binary Data Limitation:**
- `cat` command errors in JSON mode
- Use `materialize` or `metadata` as alternatives

**Testing:**
- All tests in `tests/`

## Design Principles

1. **Content-addressed**: Objects are immutable and identified by hash
2. **Simple format**: Binary format with clear headers, human-inspectable
3. **Canonical hashing**: Tree entries sorted by name for stable hashes
4. **Garbage collection**: Unreferenced objects removed via reachability analysis from refs
5. **Transparent optimization**: Compression and chunking automatic, invisible to API consumers
7. **Efficient storage**: 3-5x compression for typical data, incremental backups via chunking
8. **Local-only**: No network features, encryption, or multi-user support (by design)

### Implementation Workflow

When implementing new features:

1. **Planning Phase:**
   - Identify which documentation files will need updates
   - Store plan as a bean task

2. **Implementation Phase:**
   - Write code
   - Write/update tests
   - Run tests to verify

3. **Documentation Phase (REQUIRED BEFORE COMPLETION):**
   - Update all identified documentation files
   - Verify consistency across all documentation
   - Check for outdated references
   - Update version numbers and statistics

4. **Verification Phase:**
   - Run full test suite
   - Review all documentation changes
   - Mark work as complete only after documentation is updated

**Remember: Documentation is not optional. Code without documentation updates is incomplete work.**

### Hashing Rules
- **Blob hash**: `hash = blake3(uncompressed_payload_bytes)` (payload only, not header)
- **ChunkList hash**: `hash = blake3(original_file_bytes)` (not the ChunkList metadata)
- **Tree hash**: Hash of canonicalized entries (sorted by name)
- Object file path: `objects/<algo>/<first2hexchars>/<remaining60hexchars>`
- **Important**: Hashes are stable regardless of compression/chunking

### Tree Canonicalization
1. Collect all entries from filesystem
2. Sort by name (bytewise UTF-8)
3. Serialize according to tree entry format
4. Hash the serialized payload

### Refs and GC
- Refs stored as text files in `refs/` directory
- Each ref file contains hex hash (one per line, last non-empty line is current)
- GC walks from all ref roots, marks reachable objects (including chunks referenced by ChunkLists)
- Unreferenced objects deleted during sweep phase

## Current Implementation Status

### Core Features

**Storage Engine:**
- ✅ Content-addressed storage with BLAKE3 hashing
- ✅ Three object types: Blob, Tree, ChunkList
- ✅ Transparent zstd compression (files ≥ 4KB)
- ✅ Content-defined chunking with FastCDC (files ≥ 1MB)
- ✅ Atomic object writes with tempfile
- ✅ Hash-based deduplication (including chunk-level deduplication)
- ✅ Symbolic link support — preserved within trees, dereferenced at top-level
  `casq put` (see docs/decisions/0001-symlink-storage.md)

**Object Format:**
- ✅ v2 format with compression support

**Commands:**
- ✅ `initialize` - Initialize new store
- ✅ `put` - Add files/directories to store (supports stdin with `-`)
- ✅ `materialize` - Restore files from store
- ✅ `get` - Output blob content to stdout
- ✅ `list` - List tree contents
- ✅ `metadata` - Show object metadata
- ✅ `collect-garbage` - Garbage collection (handles all three object types)
- ✅ `find-orphans` - Find unreferenced objects (blobs and trees)
- ✅ `references` - Manage named references
- ✅ `--json` - JSON output for all commands (v0.6.0+)
- ✅ **Stdout/stderr separation** - Only command data should go to stdout, whereas informational output shall go to stderr.

**Module Organization:**
- `casq_core/src/lib.rs` - Library exports
- `casq_core/src/hash.rs` - BLAKE3 hashing (with Serialize support)
- `casq_core/src/object.rs` - Object types and encoding (~350 lines)
- `casq_core/src/store.rs` - Storage engine with compression/chunking (~500 lines)
- `casq_core/src/chunking.rs` - FastCDC v2020 integration (~150 lines)
- `casq_core/src/tree.rs` - Tree utilities
- `casq_core/src/gc.rs` - Garbage collection (with Serialize support)
- `casq_core/src/walk.rs` - Filesystem walking
- `casq_core/src/journal.rs` - Operation journal
- `casq_core/src/error.rs` - Error types
- `casq/src/main.rs` - CLI implementation
- `casq/src/output.rs` - JSON output abstraction and DTOs (~300 lines)

### Known Limitations (By Design)

- No encryption at rest
- No parallel operations (single-threaded)
- No remote backends (local-only by design)
- No object caching (direct disk I/O)
- No snapshot abstractions (use references)
- Special file types (devices, sockets, fifos) are silently skipped during
  tree walks. Symbolic links *are* supported (ADR 0001).

### Storage Performance

**Typical compression ratios:**
- Text files: 3-5x reduction
- Binary data: 2-3x reduction
- Already compressed (images, video): minimal overhead (~2% header)

**Incremental backup efficiency:**
- Before chunking: 1GB file + 1 byte change = 2GB stored
- After chunking: 1GB file + 1 byte change = 1GB + ~512KB stored (one chunk)

**Chunk deduplication:**
- Shared content across files stored only once
- Example: 10 files with identical 5MB section = 5MB stored (not 50MB)

---

## IMPORTANT: Keeping This Document Current

For any change to this repository:

1. **Immediately update this CLAUDE.md file** with any architectural, structural, or procedural changes
2. **Update the "Current Implementation Status" section** when features are added or test counts change
3. **Update "Known Limitations"** when limitations are removed or new ones are identified
4. **Update "Module Organization"** when files are added, removed, or significantly restructured
5. **Update examples and documentation** to reflect new capabilities

**This document is a living guide - it must evolve with the codebase. Stale documentation is worse than no documentation.**
