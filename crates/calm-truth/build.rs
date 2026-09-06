fn main() {
    // sqlx::migrate! tracks existing files; additions need the directory dependency.
    println!("cargo:rerun-if-changed=migrations");
}
