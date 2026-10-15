//! Tracing subscriber setup (FR-019: every component logs through `tracing`).

use anyhow::Result;
use tracing_subscriber::EnvFilter;

/// Install the global tracing subscriber.
///
/// `RUST_LOG` overrides the default `lingua=info` filter.
pub fn init() -> Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("lingua=info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| {
            anyhow::anyhow!(
                "installing the tracing subscriber failed (already initialised?): {error}"
            )
        })
}
