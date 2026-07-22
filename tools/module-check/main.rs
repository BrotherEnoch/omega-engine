# omega-engine/tools/module-check/src/main.rs
use std::{
    collections::{HashMap, HashSet},
    env,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use rayon::prelude::*;

#[derive(Debug)]
struct ModuleDecl {
    file: PathBuf,
    name: String,
    line: usize,
}

fn main() {
    let root = env::args()
        .nth(1)
        .unwrap_or_else(|| ".".to_string());

    let root = PathBuf::from(root);
    let src_root = root.join("src");

    if !src_root.exists() {
        eprintln!("❌ src directory not found: {}", src_root.display());
        std::process::exit(2);
    }

    let files = collect_rs_files(&src_root);

    let decls: Vec<ModuleDecl> = files
        .par_iter()
        .flat_map(|file| parse_module_decls(file))
        .collect();

    let mut errors = Vec::new();

    let decl_map = build_index(&decls);

    validate_missing_files(&decl_map, &src_root, &mut errors);
    validate_duplicate_modules(&decl_map, &mut errors);

    if !errors.is_empty() {
        eprintln!("\n❌ Module integrity check failed:\n");
        for e in &errors {
            eprintln!("  - {e}");
        }
        std::process::exit(1);
    }

    println!("✅ Module integrity OK");
}

/// Collect all .rs files under src
fn collect_rs_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Parse `mod x;` or `pub mod x;`
fn parse_module_decls(file: &PathBuf) -> Vec<ModuleDecl> {
    let Ok(content) = fs::read_to_string(file) else {
        return vec![];
    };

    let mut out = Vec::new();

    for (i, line) in content.lines().enumerate() {
        let line = line.trim();

        if let Some(name) = extract_mod(line) {
            out.push(ModuleDecl {
                file: file.clone(),
                name,
                line: i + 1,
            });
        }
    }

    out
}

/// Fast parser (no regex)
fn extract_mod(line: &str) -> Option<String> {
    let line = line.strip_suffix(';').unwrap_or(line);

    if line.starts_with("pub mod ") {
        return Some(line["pub mod ".len()..].trim().to_string());
    }

    if line.starts_with("mod ") {
        return Some(line["mod ".len()..].trim().to_string());
    }

    None
}

/// Build index: module_name -> declarations
fn build_index(decls: &[ModuleDecl]) -> HashMap<String, Vec<&ModuleDecl>> {
    let mut map: HashMap<String, Vec<&ModuleDecl>> = HashMap::new();

    for d in decls {
        map.entry(d.name.clone()).or_default().push(d);
    }

    map
}

/// Validate missing module files
fn validate_missing_files(
    map: &HashMap<String, Vec<&ModuleDecl>>,
    src_root: &Path,
    errors: &mut Vec<String>,
) {
    for (name, decls) in map {
        for d in decls {
            let parent = d.file.parent().unwrap();

            let file_a = parent.join(format!("{name}.rs"));
            let file_b = parent.join(name).join("mod.rs");

            if !file_a.exists() && !file_b.exists() {
                errors.push(format!(
                    "Missing module file `{}` declared in {}:{}",
                    name,
                    d.file.display(),
                    d.line
                ));
            }
        }
    }
}

/// Detect duplicate module declarations in same scope
fn validate_duplicate_modules(
    map: &HashMap<String, Vec<&ModuleDecl>>,
    errors: &mut Vec<String>,
) {
    for (name, decls) in map {
        if decls.len() > 1 {
            let locations: Vec<String> = decls
                .iter()
                .map(|d| format!("{}:{}", d.file.display(), d.line))
                .collect();

            errors.push(format!(
                "Duplicate module `{}` declared in:\n    {}",
                name,
                locations.join("\n    ")
            ));
        }
    }
}
