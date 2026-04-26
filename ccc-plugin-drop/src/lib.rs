use axum::{
    response::{IntoResponse, Json},
    routing::post,
    Router,
};
use ccc_core::{Plugin, PluginError};
use serde::{Deserialize, Serialize};
use std::any::Any;

/// Represents an SDP (Session Description Protocol) offer/answer
/// used to establish the WebRTC peer-to-peer data connection.
#[derive(Debug, Serialize, Deserialize)]
pub struct SdpMessage {
    pub sdp_type: String, // e.g., "offer" or "answer"
    pub sdp: String,
}

/// Represents an ICE candidate for NAT traversal during WebRTC signaling.
#[derive(Debug, Serialize, Deserialize)]
pub struct IceCandidate {
    pub candidate: String,
    pub sdp_mid: Option<String>,
    pub sdp_m_line_index: Option<u16>,
}

/// The main entry point for the CCC Drop Plugin.
#[derive(Default)]
pub struct DropPlugin;

impl DropPlugin {
    pub fn new() -> Self {
        Self
    }

    /// Build the `axum` router for WebRTC Data Channel signaling endpoints.
    pub fn build_router() -> Router {
        Router::new()
            .route("/api/drop/offer", post(sdp_offer_handler))
            .route("/api/drop/ice", post(ice_candidate_handler))
    }
}

impl Plugin for DropPlugin {
    fn name(&self) -> &'static str {
        "CCC Drop File Transfer"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn init(&mut self) -> Result<(), PluginError> {
        println!("Initializing CCC Drop (WebRTC P2P File Transfer)...");
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Handler to receive an SDP offer from a client's browser attempting to send/receive a file.
async fn sdp_offer_handler(Json(offer): Json<SdpMessage>) -> impl IntoResponse {
    // In a full WebRTC Data Channel implementation (using `webrtc.rs`), we would:
    // 1. Parse the sender's SDP offer.
    // 2. Create a new PeerConnection and an OnDataChannel handler.
    // 3. Set the remote description.
    // 4. Generate and return a local SDP answer.

    println!("Received WebRTC Drop SDP Offer: {:?}", offer.sdp_type);

    let mock_answer = SdpMessage {
        sdp_type: "answer".to_string(),
        sdp: "v=0\r\n... (mock server answer SDP) ...".to_string(),
    };

    Json(mock_answer)
}

/// Handler to receive ICE candidates to establish the direct file transfer link.
async fn ice_candidate_handler(Json(candidate): Json<IceCandidate>) -> impl IntoResponse {
    // We pass this candidate to our `webrtc.rs` PeerConnection to establish the UDP/TCP data link.
    println!("Received WebRTC Drop ICE Candidate: {:?}", candidate.candidate);

    axum::http::StatusCode::OK
}
