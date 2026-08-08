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
}

#[derive(Debug, Clone)]
pub struct IpPool {
    network: u32,
    total: u32,
    bits: Vec<u64>,
}

impl IpPool {
    pub fn new(subnet: Ipv4Net) -> Result<Self, IpPoolError> {
        let prefix = subnet.prefix_len();
        if !(1..=30).contains(&prefix) {
            return Err(IpPoolError::InvalidSubnet);
        }
        let total = 1u32 << (32u8 - prefix);
        let broadcast_off = total - 1;
        let word_count = total.div_ceil(64);
        let network = u32::from(subnet.network());
        let bits: Vec<u64> = (0..word_count)
            .map(|w| {
                let base = w * 64;
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
            })
            .collect();
        Ok(Self {
            network,
            total,
            bits,
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
}
