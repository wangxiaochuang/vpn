use std::collections::HashSet;
use std::net::Ipv4Addr;

use ipnet::Ipv4Net;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IpPoolError {
    #[error("invalid subnet: no allocatable addresses after reservations")]
    InvalidSubnet,
    #[error("pool exhausted: no free address available")]
    PoolExhausted,
    #[error("address {0} is out of pool or reserved")]
    OutOfPool(Ipv4Addr),
    #[error("address {0} is not allocated")]
    NotAllocated(Ipv4Addr),
    #[error("address {0} is not reserved")]
    NotReserved(Ipv4Addr),
}

#[derive(Debug, Clone)]
pub struct IpPool {
    network: u32,
    total: u32,
    bits: Vec<u64>,
    reserved: HashSet<Ipv4Addr>,
}

fn build_reserved_bits(total: u32) -> Vec<u64> {
    let broadcast_off = total - 1;
    let word_count = total.div_ceil(64);
    (0..word_count)
        .map(|w| build_reserved_word(w * 64, total, broadcast_off))
        .collect()
}

fn build_reserved_word(base: u32, total: u32, broadcast_off: u32) -> u64 {
    let mut word = 0u64;
    for bit in 0..64u32 {
        let off = base + bit;
        if off >= total {
            word |= u64::MAX << bit;
            break;
        }
        if off == 0 || off == 1 || off == broadcast_off {
            word |= 1u64 << bit;
        }
    }
    word
}

impl IpPool {
    pub fn new(subnet: Ipv4Net) -> Result<Self, IpPoolError> {
        let prefix = subnet.prefix_len();
        if !(1..=30).contains(&prefix) {
            return Err(IpPoolError::InvalidSubnet);
        }
        let total = 1u32 << (32u8 - prefix);
        let network = u32::from(subnet.network());
        let bits = build_reserved_bits(total);
        Ok(Self {
            network,
            total,
            bits,
            reserved: HashSet::new(),
        })
    }

    pub fn available_count(&self) -> u32 {
        self.bits.iter().map(|w| w.count_zeros()).sum()
    }

    pub fn alloc(&mut self) -> Result<Ipv4Addr, IpPoolError> {
        let mut base = 0u32;
        for word in &mut self.bits {
            if *word != u64::MAX {
                let bit = (!*word).trailing_zeros();
                *word |= 1u64 << bit;
                return Ok(Ipv4Addr::from(self.network + base + bit));
            }
            base += u64::BITS;
        }
        Err(IpPoolError::PoolExhausted)
    }

    pub fn free(&mut self, addr: Ipv4Addr) -> Result<(), IpPoolError> {
        let Some(offset) = self.pool_offset(addr) else {
            return Err(IpPoolError::OutOfPool(addr));
        };
        if self.reserved.contains(&addr) {
            return Err(IpPoolError::NotAllocated(addr));
        }
        let word_idx = (offset / u64::BITS) as usize;
        let mask = 1u64 << (offset % u64::BITS);
        #[allow(clippy::indexing_slicing)]
        let word = &mut self.bits[word_idx];
        if *word & mask == 0 {
            return Err(IpPoolError::NotAllocated(addr));
        }
        *word &= !mask;
        Ok(())
    }

    pub fn reserve(&mut self, addr: Ipv4Addr) -> Result<(), IpPoolError> {
        if self.pool_offset(addr).is_none() {
            return Err(IpPoolError::OutOfPool(addr));
        }
        if !self.is_bit_set(addr) || self.reserved.contains(&addr) {
            return Err(IpPoolError::NotAllocated(addr));
        }
        self.reserved.insert(addr);
        Ok(())
    }

    pub fn release(&mut self, addr: Ipv4Addr) -> Result<(), IpPoolError> {
        if !self.reserved.remove(&addr) {
            return Err(IpPoolError::NotReserved(addr));
        }
        self.clear_bit(addr);
        Ok(())
    }

    #[allow(clippy::indexing_slicing)]
    fn is_bit_set(&self, addr: Ipv4Addr) -> bool {
        let Some(offset) = self.pool_offset(addr) else {
            return false;
        };
        let word_idx = (offset / u64::BITS) as usize;
        let mask = 1u64 << (offset % u64::BITS);
        (self.bits[word_idx] & mask) != 0
    }

