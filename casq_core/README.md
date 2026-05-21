# casq_core

A production-ready content-addressed file store (CAS) library with compression and chunking (v0.4.0).

## Overview

`casq_core` is a Rust library that provides the core functionality for **casq**, a content-addressed storage system with modern efficiency features. It stores files and directories by their cryptographic hash, ensuring immutable, deduplicated storage with transparent compression, content-defined chunking, and built-in garbage collection.

Think of it as a minimal git object store or restic backend, but with compression and chunking for storage efficiency.

## Features

- ✅ **Content-Addressed Storage** - Files and directories stored by BLAKE3 hash
- ✅ **Transparent Compression** - 3-5x storage reduction with zstd (files ≥ 4KB)
- ✅ **Content-Defined Chunking** - FastCDC v2020 for incremental backups (files ≥ 1MB, 60-80% chunk reuse after edits)
- ✅ **Automatic Deduplication** - Identical content stored only once (including chunk-level)
- ✅ **Tree-Based Directories** - Canonical ordering ensures stable hashes
- ✅ **Atomic Operations** - Tempfile-based writes prevent corruption
- ✅ **Garbage Collection** - Mark & sweep algorithm handles all object types
- ✅ **Corruption Detection** - Hash verification on all reads
- ✅ **Named References** - GC roots for preserving important snapshots
- ✅ **Full Round-Trip** - Add → Store → GC → Materialize
- ✅ **Cross-Platform** - Unix permissions preserved, Windows supported
- ✅ **Gitignore Support** - Respects `.gitignore` during filesystem walks

## Quick Start

```rust
use casq_core::{Store, Algorithm};
use std::path::Path;

// Initialize a new store
let store = Store::init("./my-store", Algorithm::Blake3)?;

// Add a file or directory
let hash = store.add_path(Path::new("./my-data"))?;

// Create a named reference (GC root)
store.refs().add("backup-2024", &hash)?;

// Garbage collect unreferenced objects
let stats = store.gc(false)?;
println!("Deleted {} objects, freed {} bytes",
         stats.objects_deleted, stats.bytes_freed);

// Materialize back to filesystem
store.materialize(&hash, Path::new("./restored"))?;
```

## Architecture

### Storage Format

Objects are stored with a 16-byte header followed by the payload.

**Object format:**
```
0x00  4   "CAFS" magic
0x04  1   version (u8) = 2
0x05  1   type: 1=blob, 2=tree, 3=chunk_list
0x06  1   algo: 1=blake3-256
0x07  1   compression: 0=none, 1=zstd
0x08  8   payload_len (u64 LE) - compressed size if compressed
0x10  ... payload (possibly compressed)
```

### Directory Structure

```
$STORE_ROOT/
  config               # Store configuration (version, algorithm)
  objects/
    blake3-256/        # Algorithm-specific directory
      ab/              # First 2 hex chars (shard)
        abcd...ef      # Remaining 62 hex chars (object file)
  refs/                # Named references (GC roots)
    backup-name
```

### Object Types

1. **Blob** - Raw file content (automatically compressed if ≥ 4KB, hash of uncompressed payload)
2. **Tree** - Directory structure (sorted entries by name for canonical hashing)
3. **ChunkList** - Large file metadata (files ≥ 1MB split into variable-size chunks using FastCDC v2020)

### Module Structure

```
casq_core/src/
├── lib.rs       - Public API and documentation
├── error.rs     - Error types with thiserror
├── hash.rs      - BLAKE3 hashing (32-byte digests)
├── object.rs    - Binary object encoding/decoding
├── chunking.rs  - Content-defined chunking with FastCDC v2020 (v0.4.0+)
├── store.rs     - Store management with compression/chunking
├── tree.rs      - Tree entry encoding with canonical sorting
├── walk.rs      - Filesystem traversal with gitignore
├── gc.rs        - Garbage collection (mark & sweep, handles all object types)
├── refs.rs      - Reference management
└── journal.rs   - Operation journal
```

## Building and Testing

```bash
# Build the library
cargo build --release -p casq_core

# Run all tests (121 unit tests + 23 property tests + 1 doctest)
cargo test -p casq_core

# Run only property tests
cargo test -p casq_core prop

# Run with output
cargo test -p casq_core -- --nocapture

# Check code quality
cargo clippy -p casq_core -- -D warnings

# Format code
cargo fmt -p casq_core
```

## API Overview

### Core Types

- `Store` - Main store interface
- `Hash` - 32-byte BLAKE3 hash wrapper
- `TreeEntry` - File/directory entry in a tree
- `RefManager` - Manages named references
- `GcStats` - Garbage collection statistics

### Main Operations

```rust
// Store initialization
let store = Store::init(path, Algorithm::Blake3)?;
let store = Store::open(path)?;

// Object storage
let hash = store.put_blob(reader)?;
let hash = store.put_tree(entries)?;
let hash = store.add_path(path)?;  // Recursively add file/dir

// Object retrieval
let data = store.get_blob(&hash)?;
let entries = store.get_tree(&hash)?;
store.cat_blob(&hash, writer)?;

// Materialization
store.materialize(&hash, dest_path)?;

// References
store.refs().add(name, &hash)?;
let hash = store.refs().get(name)?;
let all_refs = store.refs().list()?;
store.refs().remove(name)?;

// Garbage collection
let stats = store.gc(dry_run)?;
```

## Design Principles

