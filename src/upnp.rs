// src/upnp.rs

use anyhow::Result;
use std::net::Ipv4Addr;
use tracing::{info, warn};

pub struct PortForwarder;

impl PortForwarder {
    /// Attempt to forward a port using UPnP/IGD
    pub async fn forward_port(port: u16) -> Result<()> {
        info!("Attempting UPnP port forward for port {}", port);
        
        match igd::search_gateway(Default::default()) {
            Ok(gateway) => {
                info!("Found gateway: {:?}", gateway);
                
                let local_addr = std::net::SocketAddrV4::new(
                    Self::get_local_ip()?,
                    port,
                );

                // Try to add port mapping
                match gateway.add_port(
                    igd::PortMappingProtocol::TCP,
                    port,
                    local_addr, // Pass SocketAddrV4 directly
                    3600, // Lease duration: 1 hour
                    "Voirc IRC Server",
                ) {
                    Ok(_) => {
                        info!("Successfully forwarded port {} via UPnP", port);
                        Ok(())
                    }
                    Err(e) => {
                        warn!("Failed to add port mapping: {}", e);
                        Err(anyhow::anyhow!("UPnP port mapping failed: {}", e))
                    }
                }
            }
            Err(e) => {
                warn!("UPnP gateway not found: {}", e);
                Err(anyhow::anyhow!("UPnP not available: {}", e))
            }
        }
    }

    /// Get the external IP address
    pub async fn get_external_ip() -> Result<String> {
        match igd::search_gateway(Default::default()) {
            Ok(gateway) => {
                match gateway.get_external_ip() {
                    Ok(ip) => Ok(ip.to_string()),
                    Err(_) => {
                        // Fallback to STUN-based detection or local IP
                        Ok(Self::get_local_ip()?.to_string())
                    }
                }
            }
            Err(_) => {
                // Fallback to local IP
                Ok(Self::get_local_ip()?.to_string())
            }
        }
    }

    /// Get local IP address
    fn get_local_ip() -> Result<Ipv4Addr> {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0")?;
        socket.connect("8.8.8.8:80")?;
        let addr = socket.local_addr()?;
        
        if let std::net::SocketAddr::V4(addr_v4) = addr {
            Ok(*addr_v4.ip())
        } else {
            Err(anyhow::anyhow!("Could not determine local IPv4 address"))
        }
    }

    /// Remove port forwarding when done
    pub async fn remove_port(port: u16) -> Result<()> {
        if let Ok(gateway) = igd::search_gateway(Default::default()) {
            let _ = gateway.remove_port(igd::PortMappingProtocol::TCP, port);
        }
        Ok(())
    }
}
