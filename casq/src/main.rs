use anyhow::{Context, Result};
use casq_core::{Algorithm, Hash, Store};
use clap::{Parser, Subcommand};
use std::io;
use std::path::{Path, PathBuf};

mod output;
use output::*;

/// casq - A content-addressed file store
#[derive(Parser)]
#[command(name = "casq")]
#[command(about = "Content-addressed file store using BLAKE3", long_about = None)]
#[command(version)]
struct Cli {
    /// Store root directory (defaults to CASQ_ROOT env var or ./casq-store)
    #[arg(short = 'R', long, global = true)]
    root: Option<PathBuf>,

    /// Output results as JSON
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new store
    Initialize {
        /// Hash algorithm to use
        #[arg(short, long, default_value = "blake3")]
        algorithm: String,
    },

    /// Put files or directories to the store
    Put {
        /// Path to add (or "-" to read from stdin)
        #[arg(required = true)]
        path: String,

        /// Create a reference to the added content
        #[arg(short, long)]
        reference: Option<String>,
    },

    /// Materialize an object to the filesystem
    Materialize {
        /// Hash of the object to materialize
        #[arg(required = true)]
        hash_or_reference: String,

        /// Destination path
        #[arg(required = true)]
        destination: PathBuf,
    },

    /// Output blob content and write it to stdout
    Get {
        /// Hash of the blob
        #[arg(required = true)]
        hash_or_reference: String,
    },

    /// List tree children
    List {
        /// Hash of the object
        #[arg(required = true)]
        hash_or_reference: String,

        /// Show detailed information
        #[arg(short, long)]
        long: bool,
    },

    /// Show object metadata
    Metadata {
        /// Hash of the object
        #[arg(required = true)]
        hash_or_reference: String,
    },

    /// Garbage collect unreferenced objects
    CollectGarbage {
        /// Dry run - show what would be deleted without deleting
        #[arg(long)]
        dry_run: bool,
    },

    /// Find orphaned objects (unreferenced trees and blobs)
    FindOrphans {
        /// Show detailed information
        #[arg(short, long)]
        long: bool,
    },

    /// Manage references
    #[command(subcommand)]
    References(ReferencesCommands),
}

#[derive(Subcommand)]
enum ReferencesCommands {
    /// Add a reference
    Add {
        /// Reference name
        #[arg(required = true)]
        reference: String,

        /// Hash to reference
        #[arg(required = true)]
        hash: String,
    },

    /// List all references
    List,

    /// Remove a reference
    Remove {
        /// Reference name
        #[arg(required = true)]
        reference: String,
    },
}

fn main() {
    let cli = Cli::parse();

    // Determine store root: CLI arg > CASTOR_ROOT env var > ./casq-store default
    let root = cli
        .root
        .or_else(|| std::env::var("CASQ_ROOT").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("./casq-store"));

    // Create output writer
    let output = OutputWriter::new(cli.json);

    // Execute command and handle errors
    let result = match cli.command {
        Commands::Initialize { algorithm } => cmd_init(&root, &algorithm, &output),
        Commands::Put { path, reference } => cmd_put(&root, path, reference, &output),
        Commands::Materialize {
            hash_or_reference,
            destination,
        } => cmd_materialize(&root, &hash_or_reference, &destination, &output),
        Commands::Get { hash_or_reference } => cmd_get(&root, &hash_or_reference, &output),
        Commands::List {
            hash_or_reference,
            long,
        } => cmd_list(&root, &hash_or_reference, long, &output),
        Commands::Metadata { hash_or_reference } => {
            cmd_metadata(&root, &hash_or_reference, &output)
        }
        Commands::CollectGarbage { dry_run } => cmd_collect_garbage(&root, dry_run, &output),
        Commands::FindOrphans { long } => cmd_get_orphans(&root, long, &output),
        Commands::References(references_cmd) => match references_cmd {
            ReferencesCommands::Add { reference, hash } => {
                cmd_references_add(&root, &reference, &hash, &output)
            }
            ReferencesCommands::List => cmd_references_list(&root, &output),
            ReferencesCommands::Remove { reference } => {
                cmd_references_remove(&root, &reference, &output)
            }
        },
    };

    // Handle errors and set exit code
    if let Err(e) = result {
        output.write_error(&e, 1);
        std::process::exit(1);
    }
}

