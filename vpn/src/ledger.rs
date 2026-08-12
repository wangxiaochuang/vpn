use std::hash::Hash;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::Mutex;

use crate::ipam::IpPool;
use crate::ipam::IpPoolError;
use crate::route::RouteError;
use crate::route::SessionRegistry;
use ipnet::Ipv4Net;

/// RAII 能力令牌：由 `ConnectionLedger::register` 在 evict 时产出，
/// 持有它证明"某个 Reserved IP 待释放"。`!Copy`、`!Clone`，构造为 `pub(crate)`。
/// 唯一释放路径：作为参数传入 `ConnectionLedger::retire`（被 move 消耗）。
#[derive(Debug)]
pub struct ReservedIp {
    pub(crate) ip: Ipv4Addr,
}

/// register 命中同名旧会话时返回。`reserved` 是旧 IP 的释放令牌，
/// 上层 SHALL 在旧 supervisor 退出时把它传入 `retire`。
#[derive(Debug)]
pub struct Evicted<H> {
    pub ip: Ipv4Addr,
    pub handle: H,
    pub reserved: ReservedIp,
}

struct LedgerInner<H: Clone + Eq + Hash> {
    pool: IpPool,
    registry: SessionRegistry<H>,
}

/// `IpPool` 与 `SessionRegistry` 的唯一并发外壳，内部单一 `std::sync::Mutex`，
/// 临界区内 SHALL NOT 出现 `.await`。`H` 在生产中是 `ConnectionHandle`，
/// 泛型化以支持 Q1 纯逻辑测试。
pub struct ConnectionLedger<H: Clone + Eq + Hash> {
    inner: Arc<Mutex<LedgerInner<H>>>,
}

impl<H: Clone + Eq + Hash> ConnectionLedger<H> {
    pub fn new(subnet: Ipv4Net) -> Result<Self, IpPoolError> {
        Ok(Self {
            inner: Arc::new(Mutex::new(LedgerInner {
                pool: IpPool::new(subnet)?,
                registry: SessionRegistry::new(),
            })),
        })
    }

    pub fn alloc(&self) -> Result<Ipv4Addr, IpPoolError> {
        self.lock().pool.alloc()
    }

    pub fn available_count(&self) -> u32 {
        self.lock().pool.available_count()
    }

    pub fn lookup_by_ip(&self, ip: Ipv4Addr) -> Option<H> {
        self.lock().registry.lookup(ip).cloned()
    }

    pub fn lookup_by_username(&self, username: &str) -> Option<H> {
        self.lock().registry.lookup_by_username(username).cloned()
    }

    pub fn register(
        &self,
        username: &str,
        ip: Ipv4Addr,
        handle: H,
    ) -> Result<Option<Evicted<H>>, RouteError> {
        let mut inner = self.lock();
        let Some(evicted) = inner.registry.insert(username, ip, handle)? else {
            return Ok(None);
        };
        reserve_evicted(&mut inner.pool, evicted.ip);
        Ok(Some(Evicted {
            ip: evicted.ip,
            handle: evicted.handle,
            reserved: ReservedIp { ip: evicted.ip },
        }))
    }

