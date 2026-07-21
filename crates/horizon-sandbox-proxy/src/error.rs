/// Errors from standing up the allowlist proxy.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("failed to bind the proxy's loopback listener: {0}")]
    Bind(std::io::Error),

    #[error("failed to build the proxy: {0}")]
    Build(#[from] hudsucker::Error),
}
