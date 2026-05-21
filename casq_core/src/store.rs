//! Store management and object I/O.

use crate::chunking::{ChunkerConfig, chunk_file};
use crate::error::{Error, Result};
use crate::hash::{Algorithm, Hash};
use crate::journal::Journal;
use crate::object::{ChunkList, CompressionType, HEADER_SIZE, ObjectHeader, ObjectType};
use crate::refs::RefManager;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Compression threshold: files >= 4KB are compressed.
const COMPRESSION_THRESHOLD: usize = 4096;

/// Chunking threshold: files >= 1MB are chunked.
const CHUNKING_THRESHOLD: usize = 1024 * 1024;

/// A content-addressed store.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    algorithm: Algorithm,
    journal: Journal,
}

impl Store {
    /// Initialize a new store at the given path.
    ///
    /// Creates the directory structure:
    /// - `objects/blake3/` for storing objects
    /// - `refs/` for named references
    /// - `config` file with version and algorithm
    /// - `journal` file for operation tracking
    pub fn init<P: AsRef<Path>>(root: P, algorithm: Algorithm) -> Result<Self> {
        let root = root.as_ref().to_path_buf();

        // Create root directory
        fs::create_dir_all(&root)?;

        // Create objects directory with algorithm subdirectory
        let objects_dir = root.join("objects").join(algorithm.as_str());
        fs::create_dir_all(&objects_dir)?;

        // Create refs directory
        let refs_dir = root.join("refs");
        fs::create_dir_all(&refs_dir)?;

        // Write config file
        let config_path = root.join("config");
        let config_content = format!("version=1\nalgo={}\n", algorithm.as_str());
        fs::write(&config_path, config_content)?;

        // Open journal
        let journal_path = root.join("journal");
        let journal = Journal::open(&journal_path)?;

        Ok(Self {
            root,
            algorithm,
            journal,
        })
    }

    /// Open an existing store at the given path.
    ///
    /// Validates the store structure and reads the configuration.
    pub fn open<P: AsRef<Path>>(root: P) -> Result<Self> {
        let root = root.as_ref().to_path_buf();

        // Check root exists
        if !root.exists() {
            return Err(Error::invalid_store(&root, "directory does not exist"));
        }

        // Read config file
        let config_path = root.join("config");
        if !config_path.exists() {
            return Err(Error::invalid_store(&root, "config file not found"));
        }

        let config_content = fs::read_to_string(&config_path)?;
        let algorithm = Self::parse_config(&config_content)?;

        // Verify directory structure
        let objects_dir = root.join("objects").join(algorithm.as_str());
        if !objects_dir.exists() {
            return Err(Error::invalid_store(
                &root,
                "objects directory structure missing",
            ));
        }

        let refs_dir = root.join("refs");
        if !refs_dir.exists() {
            return Err(Error::invalid_store(&root, "refs directory missing"));
        }

        // Open journal (creates if doesn't exist)
        let journal_path = root.join("journal");
        let journal = Journal::open(&journal_path)?;

        Ok(Self {
            root,
            algorithm,
            journal,
        })
    }

