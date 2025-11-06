use anyhow::Result;
use redis::{aio::MultiplexedConnection, AsyncCommands};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct RedisStore {
    inner: Arc<Mutex<MultiplexedConnection>>,
}

static SAMPLE_COUNTER: AtomicU64 = AtomicU64::new(1);

impl RedisStore {
    pub async fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url)?;
        let conn = client.get_multiplexed_async_connection().await?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    /// Adds a value to a time-series emulated via sorted sets, trimming by window_ns.
    pub async fn ts_add(&self, key: &str, ts_ns: u64, val: f64, window_ns: u64) -> Result<()> {
        let member = format!(
            "{}:{}:{}",
            ts_ns,
            val,
            SAMPLE_COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let mut conn = self.inner.lock().await;
        conn.zadd::<_, _, _, ()>(key, member, ts_ns as f64).await?;
        if window_ns > 0 {
            let cutoff = ts_ns.saturating_sub(window_ns);
            let mut trim_cmd = redis::cmd("ZREMRANGEBYSCORE");
            trim_cmd.arg(key).arg("-inf").arg(cutoff as f64);
            trim_cmd.query_async::<_, ()>(&mut *conn).await?;
        }
        Ok(())
    }

    /// Returns most recent values since `since_ns`, limited to `limit` entries.
    pub async fn ts_recent(
        &self,
        key: &str,
        since_ns: u64,
        limit: usize,
    ) -> Result<Vec<(u64, f64)>> {
        let mut conn = self.inner.lock().await;
        let mut cmd = redis::cmd("ZREVRANGEBYSCORE");
        cmd.arg(key)
            .arg("+inf")
            .arg(since_ns as f64)
            .arg("WITHSCORES");
        if limit > 0 {
            cmd.arg("LIMIT").arg(0).arg(limit);
        }
        let mut entries: Vec<(String, f64)> = cmd.query_async(&mut *conn).await?;
        entries.reverse();
        Ok(entries
            .into_iter()
            .filter_map(|(member, score)| {
                parse_member_value(&member).map(|val| (score as u64, val))
            })
            .collect())
    }

    pub async fn ts_last(&self, key: &str) -> Result<Option<(u64, f64)>> {
        let mut conn = self.inner.lock().await;
        let mut cmd = redis::cmd("ZREVRANGE");
        cmd.arg(key).arg(0).arg(0).arg("WITHSCORES");
        let entries: Vec<(String, f64)> = cmd.query_async(&mut *conn).await?;
        Ok(entries
            .into_iter()
            .next()
            .and_then(|(member, score)| parse_member_value(&member).map(|v| (score as u64, v))))
    }

    pub async fn hm_set(&self, key: &str, fields: &[(&str, f64)]) -> Result<()> {
        if fields.is_empty() {
            return Ok(());
        }
        let mut conn = self.inner.lock().await;
        let mut cmd = redis::cmd("HSET");
        cmd.arg(key);
        for (field, value) in fields {
            cmd.arg(*field).arg(value.to_string());
        }
        cmd.query_async::<_, ()>(&mut *conn).await?;
        Ok(())
    }

    pub async fn hm_get_all(&self, key: &str) -> Result<HashMap<String, String>> {
        let mut conn = self.inner.lock().await;
        let map: HashMap<String, String> = conn.hgetall(key).await?;
        Ok(map)
    }
}

fn parse_member_value(member: &str) -> Option<f64> {
    member.split(':').nth(1).and_then(|s| s.parse::<f64>().ok())
}
