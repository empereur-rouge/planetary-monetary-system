// pms-server/src/health.rs
use axum::{routing::get, Router};

pub async fn serve(addr: &str, ready: std::sync::Arc<std::sync::atomic::AtomicBool>) -> anyhow::Result<()> {
    use std::net::SocketAddr;
    let app = Router::new()
        .route("/live",  get(|| async { "ok" }))
        .route("/ready", get({
            let r = ready.clone();
            move || async move {
                if r.load(std::sync::atomic::Ordering::Relaxed) { "ready" } else { "starting" }
            }
        }))
        .route("/metrics", get(|| async { crate::metrics::render() }));

    let addr: SocketAddr = addr.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}