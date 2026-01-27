//! GStreamer WebRTC streaming pipeline
//!
//! Takes raw RGBA frames from the renderer and encodes/streams them
//! to browser clients via WebRTC.

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// WebRTC signaling messages
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalingMessage {
    /// SDP offer from server
    Offer { sdp: String },
    /// SDP answer from client
    Answer { sdp: String },
    /// ICE candidate
    Ice { candidate: String, sdp_m_line_index: u32 },
}

/// Manages a WebRTC streaming session for one client
pub struct WebRtcStreamer {
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    webrtcbin: gst::Element,
    width: u32,
    height: u32,
    outgoing_tx: mpsc::UnboundedSender<SignalingMessage>,
}

impl WebRtcStreamer {
    /// Create a new WebRTC streaming session
    pub fn new(
        width: u32,
        height: u32,
        fps: u32,
        outgoing_tx: mpsc::UnboundedSender<SignalingMessage>,
    ) -> Result<Self> {
        gst::init().context("Failed to initialize GStreamer")?;

        // Create pipeline elements
        let pipeline = gst::Pipeline::new();

        // Video source from application (raw RGBA frames)
        let appsrc = gst_app::AppSrc::builder()
            .name("appsrc")
            .caps(
                &gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .field("width", width as i32)
                    .field("height", height as i32)
                    .field("framerate", gst::Fraction::new(fps as i32, 1))
                    .build(),
            )
            .format(gst::Format::Time)
            .is_live(true)
            .do_timestamp(true)
            .build();

        // Convert RGBA to format suitable for encoder
        let videoconvert = gst::ElementFactory::make("videoconvert")
            .name("videoconvert")
            .build()
            .context("Failed to create videoconvert")?;

        // Try hardware encoder first, fall back to software
        let encoder = Self::create_encoder()?;

        // H.264 parser to convert from avc to byte-stream format (required for RTP)
        let h264parse = gst::ElementFactory::make("h264parse")
            .name("h264parse")
            .property("config-interval", -1i32)
            .build()
            .context("Failed to create h264parse")?;

        // RTP payloader for H.264
        let rtppay = gst::ElementFactory::make("rtph264pay")
            .name("rtppay")
            .property("config-interval", -1i32)
            .property_from_str("aggregate-mode", "zero-latency")
            .build()
            .context("Failed to create rtph264pay")?;

        // Caps filter for RTP
        let rtpcaps = gst::ElementFactory::make("capsfilter")
            .name("rtpcaps")
            .property(
                "caps",
                gst::Caps::builder("application/x-rtp")
                    .field("media", "video")
                    .field("encoding-name", "H264")
                    .field("payload", 96i32)
                    .build(),
            )
            .build()
            .context("Failed to create capsfilter")?;

        // WebRTC bin
        let webrtcbin = gst::ElementFactory::make("webrtcbin")
            .name("webrtcbin")
            .property_from_str("bundle-policy", "max-bundle")
            .property("stun-server", "stun://stun.l.google.com:19302")
            .build()
            .context("Failed to create webrtcbin")?;

        // Add elements to pipeline
        pipeline.add_many([
            appsrc.upcast_ref(),
            &videoconvert,
            &encoder,
            &h264parse,
            &rtppay,
            &rtpcaps,
            &webrtcbin,
        ])?;

        // Link elements
        gst::Element::link_many([
            appsrc.upcast_ref(),
            &videoconvert,
            &encoder,
            &h264parse,
            &rtppay,
            &rtpcaps,
        ])?;

        // Link to webrtcbin (request a sink pad)
        let rtpcaps_src = rtpcaps.static_pad("src").unwrap();
        let webrtc_sink = webrtcbin
            .request_pad_simple("sink_%u")
            .context("Failed to request webrtcbin sink pad")?;
        rtpcaps_src.link(&webrtc_sink)?;

        // Set up WebRTC signaling callbacks
        let outgoing_tx_clone = outgoing_tx.clone();
        webrtcbin.connect("on-negotiation-needed", false, move |_| {
            tracing::info!("WebRTC negotiation needed");
            None
        });

        // Handle ICE candidates
        let outgoing_tx_ice = outgoing_tx.clone();
        webrtcbin.connect("on-ice-candidate", false, move |values| {
            let sdp_m_line_index = values[1].get::<u32>().unwrap();
            let candidate = values[2].get::<String>().unwrap();

            tracing::debug!("ICE candidate: {}", candidate);

            let _ = outgoing_tx_ice.send(SignalingMessage::Ice {
                candidate,
                sdp_m_line_index,
            });

            None
        });

        // Monitor ICE connection state
        webrtcbin.connect_notify(Some("ice-connection-state"), |webrtcbin, _| {
            let state: gst_webrtc::WebRTCICEConnectionState = webrtcbin
                .property("ice-connection-state");
            tracing::info!("ICE connection state: {:?}", state);
        });

        // Monitor connection state
        webrtcbin.connect_notify(Some("connection-state"), |webrtcbin, _| {
            let state: gst_webrtc::WebRTCPeerConnectionState = webrtcbin
                .property("connection-state");
            tracing::info!("WebRTC connection state: {:?}", state);
        });

        Ok(Self {
            pipeline,
            appsrc,
            webrtcbin,
            width,
            height,
            outgoing_tx: outgoing_tx_clone,
        })
    }

