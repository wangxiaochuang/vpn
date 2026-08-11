mod disk;
mod netif;
mod port;
mod process;

pub use disk::DiskCollector;
pub use netif::NetifCollector;
pub use port::PortCollector;
pub use process::ProcessFullCollector;
pub use process::ProcessSummaryCollector;

use crate::proto::ProcessEntry;

fn process_entry_from(p: &sysinfo::Process) -> ProcessEntry {
    ProcessEntry {
        pid: p.pid().as_u32(),
        name: p.name().to_string_lossy().to_string(),
        cpu_percent: p.cpu_usage(),
        mem_kb: p.memory() / 1024,
    }
}

fn sort_top_by_cpu(entries: &mut Vec<crate::proto::ProcessEntry>, limit: usize) {
    entries.sort_by(|a, b| {
        b.cpu_percent
            .partial_cmp(&a.cpu_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    entries.truncate(limit);
}

#[cfg(test)]
mod tests;
