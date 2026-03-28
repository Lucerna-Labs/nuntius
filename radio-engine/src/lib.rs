use ccc_core::{Plugin, PluginError};
use clap::ValueEnum;
use std::any::Any;
use tokio::net::TcpListener;
use tokio::time::{sleep, Duration};

/// The streaming protocol to use for the radio engine.
#[derive(Clone, Debug, Default, ValueEnum)]
pub enum StreamingProtocol {
    /// Traditional continuous TCP stream (like Icecast/SHOUTcast).
    #[default]
    Icecast,
    /// Modern HTTP Live Streaming (segments and playlists over HTTP).
    Hls,
}

/// A native Rust streaming server acting as a highly efficient replacement
/// for traditional Icecast/SHOUTcast setups.
pub struct RadioEnginePlugin {
    pub port: u16,
    pub protocol: StreamingProtocol,
}

impl RadioEnginePlugin {
    /// Create a new instance of the Radio Engine plugin.
    pub fn new(port: u16, protocol: StreamingProtocol) -> Self {
        Self { port, protocol }
    }

    /// Start the asynchronous TCP listener for the streaming server.
    pub async fn run(&self) -> Result<(), std::io::Error> {
        let addr = format!("0.0.0.0:{}", self.port);

        match self.protocol {
            StreamingProtocol::Icecast => {
                let listener = TcpListener::bind(&addr).await?;
                println!("Icecast streaming server listening on {}", addr);

                // A very basic loop to simulate accepting incoming listener connections.
                loop {
                    match listener.accept().await {
                        Ok((_socket, peer_addr)) => {
                            println!("Accepted Icecast connection from: {}", peer_addr);
                            // Real implementation would spawn a task to stream data here.
                        }
                        Err(e) => {
                            eprintln!("Error accepting connection: {}", e);
                        }
                    }
                }
            }
            StreamingProtocol::Hls => {
                println!("HLS engine initializing on {}...", addr);
                println!("(Simulating) Generating HLS playlist .m3u8 and .ts segments...");

                // Simulate segment generation loop
                let mut segment_number = 1;
                loop {
                    println!("=> Wrote segment {}.ts to memory buffer.", segment_number);
                    segment_number += 1;
                    sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }
}

impl Plugin for RadioEnginePlugin {
    fn name(&self) -> &'static str {
        "Native Radio Engine"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        println!(
            "Initializing Native Radio Engine Plugin on port {} using protocol {:?}...",
            self.port, self.protocol
        );
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
