mod irc_client;
mod state;
mod voice_mixer;
mod webrtc_peer;
mod irc_server;
mod config;
mod magic_link;
mod upnp;
mod gui;
mod topology;
mod moderation;
mod tls;
mod relay;
mod persistence;
mod pow; // <--- ADD THIS LINE

use anyhow::Result;
use tracing::info;

use crate::config::UserConfig;
use crate::gui::VoircApp;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = std::fs::create_dir_all("logs");
    let file_appender = tracing_appender::rolling::daily("logs", "voirc.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();

    // Install default rustls crypto provider
    let _ = rustls::crypto::ring::default_provider().install_default();

    let config = UserConfig::load()?;
    info!("Loaded config for user: {}", config.display_name);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("Voirc - Voice Chat over IRC"),
        ..Default::default()
    };

    eframe::run_native(
        "Voirc",
        native_options,
        Box::new(|cc| Ok(Box::new(VoircApp::new(cc, config)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {}", e))
}
