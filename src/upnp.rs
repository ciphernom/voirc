// src/upnp.rs

use anyhow::Result;
use std::net::Ipv4Addr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
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

                match gateway.add_port(
                    igd::PortMappingProtocol::TCP,
                    port,
                    local_addr, 
                    3600, 
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

    /// Get the external IP address (UPnP -> External HTTP -> Local Fallback)
    pub async fn get_external_ip() -> Result<String> {
        // 1. Try UPnP first
        if let Ok(gateway) = igd::search_gateway(Default::default()) {
            if let Ok(ip) = gateway.get_external_ip() {
                info!("Obtained external IP via UPnP: {}", ip);
                return Ok(ip.to_string());
            }
        }

        // 2. Try external HTTP service (api.ipify.org)
        // This is a manual HTTP request to avoid adding 'reqwest' dependency
        info!("UPnP IP lookup failed, trying api.ipify.org...");
        if let Ok(ip) = Self::fetch_public_ip_http().await {
            info!("Obtained external IP via HTTP: {}", ip);
            return Ok(ip);
        }

        // 3. Fallback to local IP
        warn!("External IP lookup failed. Using local IP.");
        Ok(Self::get_local_ip()?.to_string())
    }

    /// Manually fetch public IP over TCP to avoid heavy dependencies
    async fn fetch_public_ip_http() -> Result<String> {
        let addr = "api.ipify.org:80";
        let mut stream = TcpStream::connect(addr).await?;
        
        let request = format!(
            "GET / HTTP/1.1\r\n\
             Host: api.ipify.org\r\n\
             Connection: close\r\n\
             \r\n"
        );
        
        stream.write_all(request.as_bytes()).await?;
        
        let mut response = String::new();
        stream.read_to_string(&mut response).await?;
        
        // Simple parser to separate headers from body (split by double CRLF)
        if let Some((_, body)) = response.split_once("\r\n\r\n") {
            let ip = body.trim().to_string();
            // Basic validation: ensure it looks like an IP (x.x.x.x)
            if ip.split('.').count() == 4 {
                return Ok(ip);
            }
        }
        
        Err(anyhow::anyhow!("Failed to parse IP from HTTP response"))
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

    #[allow(dead_code)]
    pub async fn remove_port(port: u16) -> Result<()> {
        if let Ok(gateway) = igd::search_gateway(Default::default()) {
            let _ = gateway.remove_port(igd::PortMappingProtocol::TCP, port);
        }
        Ok(())
    }
}
