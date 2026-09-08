// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

pub const TUN_IFACE: &str = "csqtt1";
use anyhow::{Result, bail};
use std::{net::Ipv4Addr, sync::OnceLock};

#[derive(Debug)]
pub struct TunnelNetwork {
    pub prefix: [u8; 3],
    pub subnet: String,
    pub gateway: String,
}

impl TunnelNetwork {
    pub fn parse(value: &str) -> Result<Self> {
        let Some((ip, "24")) = value.split_once('/') else {
            bail!("CSQTT_TUN_SUBNET must be a private IPv4 /24 network");
        };
        let ip: Ipv4Addr = ip.parse()?;
        let [a, b, c, host] = ip.octets();
        if !ip.is_private() || host != 0 {
            bail!("CSQTT_TUN_SUBNET must be a private network address ending in .0/24");
        }
        Ok(Self {
            prefix: [a, b, c],
            subnet: format!("{ip}/24"),
            gateway: format!("{a}.{b}.{c}.1"),
        })
    }

    pub fn client_ip(&self, host: u8) -> String {
        let [a, b, c] = self.prefix;
        format!("{a}.{b}.{c}.{host}")
    }
}

static NETWORK: OnceLock<TunnelNetwork> = OnceLock::new();

pub fn initialize() -> Result<()> {
    let value = std::env::var("CSQTT_TUN_SUBNET").unwrap_or_else(|_| "10.66.67.0/24".to_owned());
    let network = TunnelNetwork::parse(&value)?;
    NETWORK
        .set(network)
        .map_err(|_| anyhow::anyhow!("tunnel network already initialized"))
}

pub fn network() -> &'static TunnelNetwork {
    NETWORK.get_or_init(|| TunnelNetwork {
        prefix: [10, 66, 67],
        subnet: "10.66.67.0/24".to_owned(),
        gateway: "10.66.67.1".to_owned(),
    })
}

pub fn tun_subnet() -> &'static str {
    &network().subnet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_network_reaches_allocator_and_routes() {
        if std::env::var_os("CSQTT_NETWORK_TEST_CHILD").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "net_setup::tests::configured_network_reaches_allocator_and_routes",
                ])
                .env("CSQTT_NETWORK_TEST_CHILD", "1")
                .env("CSQTT_TUN_SUBNET", "172.23.45.0/24")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        initialize().unwrap();
        assert_eq!(tun_subnet(), "172.23.45.0/24");
        assert_eq!(
            crate::model::get_next_ip(&crate::model::Database::default()).as_deref(),
            Some("172.23.45.2")
        );
        let mut routes = crate::tun_device::RouteTable::new();
        routes.register([172, 23, 45, 2], 1, 1, 1, 0);
        assert_eq!(routes.stream_count([172, 23, 45, 2]), 1);
        assert_eq!(routes.stream_count([10, 66, 67, 2]), 0);
    }
    #[test]
    fn custom_network_addresses_and_validation() {
        let network = TunnelNetwork::parse("172.23.45.0/24").unwrap();
        assert_eq!(network.gateway, "172.23.45.1");
        assert_eq!(network.client_ip(250), "172.23.45.250");
        for invalid in [
            "0.0.0.0/24",
            "8.8.8.0/24",
            "10.1.2.1/24",
            "10.1.0.0/16",
            "10.1.2.0/24\n",
            "::/24",
        ] {
            assert!(TunnelNetwork::parse(invalid).is_err(), "{invalid}");
        }
    }
}
