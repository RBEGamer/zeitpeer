mod config;
mod net;
mod phc;
mod store;
mod telemetry;
mod time;
mod util;

use config::{ensure_defaults, load};
use phc::reader::PhcReader;
use store::redis::RedisStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();
    let path = std::env::var("CONFIG").unwrap_or_else(|_| "config/peer1.toml".into());
    let mut cfg = load(&path)?;
    ensure_defaults(&mut cfg);
    let redis = RedisStore::connect(&cfg.redis_url).await?;
    let phc = PhcReader::new(cfg.phc_device.clone());

    let node = net::node::run_node(cfg.clone(), redis.clone(), phc.clone());
    let telem = telemetry::metrics::serve(cfg, redis);
    tokio::try_join!(node, telem)?;
    Ok(())
}