    /// Parse the config file to extract the algorithm.
    fn parse_config(content: &str) -> Result<Algorithm> {
        let mut version = None;
        let mut algo = None;

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                match key.trim() {
                    "version" => version = Some(value.trim()),
                    "algo" => algo = Some(value.trim()),
                    _ => {}
                }
            }
        }

        // Validate version
        if version != Some("1") {
            return Err(Error::invalid_hash(format!(
                "Unsupported config version: {:?}",
                version
            )));
        }

        // Parse algorithm
        let algo_str = algo.ok_or_else(|| Error::invalid_hash("Missing algo in config"))?;
        Algorithm::parse(algo_str)
    }

    /// Get the path to an object file given its hash.
    ///
    /// Returns: `objects/{algorithm}/{prefix}/{suffix}`
    pub fn object_path(&self, hash: &Hash) -> PathBuf {
        self.root
            .join("objects")
            .join(self.algorithm.as_str())
            .join(hash.prefix())
            .join(hash.suffix())
    }

    /// Get the root directory of the store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the algorithm used by this store.
    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// Get the reference manager for this store.
    pub fn refs(&self) -> RefManager<'_> {
        RefManager::new(self)
    }

    /// Get a reference to the journal.
    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// Find journal entries for orphaned objects.
    ///
    /// Uses the GC mark phase to identify reachable objects, then returns
    /// journal entries whose hashes are not reachable.
    pub fn find_orphan_journal_entries(&self) -> Result<Vec<crate::journal::JournalEntry>> {
        // Mark phase: collect all reachable objects
        let reachable = self.mark_reachable()?;

        // Find orphaned entries in journal
        self.journal.find_orphans(&reachable)
    }

    /// Read an object header from a file.
    pub(crate) fn read_object_header(&self, path: &Path) -> Result<ObjectHeader> {
        let mut file = fs::File::open(path)?;
        let mut header_buf = [0u8; HEADER_SIZE];
        file.read_exact(&mut header_buf)?;
        ObjectHeader::decode(&header_buf)
    }

    /// Read the full payload of an object.
    pub(crate) fn read_object_payload(&self, path: &Path, expected_len: u64) -> Result<Vec<u8>> {
        let mut file = fs::File::open(path)?;

        // Skip header
        let mut header_buf = [0u8; HEADER_SIZE];
        file.read_exact(&mut header_buf)?;

        // Read payload
        let mut payload = Vec::new();
        file.read_to_end(&mut payload)?;

        if payload.len() != expected_len as usize {
            return Err(Error::corrupted_object(
                path,
                format!(
                    "Payload length mismatch: expected {}, got {}",
                    expected_len,
                    payload.len()
                ),
            ));
        }

        Ok(payload)
    }

    /// Write an object atomically using tempfile.
    fn write_object_atomic(
        &self,
        hash: &Hash,
        header: &ObjectHeader,
        payload: &[u8],
    ) -> Result<()> {
        let obj_path = self.object_path(hash);

        // Create parent directory if needed
        if let Some(parent) = obj_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write atomically using tempfile
        let temp_dir = obj_path.parent().unwrap();
        let mut temp_file = tempfile::NamedTempFile::new_in(temp_dir)?;

        // Write header and payload
        temp_file.write_all(&header.encode())?;
        temp_file.write_all(payload)?;
        temp_file.flush()?;

        // Persist atomically
        temp_file.persist(&obj_path)?;

        Ok(())
    }

    /// Store a blob from a reader.
    ///
    /// Returns the hash of the stored blob.
    /// For files >= 1MB, uses content-defined chunking.
    /// For blobs >= 4KB, applies zstd compression.
    pub fn put_blob<R: Read>(&self, mut reader: R) -> Result<Hash> {
        // Read payload
        let mut payload = Vec::new();
        reader.read_to_end(&mut payload)?;

        // Chunk large files
        if payload.len() >= CHUNKING_THRESHOLD {
            return self.put_blob_chunked(&payload);
        }

        // For smaller files, store as single blob
        self.put_blob_whole(&payload)
    }

    /// Store data as a single blob (no chunking).
    fn put_blob_whole(&self, payload: &[u8]) -> Result<Hash> {
        // Compute hash of uncompressed data
        let hash = Hash::hash_bytes(payload);

        // Check if object already exists (deduplication)
        let obj_path = self.object_path(&hash);
        if obj_path.exists() {
            return Ok(hash);
        }

        // Determine if compression should be used
        let (final_payload, compression) = if payload.len() >= COMPRESSION_THRESHOLD {
            (compress_zstd(payload)?, CompressionType::Zstd)
        } else {
            (payload.to_vec(), CompressionType::None)
        };

        // Create header
        let header = ObjectHeader::new(
            ObjectType::Blob,
            self.algorithm,
            compression,
            final_payload.len() as u64,
        );

        // Write object atomically
        self.write_object_atomic(&hash, &header, &final_payload)?;

        Ok(hash)
    }

    /// Store data as chunked blob using content-defined chunking.
    fn put_blob_chunked(&self, data: &[u8]) -> Result<Hash> {
        let config = ChunkerConfig::default();
        let chunks = chunk_file(data, &config)?;

        // Write each chunk as a blob (tracking offset as we go)
        let mut offset = 0;
        for chunk_entry in &chunks {
            let end = offset + chunk_entry.size as usize;
            let chunk_data = &data[offset..end];

            // Store chunk as blob (with compression if applicable)
            // Deduplication happens automatically in put_blob_whole
            self.put_blob_whole(chunk_data)?;

            offset = end;
        }

        // Create ChunkList object
        let chunk_list = ChunkList {
            chunks: chunks.clone(),
        };
        let chunk_list_payload = chunk_list.encode();

        // Hash is computed from original file data (for stability)
        let file_hash = Hash::hash_bytes(data);

        // Check if ChunkList already exists
        let obj_path = self.object_path(&file_hash);
        if obj_path.exists() {
            return Ok(file_hash);
        }

        // Create header for ChunkList (no compression for metadata)
        let header = ObjectHeader::new(
            ObjectType::ChunkList,
            self.algorithm,
            CompressionType::None,
            chunk_list_payload.len() as u64,
        );

        // Write ChunkList object
        self.write_object_atomic(&file_hash, &header, &chunk_list_payload)?;

        Ok(file_hash)
    }

    /// Retrieve a blob by hash.
    ///
    /// Returns the blob content as a Vec<u8>.
    /// Handles both regular blobs and chunked blobs (ChunkList objects).
    pub fn get_blob(&self, hash: &Hash) -> Result<Vec<u8>> {
        let obj_path = self.object_path(hash);

        if !obj_path.exists() {
            return Err(Error::object_not_found(hash.to_hex()));
        }

        // Read and validate header
        let header = self.read_object_header(&obj_path)?;

        match header.object_type {
            ObjectType::Blob => {
                // Read payload (compressed)
                let compressed_payload = self.read_object_payload(&obj_path, header.payload_len)?;

                // Decompress if needed
                let payload = match header.compression {
                    CompressionType::None => compressed_payload,
                    CompressionType::Zstd => decompress_zstd(&compressed_payload)?,
                };

                // Verify hash matches uncompressed data (corruption detection)
                let computed_hash = Hash::hash_bytes(&payload);
                if computed_hash != *hash {
                    return Err(Error::corrupted_object(
                        &obj_path,
                        format!(
                            "Hash mismatch: expected {}, got {}",
                            hash.to_hex(),
                            computed_hash.to_hex()
                        ),
                    ));
                }

                Ok(payload)
            }
            ObjectType::ChunkList => {
                // Read ChunkList payload
                let chunk_list_payload = self.read_object_payload(&obj_path, header.payload_len)?;
                let chunk_list = ChunkList::decode(&chunk_list_payload)?;

                // Reassemble chunks
                self.reassemble_chunks(&chunk_list, hash)
            }
            _ => Err(Error::invalid_object_type(
                "Blob or ChunkList",
                header.object_type.as_str(),
            )),
        }
    }

    /// Reassemble chunks into original data.
    fn reassemble_chunks(&self, chunk_list: &ChunkList, expected_hash: &Hash) -> Result<Vec<u8>> {
        let mut data = Vec::new();

        for chunk_entry in &chunk_list.chunks {
            // Get each chunk (this will handle decompression automatically)
            let chunk_obj_path = self.object_path(&chunk_entry.hash);
            if !chunk_obj_path.exists() {
                return Err(Error::invalid_chunk(format!(
                    "Chunk not found: {}",
                    chunk_entry.hash.to_hex()
                )));
            }

            let chunk_header = self.read_object_header(&chunk_obj_path)?;
            if chunk_header.object_type != ObjectType::Blob {
                return Err(Error::invalid_chunk(format!(
                    "Chunk has wrong type: expected Blob, got {}",
                    chunk_header.object_type.as_str()
                )));
            }

            // Read and decompress chunk
            let compressed_chunk =
                self.read_object_payload(&chunk_obj_path, chunk_header.payload_len)?;
            let chunk_data = match chunk_header.compression {
                CompressionType::None => compressed_chunk,
                CompressionType::Zstd => decompress_zstd(&compressed_chunk)?,
            };

            // Verify chunk hash
            let computed_chunk_hash = Hash::hash_bytes(&chunk_data);
            if computed_chunk_hash != chunk_entry.hash {
                return Err(Error::invalid_chunk(format!(
                    "Chunk hash mismatch: expected {}, got {}",
                    chunk_entry.hash.to_hex(),
                    computed_chunk_hash.to_hex()
                )));
            }

            data.extend_from_slice(&chunk_data);
        }

        // Verify final hash
        let computed_hash = Hash::hash_bytes(&data);
        if computed_hash != *expected_hash {
            return Err(Error::corrupted_object(
                self.object_path(expected_hash),
                format!(
                    "Reassembled data hash mismatch: expected {}, got {}",
                    expected_hash.to_hex(),
                    computed_hash.to_hex()
                ),
            ));
        }

        Ok(data)
    }

    /// Stream a blob to a writer.
    pub fn blob_to_writer<W: Write>(&self, hash: &Hash, mut writer: W) -> Result<()> {
        let payload = self.get_blob(hash)?;
        writer.write_all(&payload)?;
        Ok(())
    }

    /// Store a tree from a list of entries.
    ///
    /// Entries are automatically sorted by name for canonical ordering.
    /// Returns the hash of the stored tree.
    pub fn put_tree(&self, entries: Vec<crate::tree::TreeEntry>) -> Result<Hash> {
        use crate::tree;

        // Encode tree (this also sorts entries)
        let payload = tree::encode_tree(entries);
        let hash = Hash::hash_bytes(&payload);

        // Check if object already exists (deduplication)
        let obj_path = self.object_path(&hash);
        if obj_path.exists() {
            return Ok(hash);
        }

        // Create header (trees are not compressed - they're typically small metadata)
        let header = ObjectHeader::new(
            ObjectType::Tree,
            self.algorithm,
            CompressionType::None,
            payload.len() as u64,
        );

        // Write object atomically
        self.write_object_atomic(&hash, &header, &payload)?;

        Ok(hash)
    }

    /// Retrieve a tree by hash.
    ///
    /// Returns the list of tree entries.
    pub fn get_tree(&self, hash: &Hash) -> Result<Vec<crate::tree::TreeEntry>> {
        use crate::tree;

        let obj_path = self.object_path(hash);

        if !obj_path.exists() {
            return Err(Error::object_not_found(hash.to_hex()));
        }

        // Read and validate header
        let header = self.read_object_header(&obj_path)?;

        if header.object_type != ObjectType::Tree {
            return Err(Error::invalid_object_type(
                ObjectType::Tree.as_str(),
                header.object_type.as_str(),
            ));
        }

        // Read payload
        let payload = self.read_object_payload(&obj_path, header.payload_len)?;

        // Verify hash matches (corruption detection)
        let computed_hash = Hash::hash_bytes(&payload);
        if computed_hash != *hash {
            return Err(Error::corrupted_object(
                &obj_path,
                format!(
                    "Hash mismatch: expected {}, got {}",
                    hash.to_hex(),
                    computed_hash.to_hex()
                ),
            ));
        }

        // Decode tree
        tree::decode_tree(&payload)
    }

    /// Materialize an object (blob or tree) to the filesystem.
    ///
    /// If the object is a blob, writes it to `dest` as a file.
    /// If the object is a tree, creates `dest` as a directory and recursively materializes entries.
    pub fn materialize(&self, hash: &Hash, dest: &Path) -> Result<()> {
        // Check if dest already exists
        if dest.exists() {
            return Err(Error::path_exists(dest));
        }

        let obj_path = self.object_path(hash);
        if !obj_path.exists() {
            return Err(Error::object_not_found(hash.to_hex()));
        }

        // Read header to determine type
        let header = self.read_object_header(&obj_path)?;

        match header.object_type {
            ObjectType::Blob | ObjectType::ChunkList => self.materialize_blob(hash, dest),
            ObjectType::Tree => self.materialize_tree(hash, dest),
        }
    }

    /// Materialize a blob to a file.
    fn materialize_blob(&self, hash: &Hash, dest: &Path) -> Result<()> {
        let payload = self.get_blob(hash)?;

        // Create parent directory if needed
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(dest, payload)?;

        // TODO: Set file permissions based on mode stored in tree entry
        // This would require passing mode through, which we'll handle in integration

        Ok(())
    }

    /// Materialize a tree to a directory.
    fn materialize_tree(&self, hash: &Hash, dest: &Path) -> Result<()> {
        let entries = self.get_tree(hash)?;

        // Create the directory
        fs::create_dir_all(dest)?;

        // Materialize each entry
        for entry in entries {
            let entry_path = dest.join(&entry.name);

            match entry.entry_type {
                crate::tree::EntryType::Blob => {
                    self.materialize_blob(&entry.hash, &entry_path)?;
                    // Set file permissions
                    self.set_file_mode(&entry_path, entry.mode)?;
                }
                crate::tree::EntryType::Tree => {
                    self.materialize_tree(&entry.hash, &entry_path)?;
                }
            }
        }

        Ok(())
    }

    /// Set file mode (permissions) on a path.
    #[cfg(unix)]
    fn set_file_mode(&self, path: &Path, mode: u32) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(mode);
        fs::set_permissions(path, perms)?;
        Ok(())
    }

    /// Set file mode (Windows no-op).
    #[cfg(not(unix))]
    fn set_file_mode(&self, _path: &Path, _mode: u32) -> Result<()> {
        // Windows doesn't support POSIX permissions
        Ok(())
    }

    /// Write a blob to a writer (for cat command).
    pub fn cat_blob<W: Write>(&self, hash: &Hash, writer: W) -> Result<()> {
        self.blob_to_writer(hash, writer)
    }
}