fn cmd_init(root: &Path, algo: &str, output: &OutputWriter) -> Result<()> {
    let algorithm = match algo {
        "blake3" => Algorithm::Blake3,
        _ => anyhow::bail!("Unsupported algorithm: {}", algo),
    };

    Store::init(root, algorithm)
        .with_context(|| format!("Failed to initialize store at {}", root.display()))?;

    let full_path = std::path::absolute(root)?;
    let full_path_lossy = full_path.to_string_lossy();
    let data = InitOutput {
        success: true,
        result_code: 0,
        root: root.display().to_string(),
        algorithm: algorithm.as_str().to_string(),
    };

    // Only write informational message to stderr in text mode
    if !output.is_json() {
        use std::io::Write;
        writeln!(
            io::stderr(),
            "Initialized casq store at {} (algorithm {})",
            full_path_lossy,
            algorithm.as_str()
        )?;
    }

    // Write data to stdout (path in text mode, JSON in JSON mode)
    output.write(&data, || format!("{}\n", full_path_lossy))?;

    Ok(())
}

fn cmd_put(
    root: &Path,
    path: String,
    ref_name: Option<String>,
    output: &OutputWriter,
) -> Result<()> {
    let store =
        Store::open(root).with_context(|| format!("Failed to open store at {}", root.display()))?;

    // Detect stdin mode
    let has_stdin = path == "-";

    let object: AddedObject;
    let mut reference = None;

    // Process based on mode
    if has_stdin {
        let hash = add_from_stdin(&store)?;
        object = AddedObject {
            hash,
            path: "(stdin)".to_string(),
        };

        // Create reference if requested
        if let Some(ref name) = ref_name {
            store
                .refs()
                .add(name, &hash)
                .with_context(|| format!("Failed to create reference: {}", name))?;
            reference = Some(ReferenceCreated {
                name: name.clone(),
                hash,
            });
        }
    } else {
        // Regular filesystem paths
        let path = PathBuf::from(&path);
        let hash = store
            .add_path(&path)
            .with_context(|| format!("Failed to add path: {}", path.display()))?;

        object = AddedObject {
            hash,
            path: path.display().to_string(),
        };

        // Create reference if requested
        if let Some(ref name) = ref_name {
            store
                .refs()
                .add(name, &hash)
                .with_context(|| format!("Failed to create reference: {}", name))?;
            reference = Some(ReferenceCreated {
                name: name.clone(),
                hash,
            });
        }
    }

    let data = AddOutput {
        success: true,
        result_code: 0,
        object,
        reference: reference.clone(),
    };

    // Output reference confirmation to stderr (if applicable)
    if let Some(ref r) = reference {
        use std::io::Write;
        let _ = writeln!(io::stderr(), "{}", r.name);
    }

    // Output hash+path data to stdout
    output.write(&data, || {
        let mut text = String::new();
        text.push_str(&format!("{}\n", data.object.hash));
        text
    })?;

    Ok(())
}

fn add_from_stdin(store: &Store) -> Result<Hash> {
    // Validate stdin is not a TTY (prevent user confusion)
    if atty::is(atty::Stream::Stdin) {
        anyhow::bail!(
            "stdin is a terminal (refusing to read). Pipe data with: command | casq add -"
        );
    }

    let stdin = io::stdin();
    let hash = store
        .add_stdin(stdin.lock())
        .with_context(|| "Failed to read from stdin")?;

    Ok(hash)
}

fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_reference(s: &str) -> bool {
    !is_hash(s)
}

fn get_hash(store: &Store, hash_or_reference_str: &str) -> Result<Hash> {
    if hash_or_reference_str.is_empty() {
        Err(anyhow::anyhow!("Missing hash or reference."))
    } else if is_reference(hash_or_reference_str) {
        let maybe_hash: Option<Hash> = store.refs().get(hash_or_reference_str)?;
        let hash = maybe_hash
            .ok_or_else(|| anyhow::anyhow!("Unknown reference: {}", hash_or_reference_str))?;
        Ok(hash)
    } else {
        Hash::from_hex(hash_or_reference_str)
            .with_context(|| format!("Invalid hash: {}", hash_or_reference_str))
    }
}

fn cmd_materialize(
    root: &Path,
    hash_or_reference_str: &str,
    dest: &Path,
    output: &OutputWriter,
) -> Result<()> {
    let store =
        Store::open(root).with_context(|| format!("Failed to open store at {}", root.display()))?;

    let hash = get_hash(&store, hash_or_reference_str)?;

    store
        .materialize(&hash, dest)
        .with_context(|| format!("Failed to materialize {} to {}", hash, dest.display()))?;

    let data = MaterializeOutput {
        success: true,
        result_code: 0,
        hash,
        destination: dest.display().to_string(),
    };

    output.write_info(&data, || {
        format!("Materialized {} to {}\n", hash, dest.display())
    })?;

    Ok(())
}

