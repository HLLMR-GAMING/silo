//! Tests for the organize engine, split to match its modules. Shared fixtures live here.

use super::*;

mod adopt;
mod engine;
mod foldercopy;
mod foreign;
mod fsops;
mod layout;
mod links;
mod relink;
mod remove;
mod volumes;

/// Record `FS25_<tech>.zip` as organized under `category` in `root`'s manifest.
fn organize_row(conn: &Connection, root: &Path, tech: &str, category: &str, active: bool) {
    let key = crate::modsroot::canonical_key(root);
    db::upsert_organized(
        conn,
        &key,
        &OrganizedRow {
            tech_name: tech.into(),
            file_name: format!("{tech}.zip"),
            kind: "zip".into(),
            category: category.into(),
            subcategory: None,
            active,
        },
    )
    .unwrap();
}

fn temp_dir_for(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("silo_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}
