//! Output formatting for CLI commands.
//!
//! Provides abstraction layer for outputting results in text or JSON format.

use anyhow::Result;
use casq_core::{Hash, OrphanRoot};
use serde::Serialize;
use std::io::{self, Write};

/// Output format selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
}

/// Writer for command output with format abstraction.
pub struct OutputWriter {
    format: OutputFormat,
    stdout: io::Stdout,
}

impl OutputWriter {
    /// Create a new OutputWriter.
    pub fn new(json: bool) -> Self {
        Self {
            format: if json {
                OutputFormat::Json
            } else {
                OutputFormat::Text
            },
            stdout: io::stdout(),
        }
    }

    /// Check if JSON mode is enabled.
    pub fn is_json(&self) -> bool {
        self.format == OutputFormat::Json
    }

    /// Write output using the configured format.
    ///
    /// The `data` parameter must be a serializable struct that includes
    /// `success: bool` and `result_code: u8` fields.
    ///
    /// The `text_fn` closure is called only in text mode to generate the
    /// human-readable output.
    pub fn write<T: Serialize>(&self, data: &T, text_fn: impl FnOnce() -> String) -> Result<()> {
        match self.format {
            OutputFormat::Json => {
                let json = serde_json::to_string_pretty(data)?;
                writeln!(&self.stdout, "{}", json)?;
            }
            OutputFormat::Text => {
                let text = text_fn();
                if !text.is_empty() {
                    write!(&self.stdout, "{}", text)?;
                }
            }
        }
        Ok(())
    }

    /// Write informational output to the appropriate stream.
    ///
    /// In text mode, writes to stderr (for informational messages like confirmations).
    /// In JSON mode, writes to stdout as JSON (structured data).
    ///
    /// This separates informational messages from data output in text mode,
    /// following Unix conventions and enabling proper pipeline usage.
    pub fn write_info<T: Serialize>(
        &self,
        data: &T,
        text_fn: impl FnOnce() -> String,
    ) -> Result<()> {
        match self.format {
            OutputFormat::Json => {
                // JSON mode: write to stdout (structured data)
                let json = serde_json::to_string_pretty(data)?;
                writeln!(&self.stdout, "{}", json)?;
            }
            OutputFormat::Text => {
                // Text mode: write to stderr (informational message)
                let text = text_fn();
                if !text.is_empty() {
                    write!(io::stderr(), "{}", text)?;
                }
            }
        }
        Ok(())
    }

    /// Write an error message to stderr.
    ///
    /// In JSON mode, writes a JSON error object with success=false.
    /// In text mode, writes the error message directly.
    pub fn write_error(&self, error: &anyhow::Error, result_code: u8) {
        // `{:#}` renders the full anyhow context chain as
        // "outer: inner: root", so the actual cause (e.g. which symlink
        // triggered the failure) reaches the user instead of being swallowed
        // by the top-level `with_context` message.
        match self.format {
            OutputFormat::Json => {
                let error_output = ErrorOutput {
                    success: false,
                    result_code,
                    error: format!("{:#}", error),
                };
                if let Ok(json) = serde_json::to_string_pretty(&error_output) {
                    let _ = writeln!(io::stderr(), "{}", json);
                }
            }
            OutputFormat::Text => {
                let _ = writeln!(io::stderr(), "Error: {:#}", error);
            }
        }
    }
}

// ============================================================================
// Data Transfer Objects (DTOs) for JSON output
// ============================================================================

/// Error output structure.
#[derive(Debug, Serialize)]
pub struct ErrorOutput {
    pub success: bool,
    pub result_code: u8,
    pub error: String,
}

/// Output for `init` command.
#[derive(Debug, Serialize)]
pub struct InitOutput {
    pub success: bool,
    pub result_code: u8,
    pub root: String,
    pub algorithm: String,
}

/// Object added during `add` command.
#[derive(Debug, Clone, Serialize)]
pub struct AddedObject {
    pub hash: Hash,
    pub path: String,
}

/// Reference created during `add` command.
#[derive(Debug, Clone, Serialize)]
pub struct ReferenceCreated {
    pub name: String,
    pub hash: Hash,
}

/// Output for `add` command.
#[derive(Debug, Serialize)]
pub struct AddOutput {
    pub success: bool,
    pub result_code: u8,
    pub object: AddedObject,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<ReferenceCreated>,
}

/// Output for `materialize` command.
#[derive(Debug, Serialize)]
pub struct MaterializeOutput {
    pub success: bool,
    pub result_code: u8,
    pub hash: Hash,
    pub destination: String,
}

/// Reference information for `refs list`.
#[derive(Debug, Clone, Serialize)]
pub struct RefInfo {
    pub name: String,
    pub hash: Hash,
}

