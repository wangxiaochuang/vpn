use std::time::Duration;

use async_trait::async_trait;
use sysinfo::Disks;

use crate::collector::CollectError;
use crate::collector::Collector;
use crate::proto::DiskInfo;
use crate::proto::InfoKind;
use crate::proto::InfoSnapshot;
use crate::proto::info_snapshot::Payload;

pub struct DiskCollector;

impl Default for DiskCollector {
    fn default() -> Self {
        Self
    }
}

impl DiskCollector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Collector for DiskCollector {
    fn kind(&self) -> InfoKind {
        InfoKind::DiskInfo
    }

    fn cadence(&self) -> Option<Duration> {
        None
    }

    async fn collect(&self) -> Result<InfoSnapshot, CollectError> {
        let info = tokio::task::spawn_blocking(collect_disk_blocking)
            .await
            .map_err(|e| CollectError::System(format!("join error: {e}")))?;
        Ok(InfoSnapshot {
            kind: InfoKind::DiskInfo as i32,
            payload: Some(Payload::Disks(info)),
        })
    }
}

fn collect_disk_blocking() -> DiskInfo {
    let disks = Disks::new_with_refreshed_list();
    let entries = disks
        .list()
        .iter()
        .map(|d| crate::proto::DiskEntry {
            mount_point: d.mount_point().to_string_lossy().to_string(),
            fs_type: d.file_system().to_string_lossy().to_string(),
            total_bytes: d.total_space(),
            used_bytes: d.total_space().saturating_sub(d.available_space()),
            free_bytes: d.available_space(),
        })
        .collect();
    DiskInfo { disks: entries }
}
