fn main() {
    // Embed migration SQL files into the binary at compile time.
    // Migration files are referenced via include_str! in
    // crates/carryctx-sqlite/src/database.rs.
    //
    // Trigger rebuilds when migration files change.
    println!("cargo::rerun-if-changed=crates/carryctx-sqlite/migrations/");

    // Verify that migrations directory has the expected structure.
    let project_dir = std::path::Path::new("crates/carryctx-sqlite/migrations/project");

    if !project_dir.exists() {
        println!("cargo::warning=crates/carryctx-sqlite/migrations/project/ directory not found");
    }
}
