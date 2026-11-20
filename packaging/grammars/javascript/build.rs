// Export the grammar's own C entry point (`tree_sitter_javascript`) from the DLL, so that
// grammar.dll is a plain tree-sitter grammar library that Selachi (or any tree-sitter
// host) can load with libloading.
fn main() {
    println!("cargo:rustc-link-arg-cdylib=/EXPORT:tree_sitter_javascript");
}
