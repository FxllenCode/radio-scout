use std::path::Path;

/// `rust-embed` (see `src/web.rs`) embeds `client/dist/` and requires the folder
/// to exist at compile time. On a fresh checkout the frontend hasn't been built
/// yet, so create an empty `client/dist/` to let the backend compile on its own
/// (it then serves a fallback page until `npm run build` fills the folder).
fn main() {
    let dist = Path::new("client/dist");
    if !dist.exists() {
        std::fs::create_dir_all(dist).expect("create client/dist for rust-embed");
    }
    println!("cargo:rerun-if-changed=client/dist");
}
