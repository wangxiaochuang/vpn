#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::duration_suboptimal_units
)]

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;

use super::CollectError;
use super::Collector;
use super::CollectorRegistry;
use crate::proto::InfoKind;
use crate::proto::InfoSnapshot;

struct MockCollector {
    kind: InfoKind,
    cadence: Option<Duration>,
    fail: bool,
    collect_calls: AtomicUsize,
}

impl MockCollector {
    fn new(kind: InfoKind, cadence: Option<Duration>) -> Self {
        Self {
            kind,
            cadence,
            fail: false,
            collect_calls: AtomicUsize::new(0),
        }
    }

    fn failing(kind: InfoKind) -> Self {
        Self {
            kind,
            cadence: None,
            fail: true,
            collect_calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.collect_calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl Collector for MockCollector {
    fn kind(&self) -> InfoKind {
        self.kind
    }

    fn cadence(&self) -> Option<Duration> {
        self.cadence
    }

    async fn collect(&self) -> Result<InfoSnapshot, CollectError> {
        self.collect_calls.fetch_add(1, Ordering::Relaxed);
        if self.fail {
            return Err(CollectError::Io("mock failure".into()));
        }
        Ok(InfoSnapshot {
            kind: self.kind as i32,
            payload: None,
        })
    }
}

#[tokio::test]
async fn test_collector_kind_matches_collect_output_kind() {
    let c = MockCollector::new(InfoKind::ProcessSummary, Some(Duration::from_secs(30)));
    let snapshot = c.collect().await.unwrap();
    assert_eq!(snapshot.kind, InfoKind::ProcessSummary as i32);
    assert_eq!(c.kind(), InfoKind::ProcessSummary);
}

#[tokio::test]
async fn test_collector_cadence_none_signifies_pull_only() {
    let c = MockCollector::new(InfoKind::DiskInfo, None);
    assert!(c.cadence().is_none());
}

#[tokio::test]
async fn test_collector_cadence_some_signifies_periodic_push() {
    let c = MockCollector::new(InfoKind::ProcessSummary, Some(Duration::from_secs(30)));
    assert_eq!(c.cadence(), Some(Duration::from_secs(30)));
}

#[tokio::test]
async fn test_collector_collect_failure_returns_err_without_panic() {
    let c = MockCollector::failing(InfoKind::PortList);
    let result = c.collect().await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, CollectError::Io(_)));
    assert_eq!(c.calls(), 1);
}

#[test]
fn test_registry_get_after_register_returns_collector() {
    let mut reg = CollectorRegistry::new();
    reg.register(Box::new(MockCollector::new(
        InfoKind::PortList,
        Some(Duration::from_secs(60)),
    )));
    let c = reg.get(InfoKind::PortList);
    assert!(c.is_some());
    assert_eq!(c.unwrap().kind(), InfoKind::PortList);
}

#[test]
fn test_registry_get_unregistered_kind_returns_none() {
    let reg = CollectorRegistry::new();
    assert!(reg.get(InfoKind::DiskInfo).is_none());
}

#[test]
fn test_registry_duplicate_kind_overrides_previous() {
    let mut reg = CollectorRegistry::new();
    reg.register(Box::new(MockCollector::new(
        InfoKind::ProcessSummary,
        Some(Duration::from_secs(30)),
    )));
    reg.register(Box::new(MockCollector::new(
        InfoKind::ProcessSummary,
        Some(Duration::from_secs(15)),
    )));
    let kinds = reg.kinds();
    assert_eq!(kinds.len(), 1);
    let c = reg.get(InfoKind::ProcessSummary).unwrap();
    assert_eq!(c.cadence(), Some(Duration::from_secs(15)));
}

#[test]
fn test_registry_kinds_returns_all_registered() {
    let mut reg = CollectorRegistry::new();
    register_three(&mut reg);
    let mut kinds = reg.kinds();
    kinds.sort();
    assert_eq!(
        kinds,
        vec![
            InfoKind::ProcessSummary,
            InfoKind::PortList,
            InfoKind::NetifList,
        ]
    );
}

fn register_three(reg: &mut CollectorRegistry) {
    reg.register(Box::new(MockCollector::new(
        InfoKind::ProcessSummary,
        Some(Duration::from_secs(30)),
    )));
    reg.register(Box::new(MockCollector::new(
        InfoKind::PortList,
        Some(Duration::from_secs(60)),
    )));
    reg.register(Box::new(MockCollector::new(
        InfoKind::NetifList,
        Some(Duration::from_secs(600)),
    )));
}

#[tokio::test]
async fn test_collect_by_kinds_produces_snapshots_for_registered_kinds() {
    let mut reg = CollectorRegistry::new();
    reg.register(Box::new(MockCollector::new(
        InfoKind::ProcessSummary,
        Some(Duration::from_secs(30)),
    )));
    reg.register(Box::new(MockCollector::new(
        InfoKind::PortList,
        Some(Duration::from_secs(60)),
    )));
    let report = reg
        .collect_by_kinds(&[InfoKind::ProcessSummary, InfoKind::PortList])
        .await;
    assert_eq!(report.items.len(), 2);
    assert_eq!(report.items[0].kind, InfoKind::ProcessSummary as i32);
    assert_eq!(report.items[1].kind, InfoKind::PortList as i32);
    assert!(report.ts_ms > 0);
}

#[tokio::test]
async fn test_collect_by_kinds_skips_failing_collector_keeps_others() {
    let mut reg = CollectorRegistry::new();
    reg.register(Box::new(MockCollector::new(
        InfoKind::ProcessSummary,
        Some(Duration::from_secs(30)),
    )));
    reg.register(Box::new(MockCollector::failing(InfoKind::PortList)));
    let report = reg
        .collect_by_kinds(&[InfoKind::ProcessSummary, InfoKind::PortList])
        .await;
    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].kind, InfoKind::ProcessSummary as i32);
}

