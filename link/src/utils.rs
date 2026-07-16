use crate::config::BackupConfig;
use anyhow::{Context, Result};
use lazy_static::lazy_static;
use quiche::ConnectionId;
use ring::hmac::{sign, Key};
use ring::rand::{SecureRandom, SystemRandom};
use std::net::{SocketAddr, ToSocketAddrs};

lazy_static! {
    pub(crate) static ref SYSTEM_RANDOM: SystemRandom = {
        let r = SystemRandom::new();
        // SystemRandom 的 fill 在初次调用时, 比较耗时, 提前初始化
        let mut b = [0u8; quiche::MAX_CONN_ID_LEN];
        r.fill(&mut b).unwrap();
        r
    };
}

pub(crate) fn random_cid() -> Result<ConnectionId<'static>> {
    let mut scid = [0u8; quiche::MAX_CONN_ID_LEN];
    // TODO: 生成一个非回文的随机连接 Source connetion ID
    // 避免在 server 端解包时, 需要通过 hash 算法根据 xxx 来算出一个 Destination connection ID, 带来资源的 overhead
    // 但需要在 go impl 有同样的处理, 有点 trick, 容易给不知情的 client impl 带来潜在 bug
    // 但, 如果这个开锁比较大的话, 仍然值得 DO
    SYSTEM_RANDOM
        .fill(&mut scid)
        .map_err(|err| anyhow::anyhow!("System random fill scid err {err}"))?;
    Ok(ConnectionId::from_vec(scid.to_vec()))
}

pub(crate) fn sign_cid(seed: &Key, cid: &ConnectionId<'_>) -> ConnectionId<'static> {
    let b = sign(seed, cid);
    let b = &b.as_ref()[..quiche::MAX_CONN_ID_LEN];
    ConnectionId::from_vec(b.to_vec())
}

#[derive(Debug, Clone)]
pub(crate) struct PeerAddr {
    pub(crate) socket_addr: SocketAddr,
    pub(crate) raw_addr: String,
}

impl std::fmt::Display for PeerAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} -> DNS -> {}", self.raw_addr, self.socket_addr)
    }
}

pub(crate) fn addr_resolve(
    addr: &str,
    backup_config: &Option<BackupConfig>,
) -> Result<(PeerAddr, Vec<PeerAddr>)> {
    let peer_addrs = addr
        .to_socket_addrs()
        .context("unable to resolve domin")?
        .filter(|addr| addr.is_ipv4())
        .map(|socket_addr| PeerAddr {
            socket_addr,
            raw_addr: addr.to_string(),
        })
        .collect::<Vec<PeerAddr>>();

    let (master_addr, mut backup_addrs) = match peer_addrs.split_first() {
        Some((first, rest)) => (first.clone(), rest.to_vec()),
        None => return Err(anyhow::anyhow!("master address dns resolve get empty")),
    };

    if let Some(backup_cfg) = backup_config {
        let addrs = backup_cfg
            .addr
            .to_socket_addrs()
            .context("unable to resolve domin")?
            .filter(|addr| addr.is_ipv4())
            .map(|socket_addr| PeerAddr {
                socket_addr,
                raw_addr: backup_cfg.addr.to_string(),
            })
            .collect::<Vec<PeerAddr>>();
        backup_addrs.extend(addrs);
    }

    Ok((master_addr, backup_addrs))
}
