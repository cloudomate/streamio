//! Standalone WebRTC streaming using pure Rust libraries
//!
//! Uses webrtc-rs for WebRTC and openh264 for encoding.
//! No external dependencies - compiles to a single binary.

use anyhow::{Context, Result};
use bytes::Bytes;
use openh264::encoder::Encoder;
use openh264::formats::YUVSource;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264};
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::media::Sample;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;

/// WebRTC signaling messages
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalingMessage {
    /// SDP offer from server
    Offer { sdp: String },
    /// SDP answer from client
    Answer { sdp: String },
    /// ICE candidate
    Ice {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_m_line_index: Option<u16>,
    },
}

/// YUV420 buffer that implements YUVSource
struct Yuv420Buffer {
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Yuv420Buffer {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            y: vec![0u8; width * height],
            u: vec![128u8; width * height / 4],
            v: vec![128u8; width * height / 4],
        }
    }

    fn from_rgba(rgba: &[u8], width: usize, height: usize) -> Self {
        let mut buf = Self::new(width, height);

        for row in 0..height {
            for col in 0..width {
                let rgba_idx = (row * width + col) * 4;
                let r = rgba[rgba_idx] as i32;
                let g = rgba[rgba_idx + 1] as i32;
                let b = rgba[rgba_idx + 2] as i32;

                // BT.601 conversion
                let y_val = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
                buf.y[row * width + col] = y_val.clamp(0, 255) as u8;

                // Subsample U and V (2x2 blocks)
                if col % 2 == 0 && row % 2 == 0 {
                    let uv_idx = (row / 2) * (width / 2) + (col / 2);
                    let u_val = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
                    let v_val = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
                    buf.u[uv_idx] = u_val.clamp(0, 255) as u8;
                    buf.v[uv_idx] = v_val.clamp(0, 255) as u8;
                }
            }
        }

        buf
    }
}

impl YUVSource for Yuv420Buffer {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn strides(&self) -> (usize, usize, usize) {
        (self.width, self.width / 2, self.width / 2)
    }

    fn y(&self) -> &[u8] {
        &self.y
    }

    fn u(&self) -> &[u8] {
        &self.u
    }

    fn v(&self) -> &[u8] {
        &self.v
    }
}

/// H.264 encoder wrapper
struct H264Encoder {
    encoder: Encoder,
    width: u32,
    height: u32,
}

impl H264Encoder {
    fn new(width: u32, height: u32) -> Result<Self> {
        let encoder = Encoder::new().context("Failed to create H.264 encoder")?;

        Ok(Self {
            encoder,
            width,
            height,
        })
    }

    fn encode(&mut self, rgba: &[u8]) -> Result<Option<Bytes>> {
        let yuv = Yuv420Buffer::from_rgba(rgba, self.width as usize, self.height as usize);

        // Encode frame
        let bitstream = self.encoder.encode(&yuv).context("Encoding failed")?;

        // Get raw bitstream data
        let data = bitstream.to_vec();

        if data.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Bytes::from(data)))
        }
    }
}

/// Manages a WebRTC streaming session
pub struct WebRtcStreamer {
    peer_connection: Arc<RTCPeerConnection>,
    video_track: Arc<TrackLocalStaticSample>,
    encoder: Arc<Mutex<H264Encoder>>,
    outgoing_tx: mpsc::UnboundedSender<SignalingMessage>,
    frame_duration: std::time::Duration,
}

impl WebRtcStreamer {
    /// Create a new WebRTC streaming session
    pub async fn new(
        width: u32,
        height: u32,
        fps: u32,
        outgoing_tx: mpsc::UnboundedSender<SignalingMessage>,
    ) -> Result<Self> {
        // Create media engine with H.264 support
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs()?;

        // Create interceptor registry
        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)?;

