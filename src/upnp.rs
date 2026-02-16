use anyhow::Result;
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{info, warn};

use crate::state::AppState;

pub struct PortForwarder;

impl PortForwarder {
    pub async fn forward_port(port: u16, state: Option<Arc<AppState>>) -> Result<()> {
        info!("Attempting UPnP port forward for port {}", port);

        match igd::search_gateway(Default::default()) {
            Ok(gateway) => {
                info!("Found gateway: {:?}", gateway);
                if let Some(s) = &state {
                    let mut d = s.diagnostics.write().await;
                    d.upnp_status = Some("Gateway found".to_string());
                }

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
                        if let Some(s) = &state {
                            let mut d = s.diagnostics.write().await;
                            d.upnp_status = Some(format!("Port {} forwarded", port));
                            d.port_open = Some(true);
                        }
                        Ok(())
                    }
                    Err(e) => {
                        warn!("Failed to add port mapping: {}", e);
                        if let Some(s) = &state {
                            let mut d = s.diagnostics.write().await;
                            d.upnp_status = Some(format!("Port mapping failed: {}", e));
                            d.port_open = Some(false);
                        }
                        Err(anyhow::anyhow!("UPnP port mapping failed: {}", e))
                    }
                }
            }
            Err(e) => {
                warn!("UPnP gateway not found: {}", e);
                if let Some(s) = &state {
                    let mut d = s.diagnostics.write().await;
                    d.upnp_status = Some(format!("Not available: {}", e));
                    d.port_open = Some(false);
                }
                Err(anyhow::anyhow!("UPnP not available: {}", e))
            }
        }
    }

    pub async fn get_external_ip(state: Option<Arc<AppState>>) -> Result<String> {
        // Store local IP in diagnostics
        if let Ok(local) = Self::get_local_ip() {
            if let Some(s) = &state {
                let mut d = s.diagnostics.write().await;
                d.local_ip = Some(local.to_string());
            }
        }

        if let Ok(gateway) = igd::search_gateway(Default::default()) {
            if let Ok(ip) = gateway.get_external_ip() {
                info!("Obtained external IP via UPnP: {}", ip);
                if let Some(s) = &state {
                    let mut d = s.diagnostics.write().await;
                    d.external_ip = Some(ip.to_string());
                }
                return Ok(ip.to_string());
            }
        }

        info!("UPnP IP lookup failed, trying api.ipify.org...");
        if let Ok(ip) = Self::fetch_public_ip_http().await {
            info!("Obtained external IP via HTTP: {}", ip);
            if let Some(s) = &state {
                let mut d = s.diagnostics.write().await;
                d.external_ip = Some(ip.clone());
            }
            return Ok(ip);
        }

        warn!("External IP lookup failed. Using local IP.");
        Ok(Self::get_local_ip()?.to_string())
    }

    async fn fetch_public_ip_http() -> Result<String> {
        let addr = "api.ipify.org:80";
        let mut stream = TcpStream::connect(addr).await?;

        let request = "GET / HTTP/1.1\r\nHost: api.ipify.org\r\nConnection: close\r\n\r\n";
        stream.write_all(request.as_bytes()).await?;

        let mut response = String::new();
        stream.read_to_string(&mut response).await?;

        if let Some((_, body)) = response.split_once("\r\n\r\n") {
            let ip = body.trim().to_string();
            if ip.split('.').count() == 4 {
                return Ok(ip);
            }
        }

        Err(anyhow::anyhow!("Failed to parse IP from HTTP response"))
    }

    pub fn get_local_ip() -> Result<Ipv4Addr> {
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
