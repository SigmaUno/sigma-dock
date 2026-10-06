//! Best-effort local port leases, unique among workers. Programs bind their own sockets.
use anyhow::{Result, bail};
use std::{
    collections::HashSet,
    net::{Ipv4Addr, TcpListener},
};
pub struct PortPool {
    first: u16,
    last: u16,
    leased: HashSet<u16>,
}
impl PortPool {
    pub fn new(first: u16, last: u16) -> Self {
        Self {
            first,
            last,
            leased: HashSet::new(),
        }
    }
    pub fn reserve(&mut self, port: u16) {
        self.leased.insert(port);
    }
    pub fn allocate(&mut self) -> Result<u16> {
        for port in self.first..=self.last {
            if !self.leased.contains(&port)
                && TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
            {
                self.leased.insert(port);
                return Ok(port);
            }
        }
        bail!("no available development port")
    }
    pub fn release(&mut self, port: u16) {
        self.leased.remove(&port);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_bind_and_leases() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut pool = PortPool::new(port, port);
        assert!(pool.allocate().is_err());
        drop(listener);
        assert_eq!(pool.allocate().unwrap(), port);
        assert!(pool.allocate().is_err());
        pool.release(port);
        assert_eq!(pool.allocate().unwrap(), port);
    }
}
