fn main() {
    // Embed migration SQL files into the binary at compile time.
    // Migration files are referenced via include_str! in src/adapter/sqlite.rs.
    //
    // Trigger rebuilds when migration files change.
    println!("cargo::rerun-if-changed=migrations/");

    // Verify that migrations directory has the expected structure.
    let project_dir = std::path::Path::new("migrations/project");
    let registry_dir = std::path::Path::new("migrations/registry");

    if !project_dir.exists() {
        println!("cargo::warning=migrations/project/ directory not found");
    }
    if !registry_dir.exists() {
        println!("cargo::warning=migrations/registry/ directory not found");
    }
}
