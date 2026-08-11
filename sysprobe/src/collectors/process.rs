use std::time::Duration;

use async_trait::async_trait;
use sysinfo::ProcessRefreshKind;
use sysinfo::ProcessesToUpdate;
use sysinfo::System;

use super::process_entry_from;
use super::sort_top_by_cpu;
use crate::collector::CollectError;
use crate::collector::Collector;
use crate::proto::InfoKind;
use crate::proto::InfoSnapshot;
use crate::proto::ProcessList;
use crate::proto::ProcessSummary;
use crate::proto::info_snapshot::Payload;

const SUMMARY_TOP_N: usize = 5;
const SUMMARY_CADENCE: Duration = Duration::from_secs(30);
const FULL_CADENCE: Duration = Duration::from_mins(5);

pub struct ProcessSummaryCollector;

impl Default for ProcessSummaryCollector {
    fn default() -> Self {
        Self
    }
}

impl ProcessSummaryCollector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Collector for ProcessSummaryCollector {
    fn kind(&self) -> InfoKind {
        InfoKind::ProcessSummary
    }

    fn cadence(&self) -> Option<Duration> {
        Some(SUMMARY_CADENCE)
    }

    async fn collect(&self) -> Result<InfoSnapshot, CollectError> {
        let summary = tokio::task::spawn_blocking(collect_summary_blocking)
            .await
            .map_err(|e| CollectError::System(format!("join error: {e}")))?;
        Ok(InfoSnapshot {
            kind: InfoKind::ProcessSummary as i32,
            payload: Some(Payload::ProcessSummary(summary)),
        })
    }
}

fn collect_summary_blocking() -> ProcessSummary {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_memory().with_cpu(),
    );
    let mut entries: Vec<_> = sys.processes().values().map(process_entry_from).collect();
    sort_top_by_cpu(&mut entries, SUMMARY_TOP_N);
    ProcessSummary {
        count: u32::try_from(sys.processes().len()).unwrap_or(u32::MAX),
        top_by_cpu: entries,
    }
}

pub struct ProcessFullCollector;

impl Default for ProcessFullCollector {
    fn default() -> Self {
        Self
    }
}

impl ProcessFullCollector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Collector for ProcessFullCollector {
    fn kind(&self) -> InfoKind {
        InfoKind::ProcessList
    }

    fn cadence(&self) -> Option<Duration> {
        Some(FULL_CADENCE)
    }

    async fn collect(&self) -> Result<InfoSnapshot, CollectError> {
        let list = tokio::task::spawn_blocking(collect_full_blocking)
            .await
            .map_err(|e| CollectError::System(format!("join error: {e}")))?;
        Ok(InfoSnapshot {
            kind: InfoKind::ProcessList as i32,
            payload: Some(Payload::Processes(list)),
        })
    }
}

fn collect_full_blocking() -> ProcessList {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_memory().with_cpu(),
    );
    let processes: Vec<_> = sys.processes().values().map(process_entry_from).collect();
    ProcessList { processes }
}
