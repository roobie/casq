//! Tree encoding and directory structure.

use crate::error::{Error, Result};
use crate::hash::Hash;
use std::io::Read;

/// Entry type in a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    /// A blob (file).
    Blob = 1,
    /// A subtree (directory).
    Tree = 2,
    /// A symbolic link. Hash references a blob containing the link target bytes.
    /// See docs/decisions/0001-symlink-storage.md.
    Symlink = 3,
}

impl EntryType {
    /// Convert to byte representation.
    pub fn to_u8(self) -> u8 {
        self as u8
    }

    /// Parse from byte representation.
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            1 => Ok(EntryType::Blob),
            2 => Ok(EntryType::Tree),
            3 => Ok(EntryType::Symlink),
            _ => Err(Error::invalid_tree_entry(format!(
                "Invalid entry type: {}",
                value
            ))),
        }
    }
}

/// File mode (POSIX permissions).
pub type FileMode = u32;

/// Common file modes.
pub mod file_modes {
    use super::FileMode;

    /// Regular file (non-executable).
    pub const REGULAR: FileMode = 0o100644;

    /// Executable file.
    pub const EXECUTABLE: FileMode = 0o100755;

    /// Directory.
    pub const DIRECTORY: FileMode = 0o040755;

    /// Symbolic link (POSIX S_IFLNK | 0o777). Matches git's symlink mode.
    pub const SYMLINK: FileMode = 0o120777;
}

/// An entry in a tree (file or subdirectory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Type of entry (blob or tree).
    pub entry_type: EntryType,
    /// POSIX file mode.
    pub mode: FileMode,
    /// Hash of the object.
    pub hash: Hash,
    /// Name of the entry (UTF-8).
    pub name: String,
}

impl TreeEntry {
    /// Create a new tree entry.
    pub fn new(entry_type: EntryType, mode: FileMode, hash: Hash, name: String) -> Result<Self> {
        if name.is_empty() {
            return Err(Error::invalid_tree_entry("Name cannot be empty"));
        }

        if name.len() > 255 {
            return Err(Error::invalid_tree_entry(format!(
                "Name too long: {} bytes (max 255)",
                name.len()
            )));
        }

        if name.contains('\0') {
            return Err(Error::invalid_tree_entry("Name cannot contain null bytes"));
        }

        Ok(Self {
            entry_type,
            mode,
            hash,
            name,
        })
    }

    /// Encode the entry to bytes.
    ///
    /// Format:
    /// - 1 byte: type (1=blob, 2=tree, 3=symlink)
    /// - 4 bytes: mode (u32 LE)
    /// - 32 bytes: hash (for symlinks: hash of the target-path blob)
    /// - 1 byte: name_len
    /// - N bytes: name (UTF-8)
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Type (1 byte)
        buf.push(self.entry_type.to_u8());

        // Mode (4 bytes, little-endian)
        buf.extend_from_slice(&self.mode.to_le_bytes());

        // Hash (32 bytes)
        buf.extend_from_slice(self.hash.as_bytes());

        // Name length (1 byte)
        buf.push(self.name.len() as u8);

        // Name (UTF-8)
        buf.extend_from_slice(self.name.as_bytes());

        buf
    }

    /// Decode an entry from a reader.
    pub fn decode<R: Read>(reader: &mut R) -> Result<Self> {
        // Read type (1 byte)
        let mut type_buf = [0u8; 1];
        reader.read_exact(&mut type_buf)?;
        let entry_type = EntryType::from_u8(type_buf[0])?;

        // Read mode (4 bytes)
        let mut mode_buf = [0u8; 4];
        reader.read_exact(&mut mode_buf)?;
        let mode = u32::from_le_bytes(mode_buf);

        // Read hash (32 bytes)
        let mut hash_buf = [0u8; 32];
        reader.read_exact(&mut hash_buf)?;
        let hash = Hash::from_bytes(hash_buf);

        // Read name length (1 byte)
        let mut name_len_buf = [0u8; 1];
        reader.read_exact(&mut name_len_buf)?;
        let name_len = name_len_buf[0] as usize;

        if name_len == 0 {
            return Err(Error::invalid_tree_entry("Name length is zero"));
        }

        // Read name
        let mut name_buf = vec![0u8; name_len];
        reader.read_exact(&mut name_buf)?;
        let name = String::from_utf8(name_buf)
            .map_err(|e| Error::invalid_tree_entry(format!("Invalid UTF-8 in name: {}", e)))?;

        Self::new(entry_type, mode, hash, name)
    }
}

