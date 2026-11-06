//! JSON Schema of the boundary types (spec §13), generated from the Rust types.
//!
//! `cargo test -p coffee-core --features schema --test schema` checks that `schema/*.json` is up
//! to date; set `SCHEMA_WRITE=1` to regenerate the files.
#![cfg(feature = "schema")]

use std::path::PathBuf;

fn check(name: &str, schema: schemars::Schema) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema").join(format!("{name}.schema.json"));
    let text = serde_json::to_string_pretty(&schema).unwrap() + "\n";
    if std::env::var("SCHEMA_WRITE").is_ok() {
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        current == text,
        "{} is out of date: run SCHEMA_WRITE=1 cargo test -p coffee-core --features schema --test schema",
        path.display()
    );
}

#[test]
fn schemas_are_up_to_date() {
    check("model", schemars::schema_for!(coffee_core::OfficeDocument));
    check("limits", schemars::schema_for!(coffee_core::Limits));
    check("error", schemars::schema_for!(coffee_core::Error));
}
