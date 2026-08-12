//! Build script: guards the `embed-frontend` feature against a missing
//! `frontend/dist` build output, and re-runs whenever the frontend build
//! output changes.

fn main() {
    println!("cargo:rerun-if-changed=frontend/dist");
    println!("cargo:rerun-if-env-changed=SMIDR_BUILD_ID");

    if std::env::var("CARGO_FEATURE_EMBED_FRONTEND").is_ok() {
        let index = std::path::Path::new("frontend/dist/index.html");
        if !index.exists() {
            panic!(
                "frontend/dist/index.html is missing. Build the frontend first:\n    cd frontend && npm install && npm run build\nthen rebuild with --features embed-frontend."
            );
        }
    }
}
