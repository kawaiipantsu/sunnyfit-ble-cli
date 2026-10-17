use crate::config::Config;
use anyhow::{anyhow, Result};
use redis::AsyncCommands;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle, time::timeout};
#[derive(Default)]
struct Stats {
    published: AtomicU64,
    failed: AtomicU64,
}
pub struct Publisher {
    tx: mpsc::Sender<(String, String)>,
    task: JoinHandle<()>,
    stats: Arc<Stats>,
    enqueued: u64,
    dropped: u64,
}
impl Publisher {
    pub fn new(c: &Config) -> Result<Option<Self>> {
        if !c.redis_enabled || !c.sinks.iter().any(|s| s == "redis") {
            return Ok(None);
        }
        let Some(url) = &c.redis_url else {
            return Ok(None);
        };
        let client = redis::Client::open(url.as_str())
            .map_err(|_| anyhow!("invalid Redis URL (value hidden)"))?;
        let (tx, mut rx) = mpsc::channel::<(String, String)>(c.redis_queue);
        let stats = Arc::new(Stats::default());
        let s = stats.clone();
        let task = tokio::spawn(async move {
            let mut conn = None;
            let mut delay = 1u64;
            while let Some((channel, line)) = rx.recv().await {
                // Retry connection for this record; the bounded queue protects BLE acquisition.
                while conn.is_none() {
                    if let Ok(Ok(c)) = timeout(
                        Duration::from_secs(3),
                        client.get_multiplexed_async_connection(),
                    )
                    .await
                    {
                        conn = Some(c);
                        delay = 1;
                    } else {
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                        delay = (delay * 2).min(30);
                    }
                }
                let result: Result<redis::RedisResult<u64>, _> = timeout(
                    Duration::from_secs(2),
                    conn.as_mut().unwrap().publish(&channel, &line),
                )
                .await;
                if matches!(result, Ok(Ok(_))) {
                    s.published.fetch_add(1, Ordering::Relaxed);
                } else {
                    s.failed.fetch_add(1, Ordering::Relaxed);
                    conn = None;
                }
            }
        });
        Ok(Some(Self {
            tx,
            task,
            stats,
            enqueued: 0,
            dropped: 0,
        }))
    }
    pub fn enqueue(&mut self, channel: &str, row: &Value) {
        if self.tx.try_send((channel.into(), row.to_string())).is_ok() {
            self.enqueued += 1;
        } else {
            self.dropped += 1;
        }
    }
    pub async fn replay(&mut self, channel: &str, row: &Value) -> Result<()> {
        timeout(
            Duration::from_secs(10),
            self.tx.send((channel.into(), row.to_string())),
        )
        .await
        .map_err(|_| anyhow!("Redis replay stalled; source file retained"))?
        .map_err(|_| anyhow!("Redis replay worker stopped"))?;
        self.enqueued += 1;
        Ok(())
    }
    pub fn stats(&self) -> Value {
        let p = self.stats.published.load(Ordering::Relaxed);
        let f = self.stats.failed.load(Ordering::Relaxed);
        json!({"published":p,"failed_or_unconfirmed":f,"pending":self.enqueued.saturating_sub(p+f),"queue_dropped":self.dropped})
    }
    pub async fn finish(mut self) -> Value {
        drop(self.tx);
        if timeout(Duration::from_secs(5), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
            let _ = (&mut self.task).await;
        }
        let p = self.stats.published.load(Ordering::Relaxed);
        let f = self.stats.failed.load(Ordering::Relaxed);
        json!({"published":p,"failed_or_unconfirmed":f,"pending":self.enqueued.saturating_sub(p+f),"queue_dropped":self.dropped})
    }
}