impl PartialOrd for TreeEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TreeEntry {
    /// Compare by name (bytewise UTF-8) for canonical ordering.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name.as_bytes().cmp(other.name.as_bytes())
    }
}

/// Encode a list of tree entries (sorted by name).
pub fn encode_tree(mut entries: Vec<TreeEntry>) -> Vec<u8> {
    // Sort entries by name for canonical ordering
    entries.sort();

    let mut buf = Vec::new();
    for entry in entries {
        buf.extend_from_slice(&entry.encode());
    }
    buf
}

/// Decode a list of tree entries from bytes.
pub fn decode_tree(data: &[u8]) -> Result<Vec<TreeEntry>> {
    let mut reader = std::io::Cursor::new(data);
    let mut entries = Vec::new();

    while reader.position() < data.len() as u64 {
        let entry = TreeEntry::decode(&mut reader)?;
        entries.push(entry);
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entry_encode_decode() {
        let hash = Hash::hash_bytes(b"test");
        let entry = TreeEntry::new(
            EntryType::Blob,
            file_modes::REGULAR,
            hash,
            "test.txt".to_string(),
        )
        .unwrap();

        let encoded = entry.encode();
        let mut reader = std::io::Cursor::new(&encoded);
        let decoded = TreeEntry::decode(&mut reader).unwrap();

        assert_eq!(decoded, entry);
    }

    #[test]
    fn test_entry_name_validation() {
        let hash = Hash::hash_bytes(b"test");

        // Empty name
        assert!(
            TreeEntry::new(EntryType::Blob, file_modes::REGULAR, hash, "".to_string()).is_err()
        );

        // Name too long
        let long_name = "a".repeat(256);
        assert!(TreeEntry::new(EntryType::Blob, file_modes::REGULAR, hash, long_name).is_err());

        // Name with null byte
        assert!(
            TreeEntry::new(
                EntryType::Blob,
                file_modes::REGULAR,
                hash,
                "test\0file".to_string()
            )
            .is_err()
        );
    }

    #[test]
    fn test_tree_ordering() {
        let hash = Hash::hash_bytes(b"test");

        let entry1 = TreeEntry::new(
            EntryType::Blob,
            file_modes::REGULAR,
            hash,
            "a.txt".to_string(),
        )
        .unwrap();
        let entry2 = TreeEntry::new(
            EntryType::Blob,
            file_modes::REGULAR,
            hash,
            "b.txt".to_string(),
        )
        .unwrap();
        let entry3 = TreeEntry::new(
            EntryType::Blob,
            file_modes::REGULAR,
            hash,
            "c.txt".to_string(),
        )
        .unwrap();

        assert!(entry1 < entry2);
        assert!(entry2 < entry3);
    }

    #[test]
    fn test_encode_decode_tree() {
        let hash1 = Hash::hash_bytes(b"test1");
        let hash2 = Hash::hash_bytes(b"test2");

        let entries = vec![
            TreeEntry::new(
                EntryType::Blob,
                file_modes::REGULAR,
                hash1,
                "b.txt".to_string(),
            )
            .unwrap(),
            TreeEntry::new(
                EntryType::Blob,
                file_modes::REGULAR,
                hash2,
                "a.txt".to_string(),
            )
            .unwrap(),
        ];

        let encoded = encode_tree(entries.clone());
        let decoded = decode_tree(&encoded).unwrap();

        // Should be sorted by name
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].name, "a.txt");
        assert_eq!(decoded[1].name, "b.txt");
    }

    #[test]
    fn test_empty_tree() {
        let entries: Vec<TreeEntry> = vec![];
        let encoded = encode_tree(entries);
        assert_eq!(encoded.len(), 0);

        let decoded = decode_tree(&encoded).unwrap();
        assert_eq!(decoded.len(), 0);
    }

    // Property-based tests
    use proptest::prelude::*;

    // Strategy for generating valid entry names (1-255 chars, no nulls)
    fn arb_entry_name() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9._-]{1,255}".prop_filter("no null bytes", |s| !s.contains('\0'))
    }

    // Strategy for generating valid tree entries
    fn arb_tree_entry() -> impl Strategy<Value = TreeEntry> {
        (
            prop::sample::select(vec![EntryType::Blob, EntryType::Tree, EntryType::Symlink]),
            any::<u32>(),
            prop::array::uniform32(any::<u8>()),
            arb_entry_name(),
        )
            .prop_map(|(entry_type, mode, hash_bytes, name)| {
                TreeEntry::new(entry_type, mode, Hash::from_bytes(hash_bytes), name).unwrap()
            })
    }
    use proptest::collection;
    use std::collections::HashSet;

    fn arb_tree_entries_unique() -> impl Strategy<Value = Vec<TreeEntry>> {
        collection::vec(arb_tree_entry(), 1..20).prop_filter(
            "entry names must be unique",
            |entries| {
                let mut seen = HashSet::new();
                entries.iter().all(|e| seen.insert(e.name.clone()))
            },
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 56,
            max_shrink_iters: 10000,
            ..ProptestConfig::default()
        })]

        /// Property 10: TreeEntry round-trip
        #[test]
        fn prop_tree_entry_roundtrip(entry in arb_tree_entry()) {
            let encoded = entry.encode();
            let mut reader = std::io::Cursor::new(&encoded);
            let decoded = TreeEntry::decode(&mut reader)?;
            prop_assert_eq!(entry, decoded);
        }

        /// Property 11: Tree canonicalization - order-independent hashing
        #[test]
        fn prop_tree_canonicalization_order_independent(
            entries in arb_tree_entries_unique()
        ) {
            // Hash of sorted entries
            let mut sorted1 = entries.clone();
            sorted1.sort();
            let encoded1 = encode_tree(sorted1);
            let hash1 = Hash::hash_bytes(&encoded1);

            // Hash after reversing and re-sorting
            let mut reversed = entries;
            reversed.reverse();
            reversed.sort();
            let encoded2 = encode_tree(reversed);
            let hash2 = Hash::hash_bytes(&encoded2);

            prop_assert_eq!(
                hash1,
                hash2,
                "Tree hash must be independent of input ordering"
            );
        }

        /// Property 12: Empty names are rejected
        #[test]
        fn prop_empty_name_rejected(
            entry_type in prop::sample::select(vec![EntryType::Blob, EntryType::Tree, EntryType::Symlink]),
            mode in any::<u32>(),
            hash_bytes in prop::array::uniform32(any::<u8>()),
        ) {
            let result = TreeEntry::new(
                entry_type,
                mode,
                Hash::from_bytes(hash_bytes),
                String::new(),
            );
            prop_assert!(result.is_err());
        }

        /// Property 13: Names with null bytes are rejected
        #[test]
        fn prop_null_byte_rejected(
            prefix in "[a-zA-Z0-9]{0,10}",
            suffix in "[a-zA-Z0-9]{0,10}",
        ) {
            let name = format!("{}\0{}", prefix, suffix);
            let result = TreeEntry::new(
                EntryType::Blob,
                0o644,
                Hash::hash_bytes(b"test"),
                name,
            );
            prop_assert!(result.is_err());
        }

        /// Property 14: Names >255 bytes are rejected
        #[test]
        fn prop_long_name_rejected(name in "[a-zA-Z]{256,300}") {
            let result = TreeEntry::new(
                EntryType::Blob,
                0o644,
                Hash::hash_bytes(b"test"),
                name,
            );
            prop_assert!(result.is_err());
        }
    }
}