/// Tree entry information for `ls` command.
#[derive(Debug, Clone, Serialize)]
pub struct TreeEntryInfo {
    pub name: String,
    pub entry_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<Hash>,
}

/// Blob information for `ls` command.
#[derive(Debug, Serialize)]
pub struct BlobInfo {
    pub hash: Hash,
    pub size: u64,
}

/// Data variants for `ls` command.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum LsData {
    TreeContents {
        hash: Hash,
        entries: Vec<TreeEntryInfo>,
    },
    BlobInfo(BlobInfo),
}

/// Output for `ls` command.
#[derive(Debug, Serialize)]
pub struct LsOutput {
    pub success: bool,
    pub result_code: u8,
    #[serde(flatten)]
    pub data: LsData,
}

/// Tree statistics for `stat` command.
#[derive(Debug, Serialize)]
pub struct TreeStatInfo {
    pub hash: Hash,
    pub entry_count: usize,
    pub path: String,
}

/// Blob statistics for `stat` command.
#[derive(Debug, Serialize)]
pub struct BlobStatInfo {
    pub hash: Hash,
    pub size: u64,
    pub size_on_disk: u64,
    pub path: String,
}

/// Data variants for `stat` command.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum StatData {
    Tree(TreeStatInfo),
    Blob(BlobStatInfo),
}

/// Output for `stat` command.
#[derive(Debug, Serialize)]
pub struct StatOutput {
    pub success: bool,
    pub result_code: u8,
    #[serde(flatten)]
    pub data: StatData,
}

/// Output for `gc` command.
#[derive(Debug, Serialize)]
pub struct GcOutput {
    pub success: bool,
    pub result_code: u8,
    pub dry_run: bool,
    pub objects_deleted: usize,
    pub bytes_freed: u64,
}

/// Orphan information for `orphans` command.
#[derive(Debug, Clone, Serialize)]
pub struct OrphanInfo {
    pub hash: Hash,
    pub object_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_count: Option<usize>,
    pub approx_size: u64,
}

impl From<OrphanRoot> for OrphanInfo {
    fn from(root: OrphanRoot) -> Self {
        use casq_core::ObjectType;

        Self {
            hash: root.hash,
            object_type: match root.object_type {
                ObjectType::Blob => "blob".to_string(),
                ObjectType::Tree => "tree".to_string(),
                ObjectType::ChunkList => "chunk_list".to_string(),
            },
            entry_count: root.entry_count,
            approx_size: root.approx_size,
        }
    }
}

/// Output for `orphans` command.
#[derive(Debug, Serialize)]
pub struct OrphansOutput {
    pub success: bool,
    pub result_code: u8,
    pub orphans: Vec<OrphanInfo>,
}

/// Output for `refs add` command.
#[derive(Debug, Serialize)]
pub struct RefsAddOutput {
    pub success: bool,
    pub result_code: u8,
    pub name: String,
    pub hash: Hash,
}

/// Output for `refs list` command.
#[derive(Debug, Serialize)]
pub struct RefsListOutput {
    pub success: bool,
    pub result_code: u8,
    pub refs: Vec<RefInfo>,
}

/// Output for `refs rm` command.
#[derive(Debug, Serialize)]
pub struct RefsRmOutput {
    pub success: bool,
    pub result_code: u8,
    pub name: String,
}

// -------------------------
// Tests for output module
// -------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use casq_core::ObjectType;

    #[derive(serde::Serialize)]
    struct Simple {
        success: bool,
        result_code: u8,
        msg: String,
    }

    #[test]
    fn test_output_writer_json_and_text_write() {
        // JSON mode
        let writer_json = OutputWriter::new(true);
        let data = Simple {
            success: true,
            result_code: 0,
            msg: "ok".to_string(),
        };
        let _ = writer_json.write(&data, || "text".to_string());
        let _ = writer_json.write_info(&data, || "info".to_string());
        writer_json.write_error(&anyhow!("boom"), 1);

        // Text mode
        let writer_text = OutputWriter::new(false);
        let _ = writer_text.write(&data, || "plain text".to_string());
        let _ = writer_text.write_info(&data, || "info text".to_string());
        writer_text.write_error(&anyhow!("oops"), 2);
    }

    #[test]
    fn test_orphaninfo_from_orphanroot() {
        use casq_core::Hash;
        use casq_core::OrphanRoot;

        let hash = Hash::hash_bytes(b"abc");
        let root = OrphanRoot {
            hash,
            object_type: ObjectType::Blob,
            entry_count: None,
            approx_size: 123,
        };
        let info: OrphanInfo = root.into();
        assert_eq!(info.hash, hash);
        assert_eq!(info.object_type, "blob");
        assert_eq!(info.entry_count, None);
        assert_eq!(info.approx_size, 123);
    }
}