fn cmd_get(root: &Path, hash_or_reference_str: &str, output: &OutputWriter) -> Result<()> {
    if output.is_json() {
        anyhow::bail!(
            "The 'cat' command outputs binary data to stdout and cannot be used with --json.\n\
             Alternatives:\n\
             - Use 'materialize' to save to a file\n\
             - Use 'stat' to get metadata\n\
             - Use 'ls' to list tree contents"
        );
    }

    let store =
        Store::open(root).with_context(|| format!("Failed to open store at {}", root.display()))?;
    let hash = get_hash(&store, hash_or_reference_str)?;

    let stdout = io::stdout();
    let mut handle = stdout.lock();

    store
        .cat_blob(&hash, &mut handle)
        .with_context(|| format!("Failed to output blob {}", hash))?;

    Ok(())
}

fn cmd_list(
    root: &Path,
    hash_or_reference_str: &str,
    long: bool,
    output: &OutputWriter,
) -> Result<()> {
    let store =
        Store::open(root).with_context(|| format!("Failed to open store at {}", root.display()))?;

    let hash = get_hash(&store, hash_or_reference_str)?;

    // Check if it's a tree or blob
    let obj_path = store.object_path(&hash);
    if !obj_path.exists() {
        anyhow::bail!("Object not found: {}", hash);
    }

    // Try to read as tree first
    match store.get_tree(&hash) {
        Ok(entries) => {
            // It's a tree
            let entry_infos: Vec<TreeEntryInfo> = entries
                .iter()
                .map(|e| TreeEntryInfo {
                    name: e.name.clone(),
                    entry_type: match e.entry_type {
                        casq_core::EntryType::Blob => "blob".to_string(),
                        casq_core::EntryType::Tree => "tree".to_string(),
                        casq_core::EntryType::Symlink => "symlink".to_string(),
                    },
                    mode: if long {
                        Some(format!("{:06o}", e.mode))
                    } else {
                        None
                    },
                    hash: if long { Some(e.hash) } else { None },
                })
                .collect();

            let data = LsOutput {
                success: true,
                result_code: 0,
                data: LsData::TreeContents {
                    hash,
                    entries: entry_infos.clone(),
                },
            };

            output.write(&data, || {
                let mut text = String::new();
                for entry in &entries {
                    if long {
                        let type_char = match entry.entry_type {
                            casq_core::EntryType::Blob => 'b',
                            casq_core::EntryType::Tree => 't',
                            casq_core::EntryType::Symlink => 'l',
                        };
                        text.push_str(&format!(
                            "{} {:06o} {} {}\n",
                            type_char, entry.mode, entry.hash, entry.name
                        ));
                    } else {
                        text.push_str(&format!("{}\n", entry.name));
                    }
                }
                text
            })?;
        }
        Err(_) => {
            // Try as blob
            let blob = store
                .get_blob(&hash)
                .with_context(|| format!("Failed to read object {}", hash))?;

            let data = LsOutput {
                success: true,
                result_code: 0,
                data: LsData::BlobInfo(BlobInfo {
                    hash,
                    size: blob.len() as u64,
                }),
            };

            output.write(&data, || {
                if long {
                    format!("blob {} bytes\n", blob.len())
                } else {
                    "blob\n".to_string()
                }
            })?;
        }
    }

    Ok(())
}

fn cmd_metadata(root: &Path, hash_or_reference_str: &str, output: &OutputWriter) -> Result<()> {
    let store =
        Store::open(root).with_context(|| format!("Failed to open store at {}", root.display()))?;

    let hash = get_hash(&store, hash_or_reference_str)?;

    let obj_path = store.object_path(&hash);
    if !obj_path.exists() {
        anyhow::bail!("Object not found: {}", hash);
    }

    // Get file metadata
    let metadata =
        std::fs::metadata(&obj_path).with_context(|| "Failed to read object metadata")?;

    // Try to determine type
    match store.get_tree(&hash) {
        Ok(entries) => {
            let data = StatOutput {
                success: true,
                result_code: 0,
                data: StatData::Tree(TreeStatInfo {
                    hash,
                    entry_count: entries.len(),
                    path: obj_path.display().to_string(),
                }),
            };

            output.write(&data, || {
                format!(
                    "Hash: {}\nType: tree\nEntries: {}\nSize: {} bytes (on disk)\nPath: {}\n",
                    hash,
                    entries.len(),
                    metadata.len(),
                    obj_path.display()
                )
            })?;
        }
        Err(_) => {
            // It's a blob
            let blob = store
                .get_blob(&hash)
                .with_context(|| "Failed to read blob")?;

            let data = StatOutput {
                success: true,
                result_code: 0,
                data: StatData::Blob(BlobStatInfo {
                    hash,
                    size: blob.len() as u64,
                    size_on_disk: metadata.len(),
                    path: obj_path.display().to_string(),
                }),
            };

            output.write(&data, || {
                format!(
                    "Hash: {}\nType: blob\nSize: {} bytes\nSize (on disk): {} bytes\nPath: {}\n",
                    hash,
                    blob.len(),
                    metadata.len(),
                    obj_path.display()
                )
            })?;
        }
    }

    Ok(())
}