    /// Try to create hardware encoder, fall back to software
    fn create_encoder() -> Result<gst::Element> {
        // Try NVIDIA NVENC first
        if let Ok(encoder) = gst::ElementFactory::make("nvh264enc")
            .name("encoder")
            .property("preset", 4u32)        // low-latency-hq
            .property("zerolatency", true)
            .property("rc-mode", 2u32)       // cbr
            .property("bitrate", 4000u32)    // 4 Mbps
            .build()
        {
            tracing::info!("Using NVIDIA NVENC hardware encoder");
            return Ok(encoder);
        }

        // Try VA-API (Intel/AMD)
        if let Ok(encoder) = gst::ElementFactory::make("vaapih264enc")
            .name("encoder")
            .property("rate-control", 2u32)  // cbr
            .property("bitrate", 4000u32)
            .build()
        {
            tracing::info!("Using VA-API hardware encoder");
            return Ok(encoder);
        }

        // Try VideoToolbox (macOS)
        if let Ok(encoder) = gst::ElementFactory::make("vtenc_h264")
            .name("encoder")
            .property("bitrate", 4000u32)
            .property("realtime", true)
            .property("allow-frame-reordering", false)
            .property("max-keyframe-interval", 60i32) // Keyframe every 2 seconds
            .property("max-keyframe-interval-duration", 2_000_000_000u64) // 2 seconds in nanoseconds
            .build()
        {
            tracing::info!("Using VideoToolbox hardware encoder (macOS)");
            return Ok(encoder);
        }

        // Fall back to x264 software encoder
        let encoder = gst::ElementFactory::make("x264enc")
            .name("encoder")
            .property_from_str("tune", "zerolatency")
            .property_from_str("speed-preset", "ultrafast")
            .property("bitrate", 4000u32)
            .property("key-int-max", 30u32)
            .build()
            .context("Failed to create x264enc")?;

        tracing::info!("Using x264 software encoder");
        Ok(encoder)
    }

    /// Start the streaming pipeline
    pub fn start(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Playing)?;
        tracing::info!("WebRTC pipeline started");
        Ok(())
    }

    /// Stop the streaming pipeline
    pub fn stop(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Null)?;
        tracing::info!("WebRTC pipeline stopped");
        Ok(())
    }

    /// Push a frame to the pipeline
    pub fn push_frame(&self, rgba_data: &[u8], pts: gst::ClockTime) -> Result<()> {
        let expected_size = (self.width * self.height * 4) as usize;
        if rgba_data.len() != expected_size {
            anyhow::bail!(
                "Frame size mismatch: expected {}, got {}",
                expected_size,
                rgba_data.len()
            );
        }

        let mut buffer = gst::Buffer::with_size(rgba_data.len())?;
        {
            let buffer_ref = buffer.get_mut().unwrap();
            buffer_ref.set_pts(pts);

            let mut map = buffer_ref.map_writable()?;
            map.copy_from_slice(rgba_data);
        }

        self.appsrc.push_buffer(buffer)?;
        Ok(())
    }

    /// Create and send an SDP offer
    pub async fn create_offer(&self) -> Result<()> {
        let webrtcbin = self.webrtcbin.clone();
        let outgoing_tx = self.outgoing_tx.clone();

        // Create offer
        let promise = gst::Promise::with_change_func(move |reply| {
            let Ok(Some(reply)) = reply else {
                tracing::error!("No reply from create-offer");
                return;
            };

            let offer: gst_webrtc::WebRTCSessionDescription = match reply.value("offer") {
                Ok(val) => match val.get() {
                    Ok(o) => o,
                    Err(e) => {
                        tracing::error!("Failed to get offer: {:?}", e);
                        return;
                    }
                },
                Err(e) => {
                    tracing::error!("No offer in reply: {:?}", e);
                    return;
                }
            };

            let sdp_text = offer.sdp().to_string();
            tracing::debug!("Created SDP offer");

            // Set local description
            webrtcbin.emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);

            // Send offer to client
            let _ = outgoing_tx.send(SignalingMessage::Offer { sdp: sdp_text });
        });

        self.webrtcbin
            .emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);

        Ok(())
    }

    /// Handle incoming signaling message from client
    pub fn handle_signaling(&self, msg: SignalingMessage) -> Result<()> {
        match msg {
            SignalingMessage::Answer { sdp } => {
                tracing::debug!("Received SDP answer");

                let sdp = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes())?;
                let answer = gst_webrtc::WebRTCSessionDescription::new(
                    gst_webrtc::WebRTCSDPType::Answer,
                    sdp,
                );

                self.webrtcbin
                    .emit_by_name::<()>("set-remote-description", &[&answer, &None::<gst::Promise>]);
            }
            SignalingMessage::Ice {
                candidate,
                sdp_m_line_index,
            } => {
                tracing::debug!("Received ICE candidate");

                self.webrtcbin
                    .emit_by_name::<()>("add-ice-candidate", &[&sdp_m_line_index, &candidate]);
            }
            SignalingMessage::Offer { .. } => {
                // Server doesn't receive offers
            }
        }

        Ok(())
    }
}

impl Drop for WebRtcStreamer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
