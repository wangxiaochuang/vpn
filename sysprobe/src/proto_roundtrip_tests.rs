#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use prost::Message;

use crate::proto::CollectRequest;
use crate::proto::InfoKind;
use crate::proto::InfoSnapshot;
use crate::proto::ProcessEntry;
use crate::proto::ProcessSummary;
use crate::proto::TelemetryMessage;
use crate::proto::TelemetryReport;
use crate::proto::info_snapshot::Payload;
use crate::proto::telemetry_message::Msg;

fn round_trip<M: Message + Default>(msg: &M) -> M {
    let buf = msg.encode_to_vec();
    M::decode(buf.as_slice()).unwrap()
}

fn sample_entry() -> ProcessEntry {
    ProcessEntry {
        pid: 1234,
        name: "chrome".into(),
        cpu_percent: 12.5,
        mem_kb: 200_000,
    }
}

#[test]
fn test_telemetry_message_report_roundtrip_preserves_payload() {
    let report = TelemetryReport {
        ts_ms: 1_700_000_000_000,
        items: vec![InfoSnapshot {
            kind: InfoKind::ProcessSummary as i32,
            payload: Some(Payload::ProcessSummary(ProcessSummary {
                count: 187,
                top_by_cpu: vec![sample_entry()],
            })),
        }],
    };
    let original = TelemetryMessage {
        msg: Some(Msg::Report(report)),
    };
    let decoded = round_trip(&original);
    assert_eq!(decoded, original);
    assert!(matches!(decoded.msg, Some(Msg::Report(_))));
}

#[test]
fn test_telemetry_message_collect_req_roundtrip_preserves_payload() {
    let req = CollectRequest {
        kinds: vec![
            InfoKind::ProcessSummary as i32,
            InfoKind::PortList as i32,
            InfoKind::DiskInfo as i32,
        ],
    };
    let original = TelemetryMessage {
        msg: Some(Msg::CollectReq(req)),
    };
    let decoded = round_trip(&original);
    assert_eq!(decoded, original);
    assert!(matches!(decoded.msg, Some(Msg::CollectReq(_))));
}

#[test]
fn test_telemetry_message_oneof_is_exclusive_after_roundtrip() {
    let original = TelemetryMessage {
        msg: Some(Msg::Report(TelemetryReport::default())),
    };
    let decoded = round_trip(&original);
    assert!(matches!(decoded.msg, Some(Msg::Report(_))));
    assert!(!matches!(decoded.msg, Some(Msg::CollectReq(_))));
}

#[test]
fn test_telemetry_report_with_multiple_items_roundtrip_preserves_order() {
    let report = TelemetryReport {
        ts_ms: 1_700_000_000_000,
        items: vec![summary_snapshot(5), ports_snapshot()],
    };
    let decoded: TelemetryReport = round_trip(&report);
    assert_eq!(decoded.ts_ms, 1_700_000_000_000);
    assert_eq!(decoded.items.len(), 2);
    assert_eq!(decoded.items[0].kind, InfoKind::ProcessSummary as i32);
    assert_eq!(decoded.items[1].kind, InfoKind::PortList as i32);
}

fn summary_snapshot(count: u32) -> InfoSnapshot {
    InfoSnapshot {
        kind: InfoKind::ProcessSummary as i32,
        payload: Some(Payload::ProcessSummary(ProcessSummary {
            count,
            top_by_cpu: vec![],
        })),
    }
}

fn ports_snapshot() -> InfoSnapshot {
    use crate::proto::PortList;
    InfoSnapshot {
        kind: InfoKind::PortList as i32,
        payload: Some(Payload::Ports(PortList { ports: vec![] })),
    }
}

#[test]
fn test_telemetry_report_with_empty_items_roundtrip_preserves_ts() {
    let report = TelemetryReport {
        ts_ms: 1,
        items: vec![],
    };
    let decoded: TelemetryReport = round_trip(&report);
    assert!(decoded.items.is_empty());
    assert_eq!(decoded.ts_ms, 1);
}

#[test]
fn test_collect_request_with_multiple_kinds_roundtrip_preserves_order() {
    let req = CollectRequest {
        kinds: vec![
            InfoKind::ProcessSummary as i32,
            InfoKind::PortList as i32,
            InfoKind::DiskInfo as i32,
        ],
    };
    let decoded: CollectRequest = round_trip(&req);
    assert_eq!(decoded.kinds.len(), 3);
    assert_eq!(decoded.kinds[0], InfoKind::ProcessSummary as i32);
    assert_eq!(decoded.kinds[1], InfoKind::PortList as i32);
    assert_eq!(decoded.kinds[2], InfoKind::DiskInfo as i32);
}

