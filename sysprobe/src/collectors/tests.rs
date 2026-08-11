#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::duration_suboptimal_units
)]

use crate::collector::Collector;
use crate::collectors::DiskCollector;
use crate::collectors::NetifCollector;
use crate::collectors::PortCollector;
use crate::collectors::ProcessFullCollector;
use crate::collectors::ProcessSummaryCollector;
use crate::proto::InfoKind;
use crate::proto::info_snapshot::Payload;

#[tokio::test]
async fn test_process_summary_collect_returns_ok_with_fields() {
    let c = ProcessSummaryCollector::new();
    assert_eq!(c.kind(), InfoKind::ProcessSummary);
    assert_eq!(c.cadence(), Some(std::time::Duration::from_secs(30)));
    let snapshot = c.collect().await.unwrap();
    assert_eq!(snapshot.kind, InfoKind::ProcessSummary as i32);
    let Payload::ProcessSummary(ps) = snapshot.payload.unwrap() else {
        panic!("expected process_summary payload");
    };
    assert!(ps.count > 0);
    assert!(ps.top_by_cpu.len() <= 5);
}

#[tokio::test]
async fn test_process_full_collect_returns_ok_with_entries() {
    let c = ProcessFullCollector::new();
    assert_eq!(c.kind(), InfoKind::ProcessList);
    assert_eq!(c.cadence(), Some(std::time::Duration::from_secs(300)));
    let snapshot = c.collect().await.unwrap();
    assert_eq!(snapshot.kind, InfoKind::ProcessList as i32);
    let Payload::Processes(pl) = snapshot.payload.unwrap() else {
        panic!("expected processes payload");
    };
    assert!(!pl.processes.is_empty());
}

#[tokio::test]
async fn test_port_collect_returns_ok_does_not_panic() {
    let c = PortCollector::new();
    assert_eq!(c.kind(), InfoKind::PortList);
    assert_eq!(c.cadence(), Some(std::time::Duration::from_secs(60)));
    let snapshot = c.collect().await.unwrap();
    assert_eq!(snapshot.kind, InfoKind::PortList as i32);
    assert!(matches!(snapshot.payload, Some(Payload::Ports(_))));
}

#[tokio::test]
async fn test_netif_collect_returns_ok_with_interfaces() {
    let c = NetifCollector::new();
    assert_eq!(c.kind(), InfoKind::NetifList);
    assert_eq!(c.cadence(), Some(std::time::Duration::from_secs(600)));
    let snapshot = c.collect().await.unwrap();
    assert_eq!(snapshot.kind, InfoKind::NetifList as i32);
    let Payload::Interfaces(list) = snapshot.payload.unwrap() else {
        panic!("expected interfaces payload");
    };
    assert!(!list.interfaces.is_empty());
    assert!(list.interfaces.iter().all(|i| !i.name.is_empty()));
}

#[tokio::test]
async fn test_disk_collect_returns_ok_pull_only() {
    let c = DiskCollector::new();
    assert_eq!(c.kind(), InfoKind::DiskInfo);
    assert_eq!(c.cadence(), None);
    let snapshot = c.collect().await.unwrap();
    assert_eq!(snapshot.kind, InfoKind::DiskInfo as i32);
    let Payload::Disks(info) = snapshot.payload.unwrap() else {
        panic!("expected disks payload");
    };
    assert!(!info.disks.is_empty());
    for d in &info.disks {
        assert!(d.total_bytes >= d.used_bytes + d.free_bytes || d.total_bytes == d.used_bytes);
    }
}