    pub fn retire(&self, handle: &H, reserved: Option<ReservedIp>) {
        let mut inner = self.lock();
        match inner.registry.remove_by_handle(handle) {
            Some((ip, _)) => {
                let _ = inner.pool.free(ip);
            }
            None => {
                if let Some(g) = reserved {
                    let _ = inner.pool.release(g.ip);
                }
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerInner<H>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn reserve_evicted(pool: &mut IpPool, ip: Ipv4Addr) {
    if let Err(e) = pool.reserve(ip) {
        tracing::error!("invariant violated: reserve {ip} after evict failed: {e}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn net29() -> Ipv4Net {
        Ipv4Net::new_assert(Ipv4Addr::new(10, 0, 0, 0), 29)
    }

    #[test]
    fn test_register_first_returns_ok_none_and_indexes_hit() {
        let ledger = ConnectionLedger::<u32>::new(net29()).unwrap();
        let allocated = ledger.alloc().unwrap();
        assert!(matches!(ledger.register("alice", allocated, 1), Ok(None)));
        assert_eq!(ledger.lookup_by_ip(allocated), Some(1));
    }

    #[test]
    fn test_register_evict_reserves_old_ip_atomically() {
        let ledger = ConnectionLedger::<u32>::new(net29()).unwrap();
        let ip2 = ledger.alloc().unwrap();
        ledger.register("alice", ip2, 1).unwrap();
        let ip3 = ledger.alloc().unwrap();
        let evicted = ledger.register("alice", ip3, 2).unwrap().expect("evict");
        assert_eq!(evicted.ip, ip2);
        assert_eq!(evicted.handle, 1);
        assert_eq!(evicted.reserved.ip, ip2);
        assert_ne!(
            ledger.alloc().unwrap(),
            ip2,
            "reserved ip must not be allocatable"
        );
    }

    #[test]
    fn test_retire_with_guard_releases_reserved_ip() {
        let ledger = ConnectionLedger::<u32>::new(net29()).unwrap();
        let ip2 = ledger.alloc().unwrap();
        ledger.register("alice", ip2, 1).unwrap();
        let ip3 = ledger.alloc().unwrap();
        let evicted = ledger.register("alice", ip3, 2).unwrap().unwrap();
        assert_eq!(ledger.available_count(), 3);
        ledger.retire(&1, Some(evicted.reserved));
        assert_eq!(ledger.available_count(), 4);
        assert_eq!(ledger.alloc().unwrap(), ip2, "released ip reallocatable");
    }

    #[test]
    fn test_retire_evicted_handle_returns_none_but_still_releases() {
        let ledger = ConnectionLedger::<u32>::new(net29()).unwrap();
        let ip2 = ledger.alloc().unwrap();
        ledger.register("alice", ip2, 1).unwrap();
        let ip3 = ledger.alloc().unwrap();
        let evicted = ledger.register("alice", ip3, 2).unwrap().unwrap();
        assert_eq!(ledger.lookup_by_ip(ip3), Some(2));
        assert_eq!(
            ledger.lookup_by_ip(ip2),
            None,
            "old ip removed from registry by evict"
        );
        ledger.retire(&evicted.handle, Some(evicted.reserved));
        assert_eq!(ledger.available_count(), 4, "reserved ip released");
        assert_eq!(
            ledger.lookup_by_ip(ip3),
            Some(2),
            "new alice unaffected by old retire"
        );
    }

    #[test]
    fn test_retire_normal_session_frees_allocated_ip() {
        let ledger = ConnectionLedger::<u32>::new(net29()).unwrap();
        let ip2 = ledger.alloc().unwrap();
        ledger.register("alice", ip2, 1).unwrap();
        assert_eq!(ledger.available_count(), 4);
        ledger.retire(&1_u32, None);
        assert_eq!(ledger.available_count(), 5);
    }

    #[test]
    fn test_register_retire_sequence_no_identity_confusion() {
        let ledger = ConnectionLedger::<u32>::new(net29()).unwrap();
        let ip2 = ledger.alloc().unwrap();
        ledger.register("alice", ip2, 1).unwrap();
        let ip5 = ledger.alloc().unwrap();
        let evicted = ledger.register("alice", ip5, 2).unwrap().unwrap();
        ledger.retire(&evicted.handle, Some(evicted.reserved));
        assert_eq!(ledger.lookup_by_ip(ip5), Some(2));
        assert_eq!(ledger.lookup_by_ip(ip2), None);
        assert_eq!(ledger.alloc().unwrap(), ip2, "old ip back to free");
        let ip3 = ledger.alloc().unwrap();
        ledger.register("bob", ip3, 3).unwrap();
        assert_eq!(ledger.lookup_by_ip(ip5), Some(2), "new alice intact");
        assert_eq!(ledger.lookup_by_ip(ip3), Some(3));
    }

    #[test]
    fn test_available_count_excludes_reserved_after_evict() {
        let ledger = ConnectionLedger::<u32>::new(net29()).unwrap();
        let ip2 = ledger.alloc().unwrap();
        ledger.register("alice", ip2, 1).unwrap();
        assert_eq!(ledger.available_count(), 4);
        let ip3 = ledger.alloc().unwrap();
        ledger.register("alice", ip3, 2).unwrap().unwrap();
        assert_eq!(ledger.available_count(), 3, ".2 reserved + .3 allocated");
    }

    // 编译期保证（文档化为编译失败用例，满足 spec session-routing guard scenarios）：
    //
    // ReservedIp 不 derive Clone/Copy，下列代码 SHALL NOT 编译：
    //
    //   fn _try_clone(g: ReservedIp) { let _ = g.clone(); }
    //
    // ReservedIp 字段为 pub(crate)，下列代码 SHALL NOT 编译（在 crate 外）：
    //
    //   let g = ReservedIp { ip: Ipv4Addr::new(10,0,0,2) };
    //
    // 这些保证使 guard 唯一可由 `register` 产出、唯一可被 `retire` 消耗。
}