    #[allow(clippy::indexing_slicing)]
    fn clear_bit(&mut self, addr: Ipv4Addr) {
        let Some(offset) = self.pool_offset(addr) else {
            return;
        };
        let word_idx = (offset / u64::BITS) as usize;
        let mask = 1u64 << (offset % u64::BITS);
        self.bits[word_idx] &= !mask;
    }

    fn pool_offset(&self, addr: Ipv4Addr) -> Option<u32> {
        let off = u32::from(addr).wrapping_sub(self.network);
        if off >= self.total || off == 0 || off == 1 || off == self.total - 1 {
            return None;
        }
        Some(off)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn net(a: u8, b: u8, c: u8, d: u8, prefix: u8) -> Ipv4Net {
        Ipv4Net::new_assert(Ipv4Addr::new(a, b, c, d), prefix)
    }

    #[test]
    fn test_ip_pool_new_when_prefix_31_returns_invalid_subnet() {
        let err = IpPool::new(net(10, 0, 0, 0, 31)).unwrap_err();
        assert_eq!(err, IpPoolError::InvalidSubnet);
    }

    #[test]
    fn test_ip_pool_new_when_prefix_32_returns_invalid_subnet() {
        let err = IpPool::new(net(10, 0, 0, 0, 32)).unwrap_err();
        assert_eq!(err, IpPoolError::InvalidSubnet);
    }

    #[test]
    fn test_ip_pool_new_when_24_available_count_is_253() {
        let pool = IpPool::new(net(10, 0, 0, 0, 24)).expect("valid /24");
        assert_eq!(pool.available_count(), 253);
    }

    #[test]
    fn test_ip_pool_available_count_when_alloc_then_free_restores() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 24)).expect("valid /24");
        assert_eq!(pool.available_count(), 253);
        let a = pool.alloc().expect("alloc");
        assert_eq!(pool.available_count(), 252);
        pool.free(a).expect("free");
        assert_eq!(pool.available_count(), 253);
    }

    #[test]
    fn test_ip_pool_alloc_when_24_first_returns_gateway_next() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 24)).expect("valid /24");
        assert_eq!(pool.alloc(), Ok(Ipv4Addr::new(10, 0, 0, 2)));
    }

    #[test]
    fn test_ip_pool_alloc_when_successive_returns_ascending() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 24)).expect("valid /24");
        assert_eq!(pool.alloc(), Ok(Ipv4Addr::new(10, 0, 0, 2)));
        assert_eq!(pool.alloc(), Ok(Ipv4Addr::new(10, 0, 0, 3)));
        assert_eq!(pool.alloc(), Ok(Ipv4Addr::new(10, 0, 0, 4)));
    }

    #[test]
    fn test_ip_pool_alloc_when_exhausted_returns_pool_exhausted() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 30)).expect("valid /30");
        assert_eq!(pool.available_count(), 1);
        assert_eq!(pool.alloc(), Ok(Ipv4Addr::new(10, 0, 0, 2)));
        assert_eq!(pool.available_count(), 0);
        assert_eq!(pool.alloc(), Err(IpPoolError::PoolExhausted));
        assert_eq!(pool.available_count(), 0);
    }

    #[test]
    fn test_ip_pool_free_then_alloc_returns_freed_address() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 24)).expect("valid /24");
        assert_eq!(pool.alloc(), Ok(Ipv4Addr::new(10, 0, 0, 2)));
        assert_eq!(pool.alloc(), Ok(Ipv4Addr::new(10, 0, 0, 3)));
        assert_eq!(pool.alloc(), Ok(Ipv4Addr::new(10, 0, 0, 4)));
        pool.free(Ipv4Addr::new(10, 0, 0, 3)).expect("free .3");
        assert_eq!(pool.alloc(), Ok(Ipv4Addr::new(10, 0, 0, 3)));
    }

    #[test]
    fn test_ip_pool_free_when_out_of_subnet_returns_out_of_pool() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 24)).expect("valid /24");
        assert_eq!(
            pool.free(Ipv4Addr::new(10, 0, 1, 5)),
            Err(IpPoolError::OutOfPool(Ipv4Addr::new(10, 0, 1, 5)))
        );
    }

    #[test]
    fn test_ip_pool_free_when_unallocated_returns_not_allocated() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 24)).expect("valid /24");
        assert_eq!(
            pool.free(Ipv4Addr::new(10, 0, 0, 5)),
            Err(IpPoolError::NotAllocated(Ipv4Addr::new(10, 0, 0, 5)))
        );
    }

    #[test]
    fn test_ip_pool_free_when_gateway_returns_out_of_pool() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 24)).expect("valid /24");
        assert_eq!(
            pool.free(Ipv4Addr::new(10, 0, 0, 1)),
            Err(IpPoolError::OutOfPool(Ipv4Addr::new(10, 0, 0, 1)))
        );
    }

    #[test]
    fn test_ip_pool_free_when_broadcast_returns_out_of_pool() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 24)).expect("valid /24");
        assert_eq!(
            pool.free(Ipv4Addr::new(10, 0, 0, 255)),
            Err(IpPoolError::OutOfPool(Ipv4Addr::new(10, 0, 0, 255)))
        );
    }

    #[test]
    fn test_ip_pool_reserve_then_alloc_skips_reserved_address() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 29)).expect("valid /29");
        let a = pool.alloc().expect("alloc .2");
        assert_eq!(a, Ipv4Addr::new(10, 0, 0, 2));
        pool.reserve(a).expect("reserve .2");
        let mut got = Vec::new();
        while let Ok(ip) = pool.alloc() {
            got.push(ip);
        }
        assert!(!got.contains(&Ipv4Addr::new(10, 0, 0, 2)));
        assert_eq!(
            got,
            vec![
                Ipv4Addr::new(10, 0, 0, 3),
                Ipv4Addr::new(10, 0, 0, 4),
                Ipv4Addr::new(10, 0, 0, 5),
                Ipv4Addr::new(10, 0, 0, 6),
            ]
        );
    }

    #[test]
    fn test_ip_pool_release_then_alloc_returns_released_address() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 29)).expect("valid /29");
        let a = pool.alloc().expect("alloc");
        pool.reserve(a).expect("reserve");
        pool.release(a).expect("release");
        assert_eq!(pool.alloc(), Ok(Ipv4Addr::new(10, 0, 0, 2)));
    }

    #[test]
    fn test_ip_pool_available_count_excludes_reserved() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 29)).expect("valid /29");
        let a = pool.alloc().expect("alloc .2");
        let _b = pool.alloc().expect("alloc .3");
        pool.reserve(a).expect("reserve .2");
        assert_eq!(pool.available_count(), 3);
    }

    #[test]
    fn test_ip_pool_reserve_when_free_returns_not_allocated() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 29)).expect("valid /29");
        assert_eq!(
            pool.reserve(Ipv4Addr::new(10, 0, 0, 5)),
            Err(IpPoolError::NotAllocated(Ipv4Addr::new(10, 0, 0, 5)))
        );
    }

    #[test]
    fn test_ip_pool_reserve_when_already_reserved_returns_not_allocated() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 29)).expect("valid /29");
        let a = pool.alloc().expect("alloc");
        pool.reserve(a).expect("first reserve");
        assert_eq!(pool.reserve(a), Err(IpPoolError::NotAllocated(a)));
    }

    #[test]
    fn test_ip_pool_reserve_out_of_pool_returns_out_of_pool() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 29)).expect("valid /29");
        assert_eq!(
            pool.reserve(Ipv4Addr::new(10, 0, 0, 1)),
            Err(IpPoolError::OutOfPool(Ipv4Addr::new(10, 0, 0, 1)))
        );
    }

    #[test]
    fn test_ip_pool_release_when_allocated_returns_not_reserved() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 29)).expect("valid /29");
        let a = pool.alloc().expect("alloc");
        assert_eq!(pool.release(a), Err(IpPoolError::NotReserved(a)));
    }

    #[test]
    fn test_ip_pool_release_when_free_returns_not_reserved() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 29)).expect("valid /29");
        assert_eq!(
            pool.release(Ipv4Addr::new(10, 0, 0, 5)),
            Err(IpPoolError::NotReserved(Ipv4Addr::new(10, 0, 0, 5)))
        );
    }

    #[test]
    fn test_ip_pool_free_when_reserved_returns_not_allocated() {
        let mut pool = IpPool::new(net(10, 0, 0, 0, 29)).expect("valid /29");
        let a = pool.alloc().expect("alloc");
        pool.reserve(a).expect("reserve");
        assert_eq!(pool.free(a), Err(IpPoolError::NotAllocated(a)));
        assert_eq!(pool.available_count(), 4);
    }
}
