//! `cmm du <path> [--depth 1] [--top 20]` — quick allocated-size breakdown using the core sizer
//! (parallel, hardlinks counted once, symlinks not followed, other volumes skipped).

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result};
use cmm_core::config::home_dir;
use cmm_core::size::{SizeInfo, Sizer};
use rayon::prelude::*;

use super::{display_path, fmt_bytes};

#[derive(Debug, clap::Args)]
pub struct DuArgs {
    pub path: PathBuf,
    /// How many directory levels to break down.
    #[arg(long, default_value_t = 1)]
    pub depth: u32,
    /// Show at most this many entries per directory (biggest first).
    #[arg(long, default_value_t = 20)]
    pub top: usize,
}

#[derive(Debug)]
pub struct Node {
    pub path: PathBuf,
    pub info: SizeInfo,
    /// Sorted by allocated bytes, biggest first. Empty for files and at the depth limit.
    pub children: Vec<Node>,
}

pub fn run(args: DuArgs) -> Result<ExitCode> {
    let started = Instant::now();
    let sizer = Sizer::new();
    let root = measure(&sizer, &args.path, args.depth)?;
    let home = home_dir();
    println!(
        "{}  {} allocated ({} logical), {} files  [{:.2} s]",
        display_path(&args.path.to_string_lossy(), &home),
        fmt_bytes(root.info.bytes),
        fmt_bytes(root.info.logical_bytes),
        root.info.files,
        started.elapsed().as_secs_f64()
    );
    println!("{:>10}  {:>9}  NAME", "SIZE", "FILES");
    print_children(&root, args.top, 0);

    let errors = sizer.take_errors();
    if !errors.is_empty() {
        eprintln!("\n{} unreadable paths (not counted):", errors.len());
        for e in errors.iter().take(5) {
            eprintln!("  {}: {}", display_path(&e.path, &home), e.message);
        }
        if errors.len() > 5 {
            eprintln!("  … and {} more", errors.len() - 5);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Sizes `path`, breaking directories down `depth` levels. Children on another device are skipped.
pub fn measure(sizer: &Sizer, path: &Path, depth: u32) -> Result<Node> {
    let meta = path.symlink_metadata().with_context(|| format!("cannot read {}", path.display()))?;
    Ok(node(sizer, path, &meta, depth))
}

fn node(sizer: &Sizer, path: &Path, meta: &std::fs::Metadata, depth: u32) -> Node {
    if depth == 0 || !meta.is_dir() {
        return Node { path: path.to_path_buf(), info: sizer.size_of(path), children: Vec::new() };
    }
    let entries: Vec<(PathBuf, std::fs::Metadata)> = match std::fs::read_dir(path) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter_map(|e| e.metadata().ok().map(|m| (e.path(), m)))
            .filter(|(_, m)| m.dev() == meta.dev())
            .collect(),
        Err(e) => {
            sizer.push_error(path.to_string_lossy(), e.to_string());
            Vec::new()
        }
    };
    let mut children: Vec<Node> = entries.par_iter().map(|(p, m)| node(sizer, p, m, depth - 1)).collect();
    children.sort_by(|a, b| b.info.bytes.cmp(&a.info.bytes).then_with(|| a.path.cmp(&b.path)));

    // The directory entry itself plus everything below it.
    let mut info = SizeInfo {
        bytes: meta.blocks() * 512,
        logical_bytes: meta.len(),
        files: 0,
        newest_mtime: chrono::DateTime::from_timestamp(meta.mtime(), meta.mtime_nsec() as u32),
    };
    for child in &children {
        info.bytes += child.info.bytes;
        info.logical_bytes += child.info.logical_bytes;
        info.files += child.info.files;
        info.newest_mtime = info.newest_mtime.max(child.info.newest_mtime);
    }
    Node { path: path.to_path_buf(), info, children }
}

fn print_children(node: &Node, top: usize, indent: usize) {
    for child in node.children.iter().take(top) {
        let name =
            child.path.file_name().map_or_else(|| child.path.display().to_string(), |n| n.to_string_lossy().into());
        let slash = if child.path.symlink_metadata().is_ok_and(|m| m.is_dir()) { "/" } else { "" };
        println!("{:>10}  {:>9}  {:indent$}{name}{slash}", fmt_bytes(child.info.bytes), child.info.files, "");
        print_children(child, top, indent + 2);
    }
    if node.children.len() > top {
        let rest = &node.children[top..];
        let bytes: u64 = rest.iter().map(|c| c.info.bytes).sum();
        println!("{:>10}  {:>9}  {:indent$}(+{} more)", fmt_bytes(bytes), "", "", rest.len());
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn breaks_down_by_depth_and_sums_consistently() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("big/inner")).unwrap();
        fs::write(dir.path().join("big/inner/a.bin"), vec![1u8; 300_000]).unwrap();
        fs::write(dir.path().join("big/b.bin"), vec![1u8; 100_000]).unwrap();
        fs::write(dir.path().join("small.bin"), vec![1u8; 10_000]).unwrap();
        fs::hard_link(dir.path().join("big/b.bin"), dir.path().join("hardlink.bin")).unwrap();

        let flat = measure(&Sizer::new(), dir.path(), 0).unwrap();
        let tree = measure(&Sizer::new(), dir.path(), 2).unwrap();
        assert_eq!(tree.info, flat.info, "breakdown sums to the plain size");
        assert_eq!(tree.info.files, 3, "hardlink counted once");

        let names: Vec<_> = tree.children.iter().map(|c| c.path.file_name().unwrap().to_owned()).collect();
        assert_eq!(names[0], "big", "biggest first");
        assert_eq!(tree.children[0].children.len(), 2, "second level present");
        assert!(tree.children[0].children[0].children.is_empty(), "depth limit");
    }

    #[test]
    fn missing_path_is_an_error() {
        assert!(measure(&Sizer::new(), Path::new("/definitely/not/here"), 1).is_err());
    }
}
