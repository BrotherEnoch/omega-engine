// tools/check_modules.rs
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let crate_root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".".to_string());

    let root = PathBuf::from(crate_root);
    let src = root.join("src");

    println!("Scanning: {}", src.display());

    let mut errors = Vec::new();

    walk_dir(&src, &mut errors);

    if !errors.is_empty() {
        eprintln!("\n❌ Module integrity errors detected:\n");
        for e in &errors {
            eprintln!("  - {}", e);
        }
        std::process::exit(1);
    }

    println!("✅ Module tree is consistent");
}

fn walk_dir(dir: &Path, errors: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            walk_dir(&path, errors);
            continue;
        }

        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for line in content.lines() {
            let line = line.trim();

            if let Some(mod_name) = parse_mod(line) {
                let expected = path
                    .parent()
                    .unwrap()
                    .join(format!("{mod_name}.rs"));

                let expected_dir_mod = path
                    .parent()
                    .unwrap()
                    .join(mod_name)
                    .join("mod.rs");

                if !expected.exists() && !expected_dir_mod.exists() {
                    errors.push(format!(
                        "{} declares `mod {}` but file not found",
                        path.display(),
                        mod_name
                    ));
                }
            }
        }
    }
}

fn parse_mod(line: &str) -> Option<String> {
    if !line.starts_with("pub mod ") && !line.starts_with("mod ") {
        return None;
    }

    let line = line.trim_end_matches(';');
    let parts: Vec<&str> = line.split_whitespace().collect();

    if parts.len() < 2 {
        return None;
    }

    Some(parts[1].to_string())
}