#[tokio::test]
async fn test_collect_by_kinds_skips_unregistered_kind() {
    let mut reg = CollectorRegistry::new();
    reg.register(Box::new(MockCollector::new(
        InfoKind::ProcessSummary,
        Some(Duration::from_secs(30)),
    )));
    let report = reg
        .collect_by_kinds(&[InfoKind::ProcessSummary, InfoKind::DiskInfo])
        .await;
    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].kind, InfoKind::ProcessSummary as i32);
}

#[tokio::test]
async fn test_collect_by_kinds_empty_kinds_returns_empty_items() {
    let mut reg = CollectorRegistry::new();
    reg.register(Box::new(MockCollector::new(
        InfoKind::ProcessSummary,
        Some(Duration::from_secs(30)),
    )));
    let report = reg.collect_by_kinds(&[]).await;
    assert!(report.items.is_empty());
    assert!(report.ts_ms > 0);
}

#[test]
fn test_push_due_before_cadence_returns_empty() {
    let t0 = Instant::now();
    let mut reg = CollectorRegistry::new();
    reg.register_at(
        Box::new(MockCollector::new(
            InfoKind::ProcessSummary,
            Some(Duration::from_secs(30)),
        )),
        t0,
    );
    let due = reg.push_due(t0 + Duration::from_secs(10));
    assert!(due.is_empty());
}

#[test]
fn test_push_due_at_cadence_returns_kind() {
    let t0 = Instant::now();
    let mut reg = CollectorRegistry::new();
    reg.register_at(
        Box::new(MockCollector::new(
            InfoKind::ProcessSummary,
            Some(Duration::from_secs(30)),
        )),
        t0,
    );
    let due = reg.push_due(t0 + Duration::from_secs(30));
    assert_eq!(due, vec![InfoKind::ProcessSummary]);
}

#[test]
fn test_push_due_after_mark_pushed_restarts_timer() {
    let t0 = Instant::now();
    let mut reg = CollectorRegistry::new();
    reg.register_at(
        Box::new(MockCollector::new(
            InfoKind::ProcessSummary,
            Some(Duration::from_secs(30)),
        )),
        t0,
    );
    reg.mark_pushed(InfoKind::ProcessSummary, t0 + Duration::from_secs(30));
    let due = reg.push_due(t0 + Duration::from_secs(50));
    assert!(due.is_empty());
}

#[test]
fn test_push_due_never_returns_pull_only_collector() {
    let t0 = Instant::now();
    let mut reg = CollectorRegistry::new();
    reg.register_at(Box::new(MockCollector::new(InfoKind::DiskInfo, None)), t0);
    let due = reg.push_due(t0 + Duration::from_hours(1));
    assert!(due.is_empty());
}
