//! `rust-embed` reads `dashboard/dist` at compile time and fails if it is not
//! there. A fresh clone has no built dashboard, and `cargo check` must still
//! work — so make sure the directory exists before the macro looks for it. An
//! empty one embeds nothing, which is exactly right for a build that has not
//! run the frontend step.

fn main() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dashboard/dist");
    let _ = std::fs::create_dir_all(&dist);
    println!("cargo:rerun-if-changed=dashboard/dist");
}
