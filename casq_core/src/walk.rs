//! Filesystem walking and object creation.

use crate::error::{Error, Result};
use crate::hash::Hash;
use crate::journal::JournalEntry;
use crate::store::Store;
use crate::tree::{EntryType, TreeEntry, file_modes};
use std::fs;
use std::path::Path;

impl Store {
    /// Add a file or directory to the store.
    ///
    /// If the path is a file, creates a blob and returns its hash.
    /// If the path is a directory, recursively creates trees and returns the root tree hash.
    /// Records the operation in the journal.
    pub fn add_path(&self, path: &Path) -> Result<Hash> {
        if !path.exists() {
            return Err(Error::Io {
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Path does not exist: {}", path.display()),
                ),
            });
        }

        let metadata = fs::metadata(path)?;

        let hash = if metadata.is_file() {
            self.add_file(path)?
        } else if metadata.is_dir() {
            self.add_directory(path)?
        } else {
            return Err(Error::invalid_hash(format!(
                "Unsupported file type: {}",
                path.display()
            )));
        };

        // Append to journal
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Get metadata for journal
        let (entry_count, approx_size) = if metadata.is_file() {
            (1, metadata.len())
        } else {
            // For directories, get tree entry count and object size
            let tree = self.get_tree(&hash)?;
            let obj_path = self.object_path(&hash);
            let obj_size = fs::metadata(&obj_path)?.len();
            (tree.len(), obj_size)
        };

        let journal_metadata = format!("entries={},size={}", entry_count, approx_size);
        let journal_entry = JournalEntry::new(
            timestamp,
            "add".to_string(),
            hash,
            path.display().to_string(),
            journal_metadata,
        );

        self.journal().append(&journal_entry)?;

        Ok(hash)
    }

    /// Add content from stdin as a blob.
    ///
    /// Records the operation in the journal with path "(stdin)".
    /// Returns the hash of the stored blob.
    pub fn add_stdin<R: std::io::Read>(&self, reader: R) -> Result<Hash> {
        // Store blob from reader (handles compression/chunking automatically)
        let hash = self.put_blob(reader)?;

        // Query object file size after storage
        let obj_path = self.object_path(&hash);
        let obj_size = fs::metadata(&obj_path)?.len();

        // Record in journal (entries=1, always a blob)
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let journal_entry = JournalEntry::new(
            timestamp,
            "add".to_string(),
            hash,
            "(stdin)".to_string(),
            format!("entries=1,size={}", obj_size),
        );

        self.journal().append(&journal_entry)?;

        Ok(hash)
    }

    /// Add a single file as a blob.
    fn add_file(&self, path: &Path) -> Result<Hash> {
        let file = fs::File::open(path)?;
        self.put_blob(file)
    }

    /// Add a directory recursively as a tree.
    ///
    /// Uses `symlink_metadata` so symbolic links are preserved as `Symlink`
    /// tree entries rather than dereferenced. See
    /// docs/decisions/0001-symlink-storage.md.
    fn add_directory(&self, path: &Path) -> Result<Hash> {
        let mut entries = Vec::new();

        // Use ignore crate to respect .gitignore
        let walker = ignore::WalkBuilder::new(path)
            .max_depth(Some(1)) // Only immediate children
            .hidden(false) // Include hidden files
            .git_ignore(true) // Respect .gitignore
            .build();

        for entry in walker {
            let entry = entry?;
            let entry_path = entry.path();

            // Skip the directory itself
            if entry_path == path {
                continue;
            }

            // symlink_metadata: does NOT follow links — required so we can
            // distinguish symlinks from their targets and so dangling links
            // don't blow up here with NotFound.
            let metadata = entry_path.symlink_metadata()?;
            let file_name = entry_path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    Error::invalid_hash(format!("Invalid filename: {}", entry_path.display()))
                })?
                .to_string();

            let file_type = metadata.file_type();

            if file_type.is_symlink() {
                let hash = self.add_symlink(entry_path)?;
                let tree_entry =
                    TreeEntry::new(EntryType::Symlink, file_modes::SYMLINK, hash, file_name)?;
                entries.push(tree_entry);
            } else if file_type.is_file() {
                let mode = get_file_mode(&metadata);
                let hash = self.add_file(entry_path)?;
                let tree_entry = TreeEntry::new(EntryType::Blob, mode, hash, file_name)?;
                entries.push(tree_entry);
            } else if file_type.is_dir() {
                let hash = self.add_directory(entry_path)?;
                let tree_entry =
                    TreeEntry::new(EntryType::Tree, file_modes::DIRECTORY, hash, file_name)?;
                entries.push(tree_entry);
            }
            // Other types (sockets, fifos, char/block devices) are silently
            // skipped — they have no portable representation in a CAS tree.
        }

        // Create tree from entries
        self.put_tree(entries)
    }

    /// Add a symbolic link as a blob containing its target bytes.
    ///
    /// The blob payload is the raw bytes of the link target as returned by
    /// `read_link` (Unix paths are arbitrary byte sequences, not guaranteed
    /// UTF-8). Broken/dangling links are stored too — we never dereference.
    fn add_symlink(&self, path: &Path) -> Result<Hash> {
        let target: std::path::PathBuf = std::fs::read_link(path)?;
        let bytes = symlink_target_bytes(target.as_os_str());
        self.put_blob(std::io::Cursor::new(bytes))
    }
}