/// Compress data using zstd.
fn compress_zstd(data: &[u8]) -> Result<Vec<u8>> {
    zstd::encode_all(data, 3) // Level 3 = fast compression
        .map_err(|e| Error::compression_error(format!("zstd compression failed: {}", e)))
}

/// Decompress data using zstd.
fn decompress_zstd(data: &[u8]) -> Result<Vec<u8>> {
    zstd::decode_all(data)
        .map_err(|e| Error::compression_error(format!("zstd decompression failed: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_store_init() {
        let temp_dir = TempDir::new().unwrap();
        let store_path = temp_dir.path().join("store");

        let store = Store::init(&store_path, Algorithm::Blake3).unwrap();
        assert_eq!(store.root(), store_path);
        assert_eq!(store.algorithm(), Algorithm::Blake3);

        // Verify directory structure
        assert!(store_path.join("objects/blake3-256").exists());
        assert!(store_path.join("refs").exists());
        assert!(store_path.join("config").exists());

        // Verify config content
        let config = fs::read_to_string(store_path.join("config")).unwrap();
        assert!(config.contains("version=1"));
        assert!(config.contains("algo=blake3-256"));
    }

    #[test]
    fn test_store_open() {
        let temp_dir = TempDir::new().unwrap();
        let store_path = temp_dir.path().join("store");

        // Init store
        Store::init(&store_path, Algorithm::Blake3).unwrap();

        // Open store
        let store = Store::open(&store_path).unwrap();
        assert_eq!(store.algorithm(), Algorithm::Blake3);
    }

    #[test]
    fn test_store_open_nonexistent() {
        let temp_dir = TempDir::new().unwrap();
        let store_path = temp_dir.path().join("nonexistent");

        let result = Store::open(&store_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_store_open_invalid_no_config() {
        let temp_dir = TempDir::new().unwrap();
        let store_path = temp_dir.path().join("store");
        fs::create_dir_all(&store_path).unwrap();

        let result = Store::open(&store_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_object_path() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        let hash = Hash::hash_bytes(b"test");
        let path = store.object_path(&hash);

        let prefix = hash.prefix();
        let suffix = hash.suffix();

        assert!(path.ends_with(format!("objects/blake3-256/{}/{}", prefix, suffix)));
    }

    #[test]
    fn test_parse_config() {
        let config = "version=1\nalgo=blake3-256\n";
        let algo = Store::parse_config(config).unwrap();
        assert_eq!(algo, Algorithm::Blake3);
    }

    #[test]
    fn test_parse_config_with_comments() {
        let config = "# Comment\nversion=1\nalgo=blake3-256\n# Another comment\n";
        let algo = Store::parse_config(config).unwrap();
        assert_eq!(algo, Algorithm::Blake3);
    }

    #[test]
    fn test_parse_config_invalid_version() {
        let config = "version=99\nalgo=blake3-256\n";
        assert!(Store::parse_config(config).is_err());
    }

    #[test]
    fn test_parse_config_missing_algo() {
        let config = "version=1\n";
        assert!(Store::parse_config(config).is_err());
    }

    #[test]
    fn test_put_blob_small() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        let data = b"hello world";
        let hash = store.put_blob(&data[..]).unwrap();

        // Verify hash is correct
        let expected_hash = Hash::hash_bytes(data);
        assert_eq!(hash, expected_hash);

        // Verify object file exists
        let obj_path = store.object_path(&hash);
        assert!(obj_path.exists());
    }

    #[test]
    fn test_get_blob() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        let data = b"test data";
        let hash = store.put_blob(&data[..]).unwrap();

        let retrieved = store.get_blob(&hash).unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn test_blob_deduplication() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        let data = b"same content";
        let hash1 = store.put_blob(&data[..]).unwrap();
        let hash2 = store.put_blob(&data[..]).unwrap();

        // Same content should produce same hash
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_get_blob_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        let hash = Hash::hash_bytes(b"nonexistent");
        let result = store.get_blob(&hash);
        assert!(result.is_err());
    }

    #[test]
    fn test_blob_to_writer() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        let data = b"stream test";
        let hash = store.put_blob(&data[..]).unwrap();

        let mut output = Vec::new();
        store.blob_to_writer(&hash, &mut output).unwrap();
        assert_eq!(output, data);
    }

    #[test]
    fn test_put_blob_large() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        // Create a large blob (1MB)
        let data = vec![0xAB; 1024 * 1024];
        let hash = store.put_blob(&data[..]).unwrap();

        let retrieved = store.get_blob(&hash).unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn test_blob_corruption_detection() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        let data = b"test";
        let hash = store.put_blob(&data[..]).unwrap();

        // Corrupt the object file
        let obj_path = store.object_path(&hash);
        let mut file_data = fs::read(&obj_path).unwrap();
        // Modify a byte in the payload (after the 16-byte header)
        if file_data.len() > 16 {
            file_data[16] ^= 0xFF;
            fs::write(&obj_path, file_data).unwrap();

            // Should detect corruption
            let result = store.get_blob(&hash);
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_put_tree_empty() {
        use crate::tree::TreeEntry;

        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        let entries: Vec<TreeEntry> = vec![];
        let hash = store.put_tree(entries).unwrap();

        // Verify object exists
        let obj_path = store.object_path(&hash);
        assert!(obj_path.exists());
    }

    #[test]
    fn test_put_get_tree() {
        use crate::tree::{EntryType, TreeEntry, file_modes};

        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        // Create some blobs
        let hash1 = Hash::hash_bytes(b"file1");
        let hash2 = Hash::hash_bytes(b"file2");

        let entries = vec![
            TreeEntry::new(
                EntryType::Blob,
                file_modes::REGULAR,
                hash1,
                "file1.txt".to_string(),
            )
            .unwrap(),
            TreeEntry::new(
                EntryType::Blob,
                file_modes::EXECUTABLE,
                hash2,
                "script.sh".to_string(),
            )
            .unwrap(),
        ];

        let tree_hash = store.put_tree(entries.clone()).unwrap();
        let retrieved = store.get_tree(&tree_hash).unwrap();

        assert_eq!(retrieved.len(), 2);
        // Entries should be sorted by name
        assert_eq!(retrieved[0].name, "file1.txt");
        assert_eq!(retrieved[1].name, "script.sh");
    }

    #[test]
    fn test_tree_canonical_ordering() {
        use crate::tree::{EntryType, TreeEntry, file_modes};

        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        let hash = Hash::hash_bytes(b"test");

        // Create entries in reverse alphabetical order
        let entries = vec![
            TreeEntry::new(
                EntryType::Blob,
                file_modes::REGULAR,
                hash,
                "z.txt".to_string(),
            )
            .unwrap(),
            TreeEntry::new(
                EntryType::Blob,
                file_modes::REGULAR,
                hash,
                "a.txt".to_string(),
            )
            .unwrap(),
            TreeEntry::new(
                EntryType::Blob,
                file_modes::REGULAR,
                hash,
                "m.txt".to_string(),
            )
            .unwrap(),
        ];

        let tree_hash = store.put_tree(entries).unwrap();
        let retrieved = store.get_tree(&tree_hash).unwrap();

        // Should be sorted
        assert_eq!(retrieved[0].name, "a.txt");
        assert_eq!(retrieved[1].name, "m.txt");
        assert_eq!(retrieved[2].name, "z.txt");

        // Same entries in different order should produce same hash
        let entries2 = vec![
            TreeEntry::new(
                EntryType::Blob,
                file_modes::REGULAR,
                hash,
                "a.txt".to_string(),
            )
            .unwrap(),
            TreeEntry::new(
                EntryType::Blob,
                file_modes::REGULAR,
                hash,
                "z.txt".to_string(),
            )
            .unwrap(),
            TreeEntry::new(
                EntryType::Blob,
                file_modes::REGULAR,
                hash,
                "m.txt".to_string(),
            )
            .unwrap(),
        ];

        let tree_hash2 = store.put_tree(entries2).unwrap();
        assert_eq!(tree_hash, tree_hash2);
    }

    #[test]
    fn test_tree_with_subtree() {
        use crate::tree::{EntryType, TreeEntry, file_modes};

        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        let file_hash = Hash::hash_bytes(b"file");
        let subtree_hash = Hash::hash_bytes(b"subtree");

        let entries = vec![
            TreeEntry::new(
                EntryType::Blob,
                file_modes::REGULAR,
                file_hash,
                "file.txt".to_string(),
            )
            .unwrap(),
            TreeEntry::new(
                EntryType::Tree,
                file_modes::DIRECTORY,
                subtree_hash,
                "subdir".to_string(),
            )
            .unwrap(),
        ];

        let tree_hash = store.put_tree(entries).unwrap();
        let retrieved = store.get_tree(&tree_hash).unwrap();

        assert_eq!(retrieved.len(), 2);
        assert_eq!(retrieved[0].entry_type, EntryType::Blob);
        assert_eq!(retrieved[1].entry_type, EntryType::Tree);
    }

    #[test]
    fn test_materialize_single_blob() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let data = b"test content";
        let hash = store.put_blob(&data[..]).unwrap();

        let dest = temp_dir.path().join("output.txt");
        store.materialize(&hash, &dest).unwrap();

        // Verify file was created with correct content
        let content = fs::read(&dest).unwrap();
        assert_eq!(content, data);
    }

    #[test]
    fn test_materialize_blob_path_exists() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        let hash = store.put_blob(b"data".as_ref()).unwrap();

        let dest = temp_dir.path().join("exists.txt");
        fs::write(&dest, b"already exists").unwrap();

        // Should error if dest exists
        let result = store.materialize(&hash, &dest);
        assert!(result.is_err());
    }

    #[test]
    fn test_materialize_tree() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        // Create a simple directory structure
        let test_dir = temp_dir.path().join("source");
        fs::create_dir(&test_dir).unwrap();
        fs::write(test_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(test_dir.join("file2.txt"), b"content2").unwrap();

        // Add to store
        let hash = store.add_path(&test_dir).unwrap();

        // Materialize to new location
        let dest = temp_dir.path().join("restored");
        store.materialize(&hash, &dest).unwrap();

        // Verify files were restored
        assert!(dest.join("file1.txt").exists());
        assert!(dest.join("file2.txt").exists());
        assert_eq!(fs::read(dest.join("file1.txt")).unwrap(), b"content1");
        assert_eq!(fs::read(dest.join("file2.txt")).unwrap(), b"content2");
    }

    #[test]
    fn test_materialize_nested_tree() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        // Create nested directory structure
        let test_dir = temp_dir.path().join("source");
        fs::create_dir(&test_dir).unwrap();
        fs::write(test_dir.join("root.txt"), b"root").unwrap();

        let subdir = test_dir.join("subdir");
        fs::create_dir(&subdir).unwrap();
        fs::write(subdir.join("nested.txt"), b"nested").unwrap();

        // Add to store
        let hash = store.add_path(&test_dir).unwrap();

        // Materialize
        let dest = temp_dir.path().join("restored");
        store.materialize(&hash, &dest).unwrap();

        // Verify structure
        assert!(dest.join("root.txt").exists());
        assert!(dest.join("subdir").is_dir());
        assert!(dest.join("subdir/nested.txt").exists());
        assert_eq!(fs::read(dest.join("subdir/nested.txt")).unwrap(), b"nested");
    }

    #[test]
    fn test_roundtrip_add_materialize() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        // Create source directory
        let source = temp_dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a.txt"), b"alpha").unwrap();
        fs::write(source.join("b.txt"), b"beta").unwrap();

        let subdir = source.join("sub");
        fs::create_dir(&subdir).unwrap();
        fs::write(subdir.join("c.txt"), b"gamma").unwrap();

        // Add to store
        let hash = store.add_path(&source).unwrap();

        // Materialize to new location
        let dest = temp_dir.path().join("dest");
        store.materialize(&hash, &dest).unwrap();

        // Verify all files match
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"alpha");
        assert_eq!(fs::read(dest.join("b.txt")).unwrap(), b"beta");
        assert_eq!(fs::read(dest.join("sub/c.txt")).unwrap(), b"gamma");
    }

    #[test]
    fn test_cat_blob() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        let data = b"cat this content";
        let hash = store.put_blob(&data[..]).unwrap();

        let mut output = Vec::new();
        store.cat_blob(&hash, &mut output).unwrap();
        assert_eq!(output, data);
    }

    #[test]
    fn test_read_object_header_too_short_and_payload_mismatch() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        // Create a file that's too short to be a valid header
        let obj_path = temp_dir.path().join("shortobj");
        std::fs::write(&obj_path, b"short").unwrap();

        // read_object_header should error
        assert!(store.read_object_header(&obj_path).is_err());

        // Now create a fake object with header + small payload
        use crate::object::CompressionType;
        use crate::object::ObjectHeader;
        use crate::object::ObjectType;

        let header = ObjectHeader::new(
            ObjectType::Blob,
            store.algorithm(),
            CompressionType::None,
            10,
        );
        let mut data = Vec::new();
        data.extend_from_slice(&header.encode());
        data.extend_from_slice(&[1, 2, 3, 4, 5]); // only 5 bytes payload
        let obj_path2 = temp_dir.path().join("payload_mismatch");
        std::fs::write(&obj_path2, &data).unwrap();

        let res = store.read_object_payload(&obj_path2, 10);
        assert!(res.is_err());
    }

    #[test]
    fn test_get_blob_chunklist_missing_and_wrong_chunk_type() {
        use crate::object::{ChunkEntry, ChunkList, CompressionType, ObjectHeader, ObjectType};

        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        // Prepare a chunk hash that does not exist
        let missing_chunk_hash = Hash::hash_bytes(b"missingchunk");
        let chunk_entry = ChunkEntry {
            hash: missing_chunk_hash,
            size: 4,
        };
        let chunk_list = ChunkList {
            chunks: vec![chunk_entry.clone()],
        };
        let chunk_list_payload = chunk_list.encode();

        // File hash (ChunkList object name)
        let file_hash = Hash::hash_bytes(b"file-with-missing-chunk");
        let header = ObjectHeader::new(
            ObjectType::ChunkList,
            store.algorithm(),
            CompressionType::None,
            chunk_list_payload.len() as u64,
        );

        // Write ChunkList object into store
        let obj_path = store.object_path(&file_hash);
        if let Some(parent) = obj_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&obj_path).unwrap();
        f.write_all(&header.encode()).unwrap();
        f.write_all(&chunk_list_payload).unwrap();
        f.flush().unwrap();

        // get_blob should fail because the referenced chunk is missing
        let res = store.get_blob(&file_hash);
        assert!(res.is_err());

        // Now create a chunk object but with wrong object type (Tree instead of Blob)
        let chunk_obj_path = store.object_path(&missing_chunk_hash);
        if let Some(parent) = chunk_obj_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let wrong_header = ObjectHeader::new(
            ObjectType::Tree,
            store.algorithm(),
            CompressionType::None,
            0,
        );
        std::fs::write(&chunk_obj_path, wrong_header.encode()).unwrap();

        // get_blob should now error with wrong chunk type
        let res2 = store.get_blob(&file_hash);
        assert!(res2.is_err());
    }

    #[test]
    fn test_journal_records_add_path() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        // Create a test file
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, b"test content").unwrap();

        // Add the file
        let hash = store.add_path(&test_file).unwrap();

        // Verify journal entry was created
        let entries = store.journal().read_recent(10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].hash, hash);
        assert_eq!(entries[0].operation, "add");
        assert!(entries[0].path.contains("test.txt"));
    }

    #[test]
    fn test_journal_find_orphans() {
        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path().join("store"), Algorithm::Blake3).unwrap();

        // Create and add first directory
        let dir1 = temp_dir.path().join("dir1");
        fs::create_dir(&dir1).unwrap();
        fs::write(dir1.join("file1.txt"), b"content1").unwrap();
        let hash1 = store.add_path(&dir1).unwrap();

        // Create and add second directory
        let dir2 = temp_dir.path().join("dir2");
        fs::create_dir(&dir2).unwrap();
        fs::write(dir2.join("file2.txt"), b"content2").unwrap();
        let hash2 = store.add_path(&dir2).unwrap();

        // Reference only the first one
        store.refs().add("saved", &hash1).unwrap();

        // Find orphaned journal entries
        let orphaned = store.find_orphan_journal_entries().unwrap();

        // Should find only the second directory
        assert_eq!(orphaned.len(), 1);
        assert_eq!(orphaned[0].hash, hash2);
    }

    #[test]
    fn test_orphan_discovery_integration() {
        use crate::tree::{EntryType, TreeEntry, file_modes};

        let temp_dir = TempDir::new().unwrap();
        let store = Store::init(temp_dir.path(), Algorithm::Blake3).unwrap();

        // Create an orphaned tree manually (without add_path)
        let blob = store.put_blob(b"orphan content".as_ref()).unwrap();
        let entries = vec![
            TreeEntry::new(
                EntryType::Blob,
                file_modes::REGULAR,
                blob,
                "orphan.txt".to_string(),
            )
            .unwrap(),
        ];
        let orphan_hash = store.put_tree(entries).unwrap();

        // Create a referenced tree
        let blob2 = store.put_blob(b"saved content".as_ref()).unwrap();
        let entries2 = vec![
            TreeEntry::new(
                EntryType::Blob,
                file_modes::REGULAR,
                blob2,
                "saved.txt".to_string(),
            )
            .unwrap(),
        ];
        let saved_hash = store.put_tree(entries2).unwrap();
        store.refs().add("saved", &saved_hash).unwrap();

        // Find orphans
        let orphans = store.find_orphan_roots().unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].hash, orphan_hash);
        assert_eq!(orphans[0].entry_count, Some(1));
    }

    // Property-based tests
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 56,
            max_shrink_iters: 1000,
            ..ProptestConfig::default()
        })]

        /// Property 18: Compression round-trip preserves data
        #[test]
        fn prop_compression_roundtrip(data in prop::collection::vec(any::<u8>(), 0..100_000)) {
            let compressed = compress_zstd(&data)?;
            let decompressed = decompress_zstd(&compressed)?;
            prop_assert_eq!(decompressed, data, "Compression must be lossless");
        }

        /// Property 19: Compression threshold is respected
        #[test]
        fn prop_compression_threshold(data in prop::collection::vec(any::<u8>(), 0..10_000)) {
            let temp_dir = TempDir::new().unwrap();
            let store = Store::init(temp_dir.path(), Algorithm::Blake3)?;

            let hash = store.put_blob(&data[..])?;
            let obj_path = store.object_path(&hash);

            // Read the object header to check compression type
            let file_data = std::fs::read(&obj_path)?;
            let header = crate::object::ObjectHeader::decode(&file_data)?;

            if data.len() < COMPRESSION_THRESHOLD {
                prop_assert_eq!(
                    header.compression,
                    crate::object::CompressionType::None,
                    "Data < 4KB should not be compressed"
                );
            } else {
                prop_assert_eq!(
                    header.compression,
                    crate::object::CompressionType::Zstd,
                    "Data >= 4KB should be compressed"
                );
            }
        }
    }
}
