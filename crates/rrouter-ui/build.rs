//! Embeds `ui/dist` into the binary.
//!
//! The dashboard is built with Vite (`npm run build` in `ui/`), which is a separate
//! toolchain. This build script keeps the Rust build honest either way: when the dist
//! directory is missing (fresh clone, CI without node) the crate compiles with an empty
//! asset table and the gateway keeps serving its built-in shell.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let dist = manifest.join("../../ui/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    println!("cargo:rerun-if-changed=build.rs");

    let mut files: Vec<PathBuf> = Vec::new();
    collect(&dist, &mut files);
    files.sort();

    let mut out = String::new();
    out.push_str("/// Every file Vite emitted, sorted by path.\n");
    out.push_str("pub static ASSETS: &[Asset] = &[\n");
    for path in &files {
        let rel = path.strip_prefix(&dist).expect("inside dist");
        let rel = rel.to_string_lossy().replace('\\', "/");
        let _ = writeln!(
            out,
            "    Asset {{ path: {rel:?}, bytes: include_bytes!({:?}), content_type: {:?}, hashed: {} }},",
            path.display().to_string(),
            content_type(&rel),
            rel.starts_with("assets/"),
        );
    }
    out.push_str("];\n");

    let dest = PathBuf::from(std::env::var("OUT_DIR").expect("out dir")).join("assets.rs");
    std::fs::write(dest, out).expect("write assets.rs");
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// Content types for the files Vite actually emits; anything else is served as bytes.
fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