1. **Content-Addressed** - Objects are immutable and identified by hash
2. **Canonical Hashing** - Tree entries sorted by name for stable hashes
3. **Transparent Optimization** - Compression and chunking automatic, invisible to API consumers
4. **Atomic Writes** - Use tempfile for corruption-free operations
5. **Simple Format** - Binary format with clear headers, human-inspectable paths
6. **Efficient Storage** - 3-5x compression typical, incremental backups via chunking
7. **Local-Only** - Single-user design, no network features

## Hashing Rules

- **Blob hash**: `hash = blake3(uncompressed_payload_bytes)` (payload only, not header)
- **ChunkList hash**: `hash = blake3(original_file_bytes)` (not the ChunkList metadata)
- **Tree hash**: Hash of canonicalized entries (sorted by name, bytewise UTF-8)
- **Object path**: `objects/<algo>/<prefix>/<suffix>` where prefix is first 2 hex chars
- **Important**: Hashes are stable regardless of compression/chunking

## Garbage Collection

- **Refs** are GC roots stored in `refs/` directory
- **Mark phase** traverses from all refs, recursively following tree entries
- **Sweep phase** deletes objects not in the reachable set
- **Dry-run mode** available for safe preview before deletion

## Test Coverage

Automated coverage reporting is available for the casq_core crate. We use cargo-tarpaulin to gather coverage and generate HTML and XML reports.

Run via mise:

```bash
# Install tarpaulin (if missing) and generate reports
mise run coverage_core

# Alternatively run directly
cargo install cargo-tarpaulin || true
cargo tarpaulin -p casq_core --out Html --out Xml
# Reports will be collected under target/coverage/casq_core/
```

The reports include per-file coverage and an XML (Cobertura) output suitable for CI dashboards. Keep coverage reports in CI as a quality gate when needed.

### Test Categories

**Unit Tests:**
- **Hash operations** - Encoding, decoding, validation
- **Object encoding** - Headers, payload, compression types
- **Chunking** - FastCDC boundaries, deterministic chunking, small files
- **Store operations** - Init, open, blob/tree/chunklist storage, compression
- **Tree operations** - Canonical ordering, nested structures
- **Filesystem walking** - Files, directories, permissions, gitignore
- **References** - CRUD operations, validation
- **Garbage collection** - Mark, sweep, dry-run, all object types including chunks
- **Materialization** - Blobs, trees, chunked files, nested structures, permissions

**Property Tests:**
- **Hash determinism** - Hashing same data always produces same result
- **Serialization round-trips** - All binary formats (headers, chunks, trees)
- **Compression identity** - Compress/decompress preserves data
- **Chunking invariants** - Size bounds, determinism, total size preservation
- **Tree canonicalization** - Order-independent hashing
- **GC correctness** - Preserves referenced, deletes unreferenced, idempotent
- **Ref validation** - Valid names accepted

## Limitations (By Design)

The following are intentionally **not supported**:

- ❌ Network operations (remote stores)
- ❌ Multi-user/concurrent access
- ❌ Encryption (planned for future)
- ❌ Parallel operations (single-threaded)
- ❌ Special file types (devices, sockets, fifos, etc.) — silently skipped during walk
- ❌ Extended attributes or ACLs beyond basic POSIX permissions

**Note**: Compression and chunking are supported in v0.4.0+. Symbolic links are
supported as of the current release (see
[ADR 0001](../docs/decisions/0001-symlink-storage.md)).

## Performance Characteristics

- **Hash algorithm**: BLAKE3 (fast, cryptographically secure)
- **Compression**: Zstd level 3 (~500 MB/s compression/decompression)
- **Chunking**: FastCDC (~1 GB/s processing)
- **I/O**: Streaming for large files (no full buffering)
- **Deduplication**: Automatic via content addressing (including chunk-level)
- **GC**: Mark & sweep with efficient hash set operations (handles all object types)
- **Directory sharding**: First 2 hex chars prevent filesystem bottlenecks

**Storage Efficiency:**
- Compression: 3-5x reduction for text files, 2-3x for mixed data
- Chunking: Change 1 byte in 1GB file → store only ~512KB (changed chunk)
- Cross-file deduplication: Shared content stored only once

## Error Handling

All operations return `Result<T, Error>` with detailed error types:

- `IoError` - File system operations
- `CorruptedObject` - Hash mismatch or invalid format
- `InvalidHash` - Malformed hash string
- `ObjectNotFound` - Missing object in store
- `InvalidStore` - Store not initialized or corrupted config
- `InvalidRef` - Bad reference name or format
- `PathExists` - Destination already exists (materialization)

## Dependencies

```toml
blake3 = "1.5"      # BLAKE3 hashing
hex = "0.4"         # Hash hex encoding/decoding
tempfile = "3.0"    # Atomic object writes
ignore = "0.4"      # Filesystem walking with .gitignore support
thiserror = "2.0"   # Error handling
zstd = "0.13"       # Transparent compression (v0.4.0+)
fastcdc = "3.1"     # Content-defined chunking (v0.4.0+)
```

## Contributing

This library is part of the **casq** project. When contributing:

1. Ensure all tests pass: `cargo test -p casq_core`
2. Maintain clippy cleanliness: `cargo clippy -p casq_core -- -D warnings`
3. Format code: `cargo fmt -p casq_core`
4. Add tests for new functionality
5. Update documentation

## License

> Apache-2.0

## See Also

- [**casq**](https://crates.io/crates/casq) - CLI binary using this library
- **NOTES.md** - Detailed design and specification
- **CLAUDE.md** - Development guidelines for AI assistants
