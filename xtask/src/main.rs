//! `cargo xtask <task>`: development tasks that are not part of the reprise CLI.

use std::path::{Path, PathBuf};

use anyhow::Result;

mod reset;

const USAGE: &str = "usage: cargo xtask reset [OPTIONS]\n";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("reset") => reset::run(&workspace_root(), &args[1..]),
        _ => {
            eprintln!("{USAGE}{}", reset::USAGE);
            std::process::exit(2);
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives under the repo root")
        .to_path_buf()
}