        // Build API
        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        // ICE configuration
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_string()],
                ..Default::default()
            }],
            ..Default::default()
        };

        // Create peer connection
        let peer_connection = Arc::new(api.new_peer_connection(config).await?);

        // Create video track
        let video_track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_string(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_string(),
                rtcp_feedback: vec![],
            },
            "video".to_string(),
            "horizon-streamer".to_string(),
        ));

        // Add track to peer connection
        let rtp_sender = peer_connection
            .add_track(Arc::clone(&video_track) as Arc<dyn TrackLocal + Send + Sync>)
            .await?;

        // Spawn task to handle RTCP packets
        tokio::spawn(async move {
            let mut rtcp_buf = vec![0u8; 1500];
            while let Ok((_, _)) = rtp_sender.read(&mut rtcp_buf).await {}
        });

        // Set up ICE candidate handler
        let outgoing_tx_ice = outgoing_tx.clone();
        peer_connection.on_ice_candidate(Box::new(move |candidate| {
            let outgoing_tx = outgoing_tx_ice.clone();
            Box::pin(async move {
                if let Some(candidate) = candidate {
                    let json = candidate.to_json().unwrap();
                    let _ = outgoing_tx.send(SignalingMessage::Ice {
                        candidate: json.candidate,
                        sdp_mid: json.sdp_mid,
                        sdp_m_line_index: json.sdp_mline_index,
                    });
                }
            })
        }));

        // Log connection state changes
        peer_connection.on_ice_connection_state_change(Box::new(move |state: RTCIceConnectionState| {
            tracing::info!("ICE connection state: {:?}", state);
            Box::pin(async {})
        }));

        peer_connection.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
            tracing::info!("Peer connection state: {:?}", state);
            Box::pin(async {})
        }));

        // Create encoder
        let encoder = H264Encoder::new(width, height)?;
        tracing::info!("Created OpenH264 encoder ({}x{} @ {} fps)", width, height, fps);

        let frame_duration = std::time::Duration::from_secs_f64(1.0 / fps as f64);

        Ok(Self {
            peer_connection,
            video_track,
            encoder: Arc::new(Mutex::new(encoder)),
            outgoing_tx,
            frame_duration,
        })
    }

    /// Create and send an SDP offer
    pub async fn create_offer(&self) -> Result<()> {
        let offer = self.peer_connection.create_offer(None).await?;
        self.peer_connection.set_local_description(offer.clone()).await?;

        // Wait for ICE gathering to complete
        let mut gather_complete = self.peer_connection.gathering_complete_promise().await;
        let _ = gather_complete.recv().await;

        // Get the local description with ICE candidates
        let local_desc = self.peer_connection.local_description().await;
        if let Some(desc) = local_desc {
            tracing::debug!("Sending SDP offer");
            let _ = self.outgoing_tx.send(SignalingMessage::Offer { sdp: desc.sdp });
        }

        Ok(())
    }

    /// Handle incoming signaling message from client
    pub async fn handle_signaling(&self, msg: SignalingMessage) -> Result<()> {
        match msg {
            SignalingMessage::Answer { sdp } => {
                tracing::debug!("Received SDP answer");
                let answer = RTCSessionDescription::answer(sdp)?;
                self.peer_connection.set_remote_description(answer).await?;
            }
            SignalingMessage::Ice {
                candidate,
                sdp_mid,
                sdp_m_line_index,
            } => {
                tracing::debug!("Received ICE candidate");
                let candidate = webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
                    candidate,
                    sdp_mid,
                    sdp_mline_index: sdp_m_line_index,
                    username_fragment: None,
                };
                self.peer_connection.add_ice_candidate(candidate).await?;
            }
            SignalingMessage::Offer { .. } => {
                // Server doesn't receive offers
            }
        }
        Ok(())
    }

    /// Push a frame to the stream
    pub async fn push_frame(&self, rgba_data: &[u8]) -> Result<()> {
        let encoded = {
            let mut encoder = self.encoder.lock().await;
            encoder.encode(rgba_data)?
        };

        if let Some(data) = encoded {
            let data_len = data.len();
            let sample = Sample {
                data,
                duration: self.frame_duration,
                ..Default::default()
            };
            self.video_track.write_sample(&sample).await?;
            tracing::trace!("Sent {} bytes", data_len);
        } else {
            tracing::trace!("Encoder returned empty frame");
        }

        Ok(())
    }

    /// Close the connection
    pub async fn close(&self) -> Result<()> {
        self.peer_connection.close().await?;
        tracing::info!("WebRTC connection closed");
        Ok(())
    }
}
