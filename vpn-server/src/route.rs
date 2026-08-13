use std::collections::HashMap;
use std::hash::Hash;
use std::net::Ipv4Addr;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RouteError {
    #[error("address {0} is already in use")]
    IpInUse(Ipv4Addr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evicted<H> {
    pub ip: Ipv4Addr,
    pub handle: H,
}

pub struct SessionRegistry<H: Clone + Eq + Hash> {
    by_ip: HashMap<Ipv4Addr, H>,
    by_username: HashMap<String, H>,
}

impl<H: Clone + Eq + Hash> SessionRegistry<H> {
    pub fn new() -> Self {
        Self {
            by_ip: HashMap::new(),
            by_username: HashMap::new(),
        }
    }

    pub fn insert(
        &mut self,
        username: &str,
        ip: Ipv4Addr,
        handle: H,
    ) -> Result<Option<Evicted<H>>, RouteError> {
        self.ensure_ip_not_conflicting(username, ip)?;
        let evicted = self.evict_old_session(username, ip);
        self.by_ip.insert(ip, handle.clone());
        self.by_username.insert(username.to_string(), handle);
        Ok(evicted)
    }

    fn ensure_ip_not_conflicting(&self, username: &str, ip: Ipv4Addr) -> Result<(), RouteError> {
        let Some(existing) = self.by_ip.get(&ip) else {
            return Ok(());
        };
        let belongs_to_username = self
            .by_username
            .get(username)
            .is_some_and(|h| h == existing);
        if belongs_to_username {
            Ok(())
        } else {
            Err(RouteError::IpInUse(ip))
        }
    }

    fn evict_old_session(&mut self, username: &str, fallback_ip: Ipv4Addr) -> Option<Evicted<H>> {
        let old_handle = self.by_username.remove(username)?;
        let old_ip = self
            .by_ip
            .iter()
            .find_map(|(k, v)| (v == &old_handle).then_some(*k))
            .unwrap_or(fallback_ip);
        self.by_ip.remove(&old_ip);
        Some(Evicted {
            ip: old_ip,
            handle: old_handle,
        })
    }

    pub fn lookup(&self, ip: Ipv4Addr) -> Option<&H> {
        self.by_ip.get(&ip)
    }

    pub fn lookup_by_username(&self, username: &str) -> Option<&H> {
        self.by_username.get(username)
    }

    pub fn remove_by_ip(&mut self, ip: Ipv4Addr) -> Option<H> {
        let handle = self.by_ip.remove(&ip)?;
        self.by_username.retain(|_, v| v != &handle);
        Some(handle)
    }

    pub fn remove_by_username(&mut self, username: &str) -> Option<H> {
        let handle = self.by_username.remove(username)?;
        self.by_ip.retain(|_, v| v != &handle);
        Some(handle)
    }

    pub fn remove_by_handle(&mut self, handle: &H) -> Option<(Ipv4Addr, String)> {
        let ip = self
            .by_ip
            .iter()
            .find_map(|(k, v)| (v == handle).then_some(*k))?;
        let username = self
            .by_username
            .iter()
            .find_map(|(k, v)| (v == handle).then_some(k.clone()))
            .unwrap_or_default();
        self.by_ip.remove(&ip);
        self.by_username.retain(|_, v| v != handle);
        Some((ip, username))
    }
}

impl<H: Clone + Eq + Hash> Default for SessionRegistry<H> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn ip(last: u8) -> Ipv4Addr {
        Ipv4Addr::new(10, 0, 0, last)
    }

    #[test]
    fn test_route_error_ip_in_use_display_format() {
        let err = RouteError::IpInUse(Ipv4Addr::new(10, 0, 0, 2));
        assert_eq!(format!("{err}"), "address 10.0.0.2 is already in use");
    }

    #[test]
    fn test_insert_new_session_returns_ok_none_and_indexes_hit() {
        let mut reg = SessionRegistry::<u32>::new();
        assert_eq!(reg.insert("alice", ip(2), 1), Ok(None));
        assert_eq!(reg.lookup(ip(2)), Some(&1));
        assert_eq!(reg.lookup_by_username("alice"), Some(&1));
        assert_eq!(reg.lookup(ip(9)), None);
    }

    #[test]
    fn test_insert_generic_handle_type_succeeds() {
        let mut reg = SessionRegistry::<u32>::new();
        assert_eq!(reg.insert("bob", ip(3), 7_u32), Ok(None));
        assert_eq!(reg.lookup(ip(3)), Some(&7));
    }

    #[test]
    fn test_insert_same_username_evicts_old_session() {
        let mut reg = SessionRegistry::<u32>::new();
        reg.insert("alice", ip(2), 100).unwrap();
        let result = reg.insert("alice", ip(5), 200);
        assert_eq!(
            result,
            Ok(Some(Evicted {
                ip: ip(2),
                handle: 100,
            }))
        );
        assert_eq!(reg.lookup(ip(5)), Some(&200));
        assert_eq!(reg.lookup(ip(2)), None);
        assert_eq!(reg.lookup_by_username("alice"), Some(&200));
    }

    #[test]
    fn test_insert_same_username_same_ip_renews_session() {
        let mut reg = SessionRegistry::<u32>::new();
        reg.insert("alice", ip(2), 100).unwrap();
        let result = reg.insert("alice", ip(2), 200);
        assert_eq!(
            result,
            Ok(Some(Evicted {
                ip: ip(2),
                handle: 100,
            }))
        );
        assert_eq!(reg.lookup(ip(2)), Some(&200));
        assert_eq!(reg.lookup_by_username("alice"), Some(&200));
    }

    #[test]
    fn test_insert_different_username_same_ip_returns_ip_in_use() {
        let mut reg = SessionRegistry::<u32>::new();
        reg.insert("alice", ip(2), 10).unwrap();
        let result = reg.insert("bob", ip(2), 20);
        assert_eq!(result, Err(RouteError::IpInUse(ip(2))));
        assert_eq!(reg.lookup(ip(2)), Some(&10));
        assert_eq!(reg.lookup_by_username("alice"), Some(&10));
        assert_eq!(reg.lookup_by_username("bob"), None);
    }

    #[test]
    fn test_lookup_unregistered_ip_returns_none() {
        let reg = SessionRegistry::<u32>::new();
        assert_eq!(reg.lookup(ip(9)), None);
    }

    #[test]
    fn test_lookup_by_username_unknown_returns_none() {
        let reg = SessionRegistry::<u32>::new();
        assert_eq!(reg.lookup_by_username("nobody"), None);
    }

    #[test]
    fn test_remove_by_ip_clears_both_indexes() {
        let mut reg = SessionRegistry::<u32>::new();
        reg.insert("alice", ip(2), 1).unwrap();
        assert_eq!(reg.remove_by_ip(ip(2)), Some(1));
        assert_eq!(reg.lookup(ip(2)), None);
        assert_eq!(reg.lookup_by_username("alice"), None);
    }

    #[test]
    fn test_remove_by_username_clears_both_indexes() {
        let mut reg = SessionRegistry::<u32>::new();
        reg.insert("alice", ip(2), 1).unwrap();
        assert_eq!(reg.remove_by_username("alice"), Some(1));
        assert_eq!(reg.lookup(ip(2)), None);
        assert_eq!(reg.lookup_by_username("alice"), None);
    }

    #[test]
    fn test_remove_by_handle_clears_both_indexes() {
        let mut reg = SessionRegistry::<u32>::new();
        let h = 1_u32;
        reg.insert("alice", ip(2), h).unwrap();
        let result = reg.remove_by_handle(&h);
        assert_eq!(result, Some((ip(2), "alice".to_string())));
        assert_eq!(reg.lookup(ip(2)), None);
        assert_eq!(reg.lookup_by_username("alice"), None);
    }

    #[test]
    fn test_remove_by_ip_miss_returns_none_and_unchanged() {
        let mut reg = SessionRegistry::<u32>::new();
        reg.insert("alice", ip(2), 1).unwrap();
        assert_eq!(reg.remove_by_ip(ip(9)), None);
        assert_eq!(reg.lookup(ip(2)), Some(&1));
    }

    #[test]
    fn test_remove_by_username_miss_returns_none() {
        let mut reg = SessionRegistry::<u32>::new();
        reg.insert("alice", ip(2), 1).unwrap();
        assert_eq!(reg.remove_by_username("nobody"), None);
        assert_eq!(reg.lookup(ip(2)), Some(&1));
    }

    #[test]
    fn test_remove_by_handle_miss_returns_none() {
        let mut reg = SessionRegistry::<u32>::new();
        reg.insert("alice", ip(2), 1).unwrap();
        assert_eq!(reg.remove_by_handle(&99), None);
        assert_eq!(reg.lookup(ip(2)), Some(&1));
    }

    #[test]
    fn test_default_creates_empty_registry() {
        let reg = SessionRegistry::<u32>::default();
        assert_eq!(reg.lookup(ip(2)), None);
    }
}