/// Convert an `OsStr` symlink target to the bytes we store in the blob.
///
/// On Unix paths are byte sequences; we capture them losslessly.
/// On other platforms we fall back to UTF-8 (lossy) since the materialize
/// side has no portable way to recreate non-Unicode symlinks anyway.
#[cfg(unix)]
fn symlink_target_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn symlink_target_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    s.to_string_lossy().into_owned().into_bytes()
}

/// Get the file mode (permissions) from metadata.
#[cfg(unix)]
fn get_file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let perms = metadata.permissions();
    let mode = perms.mode();

    // Check if executable
    if mode & 0o111 != 0 {
        file_modes::EXECUTABLE
    } else {
        file_modes::REGULAR
    }
}

/// Get the file mode (permissions) from metadata (Windows fallback).
#[cfg(not(unix))]
fn get_file_mode(_metadata: &fs::Metadata) -> u32 {
    // On Windows, default to regular file mode
    file_modes::REGULAR
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::Algorithm;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_add_single_file() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, b"hello world").unwrap();

        let hash = store.add_path(&test_file).unwrap();
        let expected = Hash::hash_bytes(b"hello world");
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_add_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let test_file = temp_dir.path().join("empty.txt");
        fs::write(&test_file, b"").unwrap();

        let hash = store.add_path(&test_file).unwrap();
        let expected = Hash::hash_bytes(b"");
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_add_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let test_dir = temp_dir.path().join("empty_dir");
        fs::create_dir(&test_dir).unwrap();

        let hash = store.add_path(&test_dir).unwrap();
        let tree = store.get_tree(&hash).unwrap();
        assert_eq!(tree.len(), 0);
    }

    #[test]
    fn test_add_directory_with_files() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let test_dir = temp_dir.path().join("test_dir");
        fs::create_dir(&test_dir).unwrap();
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(test_dir.join("file2.txt"), b"content2").unwrap();

        let hash = store.add_path(&test_dir).unwrap();
        let tree = store.get_tree(&hash).unwrap();

        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].name, "file1.txt");
        assert_eq!(tree[1].name, "file2.txt");
    }

    #[test]
    fn test_add_nested_directories() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let test_dir = temp_dir.path().join("parent");
        fs::create_dir(&test_dir).unwrap();
        fs::write(test_dir.join("root_file.txt"), b"root").unwrap();

        let sub_dir = test_dir.join("subdir");
        fs::create_dir(&sub_dir).unwrap();
        fs::write(sub_dir.join("sub_file.txt"), b"sub").unwrap();

        let hash = store.add_path(&test_dir).unwrap();
        let tree = store.get_tree(&hash).unwrap();

        assert_eq!(tree.len(), 2);

        // Find the subdirectory entry
        let subdir_entry = tree.iter().find(|e| e.name == "subdir").unwrap();
        assert_eq!(subdir_entry.entry_type, EntryType::Tree);

        // Verify subtree
        let subtree = store.get_tree(&subdir_entry.hash).unwrap();
        assert_eq!(subtree.len(), 1);
        assert_eq!(subtree[0].name, "sub_file.txt");
    }

    #[test]
    fn test_add_nonexistent_path() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let nonexistent = temp_dir.path().join("nonexistent");
        let result = store.add_path(&nonexistent);
        assert!(result.is_err());
    }

    #[test]
    #[cfg(unix)]
    fn test_executable_file_mode() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let test_dir = temp_dir.path().join("test_dir");
        fs::create_dir(&test_dir).unwrap();

        let script = test_dir.join("script.sh");
        fs::write(&script, b"#!/bin/bash\necho hello").unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();

        let hash = store.add_path(&test_dir).unwrap();
        let tree = store.get_tree(&hash).unwrap();

        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].mode, file_modes::EXECUTABLE);
    }

    #[test]
    fn test_add_stdin_basic() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let input = b"hello from stdin";
        let cursor = std::io::Cursor::new(input);

        let hash = store.add_stdin(cursor).unwrap();
        let expected = Hash::hash_bytes(input);
        assert_eq!(hash, expected);

        // Verify we can retrieve the content
        let blob = store.get_blob(&hash).unwrap();
        assert_eq!(blob, input);
    }

    #[test]
    fn test_add_stdin_empty() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let input = b"";
        let cursor = std::io::Cursor::new(input);

        let hash = store.add_stdin(cursor).unwrap();
        let expected = Hash::hash_bytes(input);
        assert_eq!(hash, expected);

        // Verify empty content is stored correctly
        let blob = store.get_blob(&hash).unwrap();
        assert_eq!(blob.len(), 0);
    }

    #[test]
    fn test_add_stdin_large_triggers_compression() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        // 8KB of data (exceeds 4KB compression threshold)
        let input = vec![b'A'; 8192];
        let cursor = std::io::Cursor::new(&input);

        let hash = store.add_stdin(cursor).unwrap();
        let expected = Hash::hash_bytes(&input);
        assert_eq!(hash, expected);

        // Verify content is retrievable (decompression transparent)
        let blob = store.get_blob(&hash).unwrap();
        assert_eq!(blob, input);

        // Verify compression occurred (object file should be smaller than 8KB)
        let obj_path = store.object_path(&hash);
        let obj_size = fs::metadata(&obj_path).unwrap().len();
        // Object size includes 16-byte header, so compressed payload should be much less
        assert!(
            obj_size < 8192,
            "Expected compression, got object size {}",
            obj_size
        );
    }

    #[test]
    fn test_add_stdin_very_large_triggers_chunking() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        // 2MB of data (exceeds 1MB chunking threshold)
        let input = vec![b'B'; 2 * 1024 * 1024];
        let cursor = std::io::Cursor::new(&input);

        let hash = store.add_stdin(cursor).unwrap();
        let expected = Hash::hash_bytes(&input);
        assert_eq!(hash, expected);

        // Verify content is retrievable (chunking transparent)
        let blob = store.get_blob(&hash).unwrap();
        assert_eq!(blob.len(), input.len());
        assert_eq!(blob, input);
    }

    #[test]
    #[cfg(unix)]
    fn test_add_directory_with_symlink_to_file() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let test_dir = temp_dir.path().join("d");
        fs::create_dir(&test_dir).unwrap();
        fs::write(test_dir.join("real.txt"), b"hello").unwrap();
        std::os::unix::fs::symlink("real.txt", test_dir.join("link.txt")).unwrap();

        let hash = store.add_path(&test_dir).unwrap();
        let tree = store.get_tree(&hash).unwrap();
        assert_eq!(tree.len(), 2);

        let link = tree.iter().find(|e| e.name == "link.txt").unwrap();
        assert_eq!(link.entry_type, EntryType::Symlink);
        assert_eq!(link.mode, file_modes::SYMLINK);

        // The symlink's hash points at a blob containing the target bytes.
        let target_bytes = store.get_blob(&link.hash).unwrap();
        assert_eq!(target_bytes, b"real.txt");
    }

    #[test]
    #[cfg(unix)]
    fn test_add_directory_with_broken_symlink() {
        // Dangling links must be stored verbatim, not error out.
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let test_dir = temp_dir.path().join("d");
        fs::create_dir(&test_dir).unwrap();
        std::os::unix::fs::symlink("/no/such/path/anywhere", test_dir.join("dangling")).unwrap();

        let hash = store.add_path(&test_dir).unwrap();
        let tree = store.get_tree(&hash).unwrap();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].entry_type, EntryType::Symlink);

        let target = store.get_blob(&tree[0].hash).unwrap();
        assert_eq!(target, b"/no/such/path/anywhere");
    }

    #[test]
    #[cfg(unix)]
    fn test_symlink_roundtrip_via_materialize() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let src = temp_dir.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("payload"), b"the goods").unwrap();
        std::os::unix::fs::symlink("payload", src.join("alias")).unwrap();
        std::os::unix::fs::symlink("/absolute/dangling", src.join("orphan")).unwrap();

        let hash = store.add_path(&src).unwrap();

        let restored = temp_dir.path().join("restored");
        store.materialize(&hash, &restored).unwrap();

        // alias resolves to the real file
        let alias = restored.join("alias");
        let alias_meta = alias.symlink_metadata().unwrap();
        assert!(alias_meta.file_type().is_symlink());
        assert_eq!(fs::read_link(&alias).unwrap().to_str().unwrap(), "payload");

        // orphan is recreated as a dangling link
        let orphan = restored.join("orphan");
        let orphan_meta = orphan.symlink_metadata().unwrap();
        assert!(orphan_meta.file_type().is_symlink());
        assert_eq!(
            fs::read_link(&orphan).unwrap().to_str().unwrap(),
            "/absolute/dangling"
        );
        assert!(!orphan.exists()); // target really is missing
    }

    #[test]
    #[cfg(unix)]
    fn test_symlink_targets_dedup_via_cas() {
        // Two symlinks with the same target should share a single blob.
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let test_dir = temp_dir.path().join("d");
        fs::create_dir(&test_dir).unwrap();
        std::os::unix::fs::symlink("same/target", test_dir.join("a")).unwrap();
        std::os::unix::fs::symlink("same/target", test_dir.join("b")).unwrap();

        let hash = store.add_path(&test_dir).unwrap();
        let tree = store.get_tree(&hash).unwrap();
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].hash, tree[1].hash, "identical targets must dedup");
    }

    #[test]
    fn test_add_stdin_journal_entry() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let input = b"test content";
        let cursor = std::io::Cursor::new(input);

        let hash = store.add_stdin(cursor).unwrap();

        // Verify journal entry exists
        let entries = store.journal().read_recent(10).unwrap();
        assert_eq!(entries.len(), 1);

        let entry = &entries[0];
        assert_eq!(entry.operation, "add");
        assert_eq!(entry.hash, hash);
        assert_eq!(entry.path, "(stdin)");
        assert!(entry.metadata.contains("entries=1"));
        assert!(entry.metadata.contains("size="));
    }
}
