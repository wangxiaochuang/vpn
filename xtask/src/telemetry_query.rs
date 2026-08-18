use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use anyhow::bail;
use clap::Parser;
use prost::Message as _;
use sysprobe::proto::InfoKind;
use sysprobe::proto::InfoSnapshot;
use vpn_server::db::TelemetryFilter;
use vpn_server::db::TelemetryRow;
use vpn_server::db::TelemetryStore;
use vpn_server::db::open_telemetry_store;

use crate::users::read_telemetry_db_url;

#[derive(Parser)]
pub struct Args {
    #[arg(long, default_value = "server.toml")]
    pub config: PathBuf,
    #[arg(long)]
    pub user: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub since: Option<i64>,
    #[arg(long)]
    pub until: Option<i64>,
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
    #[arg(long)]
    pub details: bool,
}

const KIND_NAMES: [(&str, InfoKind); 5] = [
    ("PROCESS_SUMMARY", InfoKind::ProcessSummary),
    ("PROCESS_LIST", InfoKind::ProcessList),
    ("PORT_LIST", InfoKind::PortList),
    ("NETIF_LIST", InfoKind::NetifList),
    ("DISK_INFO", InfoKind::DiskInfo),
];

pub fn parse_kind(name: &str) -> anyhow::Result<InfoKind> {
    let Some((_, kind)) = KIND_NAMES.iter().find(|(n, _)| *n == name) else {
        let valid: Vec<&str> = KIND_NAMES.iter().map(|(n, _)| *n).collect();
        bail!("invalid kind '{name}', valid values: {}", valid.join(", "))
    };
    Ok(*kind)
}

pub fn kind_name(kind: i32) -> &'static str {
    KIND_NAMES
        .iter()
        .find(|(_, k)| *k as i32 == kind)
        .map_or("UNKNOWN", |(n, _)| *n)
}

pub struct TelemetryCli {
    store: Arc<dyn TelemetryStore>,
}

impl TelemetryCli {
    pub async fn open(config_path: &Path) -> anyhow::Result<Self> {
        let db = read_telemetry_db_url(config_path)?;
        let store = open_telemetry_store(&db)
            .await
            .with_context(|| format!("failed to open database {db}"))?;
        Ok(Self { store })
    }

    pub async fn query(
        &self,
        args: &Args,
        kind: Option<InfoKind>,
    ) -> anyhow::Result<Vec<TelemetryRow>> {
        let filter = TelemetryFilter {
            username: args.user.clone(),
            kind,
            since_ms: args.since,
            until_ms: args.until,
            limit: args.limit,
        };
        Ok(self.store.query(&filter).await?)
    }
}

pub async fn run(args: &Args) -> anyhow::Result<()> {
    let kind = args.kind.as_deref().map(parse_kind).transpose()?;
    let cli = TelemetryCli::open(&args.config).await?;
    let rows = cli.query(args, kind).await?;
    print_report(&rows, args.details);
    Ok(())
}

fn print_report(rows: &[TelemetryRow], details: bool) {
    if rows.is_empty() {
        println!("no matching telemetry records");
        return;
    }
    for row in rows {
        println!("{}", format_summary(row));
        if details {
            println!("  {}", format_details(&row.payload));
        }
    }
}

fn format_summary(row: &TelemetryRow) -> String {
    format!(
        "{} user={} kind={} session={}",
        format_ms(row.received_at_ms),
        row.username,
        kind_name(row.kind),
        row.session_id,
    )
}

fn format_details(payload: &[u8]) -> String {
    match InfoSnapshot::decode(payload) {
        Ok(snapshot) => format!("{snapshot:?}"),
        Err(e) => format!("payload decode failed: {e}"),
    }
}

pub fn format_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let tod = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{millis:03}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
    )
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if month <= 2 {
            yoe + era * 400 + 1
        } else {
            yoe + era * 400
        },
        month,
        day,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_args_defaults() {
        let args = Args::try_parse_from(["xtask"]).unwrap();
        assert_eq!(args.config, PathBuf::from("server.toml"));
        assert_eq!(args.limit, 50);
        assert!(!args.details);
        assert!(args.user.is_none());
        assert!(args.kind.is_none());
        assert!(args.since.is_none());
        assert!(args.until.is_none());
    }

    #[test]
    fn test_args_parses_all_filters() {
        let args = Args::try_parse_from([
            "xtask",
            "--config",
            "other.toml",
            "--user",
            "alice",
            "--kind",
            "DISK_INFO",
            "--since",
            "100",
            "--until",
            "200",
            "--limit",
            "5",
            "--details",
        ])
        .unwrap();
        assert_eq!(args.config, PathBuf::from("other.toml"));
        assert_eq!(args.user.as_deref(), Some("alice"));
        assert_eq!(args.kind.as_deref(), Some("DISK_INFO"));
        assert_eq!(args.since, Some(100));
        assert_eq!(args.until, Some(200));
        assert_eq!(args.limit, 5);
        assert!(args.details);
    }

    #[test]
    fn test_parse_kind_maps_each_name() {
        assert_eq!(
            parse_kind("PROCESS_SUMMARY").unwrap(),
            InfoKind::ProcessSummary
        );
        assert_eq!(parse_kind("PROCESS_LIST").unwrap(), InfoKind::ProcessList);
        assert_eq!(parse_kind("PORT_LIST").unwrap(), InfoKind::PortList);
        assert_eq!(parse_kind("NETIF_LIST").unwrap(), InfoKind::NetifList);
        assert_eq!(parse_kind("DISK_INFO").unwrap(), InfoKind::DiskInfo);
    }

    #[test]
    fn test_parse_kind_invalid_lists_valid_values() {
        let err = parse_kind("NOT_A_KIND").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("NOT_A_KIND"));
        for name in KIND_NAMES.iter().map(|(n, _)| *n) {
            assert!(msg.contains(name));
        }
    }

    #[test]
    fn test_kind_name_maps_each_value() {
        for (name, kind) in KIND_NAMES {
            assert_eq!(kind_name(kind as i32), name);
        }
        assert_eq!(kind_name(999), "UNKNOWN");
    }

    #[test]
    fn test_format_ms_known_timestamps() {
        assert_eq!(format_ms(0), "1970-01-01 00:00:00.000");
        assert_eq!(format_ms(1_700_000_000_123), "2023-11-14 22:13:20.123");
        assert_eq!(format_ms(-1), "1969-12-31 23:59:59.999");
    }

    #[test]
    fn test_format_details_decodes_snapshot() {
        let snapshot = InfoSnapshot {
            kind: InfoKind::ProcessSummary as i32,
            payload: None,
        };
        let text = format_details(&snapshot.encode_to_vec());
        assert!(text.contains("kind: ProcessSummary"));
    }

    #[test]
    fn test_format_details_decode_failure_shows_placeholder() {
        let text = format_details(&[0xff, 0xff]);
        assert!(text.contains("decode failed"));
    }
}
