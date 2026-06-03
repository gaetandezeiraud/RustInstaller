#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cleanup;
mod stages;
mod ui;

use anyhow::Result;
use std::path::PathBuf;

fn main() {
    if let Err(e) = run() {
        ui::fatal(&format!("{e:#}"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    ui::set_translator(common::i18n::Translator::detect(&args));

    if let Some(idx) = args.iter().position(|a| a == "--finalize") {
        let app_dir = args
            .get(idx + 1)
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("--finalize needs <app_dir>"))?;
        let data_dir = args
            .get(idx + 2)
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("--finalize needs <data_dir>"))?;
        let product = args
            .get(idx + 3)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("--finalize needs <product>"))?;
        let parent_pid = args.get(idx + 4).and_then(|s| s.parse::<u32>().ok());
        return stages::finalize::run(app_dir, data_dir, product, parent_pid);
    }

    let silent = args.iter().any(|a| a == "--silent" || a == "/S");
    stages::uninstall::run(silent)
}
