/// Errors from standing up the allowlist proxy or its UNIX-socket bridge.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("failed to bind the proxy's loopback listener: {0}")]
    Bind(std::io::Error),

    #[error("failed to bind the UNIX-socket bridge at {path}: {source}")]
    BridgeBind {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to build the proxy: {0}")]
    Build(#[from] hudsucker::Error),
}
