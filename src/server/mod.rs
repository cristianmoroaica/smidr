//! Axum-based web server: `--web` mode entry point.
//!
//! Holds a `ServerState` with lazily-created `AppCore` instances keyed by
//! project id, and exposes the project REST API (Task 2.1) plus (in a later
//! task) the WebSocket session channel.

pub mod assets;
pub mod routes;
pub mod ws;

use crate::config::Config;
use crate::core::AppCore;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Server-wide state shared across all handlers.
pub struct ServerState {
    pub config: Config,
    pub cores: HashMap<String, AppCore>,
}

/// Shared handle passed to axum via `State`.
pub type SharedState = Arc<Mutex<ServerState>>;

impl ServerState {
    /// Look up the cached `AppCore` for `project_id`, lazily creating one
    /// (with no briefing) if this is the first time it's been opened.
    ///
    /// The project REST routes read/write storage directly; this is the seam
    /// the WebSocket session channel (`server::ws`) uses to get a live core.
    pub fn core_for(&mut self, project_id: &str) -> Result<&mut AppCore, String> {
        if !self.cores.contains_key(project_id) {
            // Only cache cores for projects that exist on disk — otherwise
            // arbitrary ids from unauthenticated requests grow the map (and
            // its AppCore instances) without bound.
            let exists = crate::storage::project::list_projects()?
                .iter()
                .any(|p| p.path.file_name().is_some_and(|n| n.to_string_lossy() == project_id));
            if !exists {
                return Err(format!("unknown project: {project_id}"));
            }
            let core = AppCore::new(self.config.clone(), None)?;
            self.cores.insert(project_id.to_string(), core);
        }
        Ok(self.cores.get_mut(project_id).expect("just inserted"))
    }
}

/// Build the axum router and serve it on `127.0.0.1:{port}` (port 0 = an
/// ephemeral port chosen by the OS). Blocks until the server exits.
///
/// `on_bound` is invoked exactly once, right after the listener is bound,
/// with the actual local address (useful for tests and for printing the
/// `listening on ...` line callers parse).
pub fn run_blocking(
    config: Config,
    port: u16,
    on_bound: impl FnOnce(std::net::SocketAddr),
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to build tokio runtime: {e}"))?;

    runtime.block_on(async move {
        let state: SharedState = Arc::new(Mutex::new(ServerState {
            config,
            cores: HashMap::new(),
        }));

        let app = routes::router(state);

        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("Failed to bind {addr}: {e}"))?;

        let local_addr = listener
            .local_addr()
            .map_err(|e| format!("Failed to read local addr: {e}"))?;
        on_bound(local_addr);

        axum::serve(listener, app)
            .await
            .map_err(|e| format!("Server error: {e}"))
    })
}