#[test]
fn test_collect_request_with_empty_kinds_roundtrip_is_empty() {
    let req = CollectRequest { kinds: vec![] };
    let decoded: CollectRequest = round_trip(&req);
    assert!(decoded.kinds.is_empty());
}

#[test]
fn test_info_snapshot_process_summary_payload_roundtrip() {
    let snapshot = InfoSnapshot {
        kind: InfoKind::ProcessSummary as i32,
        payload: Some(Payload::ProcessSummary(ProcessSummary {
            count: 187,
            top_by_cpu: vec![sample_entry()],
        })),
    };
    let decoded: InfoSnapshot = round_trip(&snapshot);
    assert_eq!(decoded.kind, InfoKind::ProcessSummary as i32);
    let Payload::ProcessSummary(ps) = decoded.payload.unwrap() else {
        panic!("expected process_summary payload");
    };
    assert_eq!(ps.count, 187);
    assert_eq!(ps.top_by_cpu.len(), 1);
    assert_eq!(ps.top_by_cpu[0], sample_entry());
}

#[test]
fn test_info_snapshot_ports_payload_roundtrip() {
    use crate::proto::{PortEntry, PortList};
    let ports = PortList {
        ports: vec![PortEntry {
            proto: "tcp".into(),
            local_addr: "0.0.0.0".into(),
            local_port: 22,
            state: "LISTEN".into(),
            pid: 999,
        }],
    };
    let snapshot = InfoSnapshot {
        kind: InfoKind::PortList as i32,
        payload: Some(Payload::Ports(ports.clone())),
    };
    let decoded: InfoSnapshot = round_trip(&snapshot);
    assert_eq!(decoded.kind, InfoKind::PortList as i32);
    let Payload::Ports(decoded_ports) = decoded.payload.unwrap() else {
        panic!("expected ports payload");
    };
    assert_eq!(decoded_ports, ports);
}

#[test]
fn test_info_snapshot_disks_payload_roundtrip() {
    use crate::proto::{DiskEntry, DiskInfo};
    let disks = DiskInfo {
        disks: vec![DiskEntry {
            mount_point: "/".into(),
            fs_type: "ext4".into(),
            total_bytes: 1_000_000,
            used_bytes: 400_000,
            free_bytes: 600_000,
        }],
    };
    let snapshot = InfoSnapshot {
        kind: InfoKind::DiskInfo as i32,
        payload: Some(Payload::Disks(disks.clone())),
    };
    let decoded: InfoSnapshot = round_trip(&snapshot);
    assert_eq!(decoded.kind, InfoKind::DiskInfo as i32);
    let Payload::Disks(decoded_disks) = decoded.payload.unwrap() else {
        panic!("expected disks payload");
    };
    assert_eq!(decoded_disks, disks);
}

#[test]
fn test_info_snapshot_processes_payload_roundtrip() {
    use crate::proto::ProcessList;
    let processes = ProcessList {
        processes: vec![sample_entry()],
    };
    let snapshot = InfoSnapshot {
        kind: InfoKind::ProcessList as i32,
        payload: Some(Payload::Processes(processes.clone())),
    };
    let decoded: InfoSnapshot = round_trip(&snapshot);
    assert_eq!(decoded.kind, InfoKind::ProcessList as i32);
    let Payload::Processes(decoded_pl) = decoded.payload.unwrap() else {
        panic!("expected processes payload");
    };
    assert_eq!(decoded_pl, processes);
}

#[test]
fn test_info_snapshot_interfaces_payload_roundtrip() {
    let interfaces = sample_netif_list();
    let snapshot = InfoSnapshot {
        kind: InfoKind::NetifList as i32,
        payload: Some(Payload::Interfaces(interfaces.clone())),
    };
    let decoded: InfoSnapshot = round_trip(&snapshot);
    assert_eq!(decoded.kind, InfoKind::NetifList as i32);
    let Payload::Interfaces(decoded_ifs) = decoded.payload.unwrap() else {
        panic!("expected interfaces payload");
    };
    assert_eq!(decoded_ifs, interfaces);
}

fn sample_netif_list() -> crate::proto::NetifList {
    use crate::proto::{NetifEntry, NetifList};
    NetifList {
        interfaces: vec![NetifEntry {
            name: "eth0".into(),
            mac: "aa:bb:cc:dd:ee:ff".into(),
            ipv4_addrs: vec!["10.0.0.1".into()],
            ipv6_addrs: vec!["fe80::1".into()],
            is_up: true,
            mtu: 1500,
        }],
    }
}