fn cmd_collect_garbage(root: &Path, dry_run: bool, output: &OutputWriter) -> Result<()> {
    let store =
        Store::open(root).with_context(|| format!("Failed to open store at {}", root.display()))?;

    let stats = store
        .gc(dry_run)
        .with_context(|| "Failed to run garbage collection")?;

    let data = GcOutput {
        success: true,
        result_code: 0,
        dry_run,
        objects_deleted: stats.objects_deleted,
        bytes_freed: stats.bytes_freed,
    };

    output.write(&data, || {
        if dry_run {
            format!(
                "Dry run - no objects deleted\nWould delete {} objects\nWould free {} bytes\n",
                stats.objects_deleted, stats.bytes_freed
            )
        } else {
            format!(
                "Deleted {} objects\nFreed {} bytes\n",
                stats.objects_deleted, stats.bytes_freed
            )
        }
    })?;

    Ok(())
}

fn cmd_get_orphans(root: &Path, long: bool, output: &OutputWriter) -> Result<()> {
    let store =
        Store::open(root).with_context(|| format!("Failed to open store at {}", root.display()))?;

    let orphans = store
        .find_orphan_roots()
        .with_context(|| "Failed to find orphan roots")?;

    let orphan_infos: Vec<OrphanInfo> = orphans.iter().map(|o| o.clone().into()).collect();

    let data = OrphansOutput {
        success: true,
        result_code: 0,
        orphans: orphan_infos.clone(),
    };

    if orphan_infos.is_empty() {
        // Empty state message → stderr
        output.write_info(&data, || "No orphaned objects found\n".to_string())?;
    } else {
        // Data output → stdout
        output.write(&data, || {
            let mut text = String::new();
            for orphan in &orphan_infos {
                if long {
                    text.push_str(&format!("Hash: {}\n", orphan.hash));
                    text.push_str(&format!("Type: {}\n", orphan.object_type));
                    if let Some(entries) = orphan.entry_count {
                        text.push_str(&format!("Entries: {}\n", entries));
                    }
                    text.push_str(&format!("Approx size: {} bytes\n", orphan.approx_size));
                    text.push_str("---\n");
                } else if let Some(entries) = orphan.entry_count {
                    text.push_str(&format!(
                        "{}  {} ({} entries)\n",
                        orphan.hash, orphan.object_type, entries
                    ));
                } else {
                    text.push_str(&format!("{}  {}\n", orphan.hash, orphan.object_type));
                }
            }
            text
        })?;
    }

    Ok(())
}

fn cmd_references_add(
    root: &Path,
    name: &str,
    hash_str: &str,
    output: &OutputWriter,
) -> Result<()> {
    let store =
        Store::open(root).with_context(|| format!("Failed to open store at {}", root.display()))?;

    let hash = Hash::from_hex(hash_str).with_context(|| format!("Invalid hash: {}", hash_str))?;

    store
        .refs()
        .add(name, &hash)
        .with_context(|| format!("Failed to add reference: {}", name))?;

    let data = RefsAddOutput {
        success: true,
        result_code: 0,
        name: name.to_string(),
        hash,
    };

    output.write_info(&data, || format!("{} -> {}\n", name, hash))?;

    Ok(())
}

fn cmd_references_list(root: &Path, output: &OutputWriter) -> Result<()> {
    let store =
        Store::open(root).with_context(|| format!("Failed to open store at {}", root.display()))?;

    let refs = store
        .refs()
        .list()
        .with_context(|| "Failed to list references")?;

    let ref_infos: Vec<RefInfo> = refs
        .into_iter()
        .map(|(name, hash)| RefInfo { name, hash })
        .collect();

    let data = RefsListOutput {
        success: true,
        result_code: 0,
        refs: ref_infos.clone(),
    };

    if ref_infos.is_empty() {
        // Empty state message → stderr
        output.write_info(&data, || "No references\n".to_string())?;
    } else {
        // Data output → stdout
        output.write(&data, || {
            let mut text = String::new();
            for r in &ref_infos {
                text.push_str(&format!("{} {}\n", r.hash, r.name));
            }
            text
        })?;
    }

    Ok(())
}

fn cmd_references_remove(root: &Path, name: &str, output: &OutputWriter) -> Result<()> {
    let store =
        Store::open(root).with_context(|| format!("Failed to open store at {}", root.display()))?;

    store
        .refs()
        .remove(name)
        .with_context(|| format!("Failed to remove reference: {}", name))?;

    let data = RefsRmOutput {
        success: true,
        result_code: 0,
        name: name.to_string(),
    };

    output.write_info(&data, || format!("Removed reference: {}\n", name))?;

    Ok(())
}
