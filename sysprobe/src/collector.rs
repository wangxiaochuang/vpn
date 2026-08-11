use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use thiserror::Error;

use crate::proto::InfoKind;
use crate::proto::InfoSnapshot;
use crate::proto::TelemetryReport;

#[derive(Debug, Error)]
pub enum CollectError {
    #[error("I/O error during collection: {0}")]
    Io(String),
    #[error("system API error during collection: {0}")]
    System(String),
    #[error("collection not supported on this platform: {0}")]
    NotSupported(String),
}

#[async_trait]
pub trait Collector: Send + Sync {
    fn kind(&self) -> InfoKind;
    fn cadence(&self) -> Option<Duration>;
    async fn collect(&self) -> Result<InfoSnapshot, CollectError>;
}

#[derive(Default)]
pub struct CollectorRegistry {
    collectors: HashMap<InfoKind, Box<dyn Collector>>,
    last_push: HashMap<InfoKind, Instant>,
}

impl CollectorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, collector: Box<dyn Collector>) {
        self.register_at(collector, Instant::now());
    }

    pub fn register_at(&mut self, collector: Box<dyn Collector>, now: Instant) {
        let kind = collector.kind();
        self.last_push.insert(kind, now);
        self.collectors.insert(kind, collector);
    }

    pub fn kinds(&self) -> Vec<InfoKind> {
        self.collectors.keys().copied().collect()
    }

    pub fn get(&self, kind: InfoKind) -> Option<&dyn Collector> {
        self.collectors.get(&kind).map(|c| &**c)
    }

    pub async fn collect_by_kinds(&self, kinds: &[InfoKind]) -> TelemetryReport {
        let mut items = Vec::new();
        for &kind in kinds {
            if let Some(c) = self.collectors.get(&kind)
                && let Ok(snapshot) = c.collect().await
            {
                items.push(snapshot);
            }
        }
        TelemetryReport {
            ts_ms: epoch_ms(),
            items,
        }
    }

    pub fn push_due(&self, now: Instant) -> Vec<InfoKind> {
        self.collectors
            .iter()
            .filter_map(|(kind, c)| {
                let cadence = c.cadence()?;
                let last = self.last_push.get(kind).copied()?;
                (now >= last + cadence).then_some(*kind)
            })
            .collect()
    }

    pub fn mark_pushed(&mut self, kind: InfoKind, now: Instant) {
        self.last_push.insert(kind, now);
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or_default())
        .unwrap_or_default()
}

#[cfg(test)]
mod collector_contract_tests;
