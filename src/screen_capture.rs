//! Screen capture and WebRTC streaming
//!
//! Captures the screen/display and streams via WebRTC - like a VDI.

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_webrtc as gst_webrtc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;

/// WebRTC signaling messages
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalingMessage {
    Offer { sdp: String },
    Answer { sdp: String },
    Ice { candidate: String, sdp_mid: Option<String>, sdp_m_line_index: Option<u32> },
}

/// Screen capture streamer using GStreamer WebRTC
pub struct ScreenStreamer {
    pipeline: gst::Pipeline,
    webrtcbin: gst::Element,
    outgoing_tx: mpsc::UnboundedSender<SignalingMessage>,
}

impl ScreenStreamer {
    /// Create a new screen capture streamer
    pub fn new(
        fps: u32,
        outgoing_tx: mpsc::UnboundedSender<SignalingMessage>,
    ) -> Result<Self> {
        let pipeline = gst::Pipeline::new();

        // Screen capture source - platform specific
        #[cfg(target_os = "macos")]
        let capture_src = Self::create_macos_capture(fps)?;

        #[cfg(target_os = "linux")]
        let capture_src = Self::create_linux_capture(fps)?;

        #[cfg(target_os = "windows")]
        let capture_src = Self::create_windows_capture(fps)?;

        // Queue for buffering
        let queue = gst::ElementFactory::make("queue")
            .property("max-size-buffers", 3u32)
            .property("max-size-time", 0u64)
            .property("max-size-bytes", 0u32)
            .build()?;

        // Video conversion
        let videoconvert = gst::ElementFactory::make("videoconvert").build()?;

        // Video scaling (optional - for bandwidth control)
        let videoscale = gst::ElementFactory::make("videoscale").build()?;
        let capsfilter = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("framerate", gst::Fraction::new(fps as i32, 1))
                    .build(),
            )
            .build()?;

        // Another queue before encoder
        let queue2 = gst::ElementFactory::make("queue")
            .property("max-size-buffers", 3u32)
            .build()?;

        // H.264 encoder - try hardware first, fall back to software
        let encoder = Self::create_encoder()?;

        // H.264 parser
        let h264parse = gst::ElementFactory::make("h264parse")
            .property("config-interval", -1i32)
            .build()?;

        // RTP payloader
        let rtppay = gst::ElementFactory::make("rtph264pay")
            .property("config-interval", -1i32)
            .property_from_str("aggregate-mode", "zero-latency")
            .build()?;

        // RTP caps filter
        let rtpcaps = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("application/x-rtp")
                    .field("media", "video")
                    .field("encoding-name", "H264")
                    .field("payload", 96i32)
                    .build(),
            )
            .build()?;

        // WebRTC bin
        let webrtcbin = gst::ElementFactory::make("webrtcbin")
            .name("webrtcbin")
            .property_from_str("bundle-policy", "max-bundle")
            .property("stun-server", "stun://stun.l.google.com:19302")
            .build()?;

        // Add all elements to pipeline
        pipeline.add_many([
            &capture_src,
            &queue,
            &videoconvert,
            &videoscale,
            &capsfilter,
            &queue2,
            &encoder,
            &h264parse,
            &rtppay,
            &rtpcaps,
            &webrtcbin,
        ])?;

        // Link elements
        gst::Element::link_many([
            &capture_src,
            &queue,
            &videoconvert,
            &videoscale,
            &capsfilter,
            &queue2,
            &encoder,
            &h264parse,
            &rtppay,
            &rtpcaps,
        ])?;

        // Link to webrtcbin
        let rtpcaps_src = rtpcaps.static_pad("src").unwrap();
        let webrtc_sink = webrtcbin.request_pad_simple("sink_%u").unwrap();
        rtpcaps_src.link(&webrtc_sink)?;

        // Set up WebRTC callbacks
        let tx = outgoing_tx.clone();
        webrtcbin.connect("on-negotiation-needed", false, move |_| {
            tracing::info!("WebRTC negotiation needed");
            None
        });

        let tx = outgoing_tx.clone();
        webrtcbin.connect("on-ice-candidate", false, move |values| {
            let sdp_m_line_index = values[1].get::<u32>().unwrap();
            let candidate = values[2].get::<String>().unwrap();

            let _ = tx.send(SignalingMessage::Ice {
                candidate,
                sdp_mid: None,
                sdp_m_line_index: Some(sdp_m_line_index),
            });
            None
        });

        // Monitor ICE connection state
        webrtcbin.connect("notify::ice-connection-state", false, |values| {
            let webrtcbin = values[0].get::<gst::Element>().unwrap();
            let state = webrtcbin.property::<gst_webrtc::WebRTCICEConnectionState>("ice-connection-state");
            tracing::info!("ICE connection state: {:?}", state);
            None
        });

        webrtcbin.connect("notify::connection-state", false, |values| {
            let webrtcbin = values[0].get::<gst::Element>().unwrap();
            let state = webrtcbin.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");
            tracing::info!("WebRTC connection state: {:?}", state);
            None
        });

        tracing::info!("Screen capture pipeline created");

        Ok(Self {
            pipeline,
            webrtcbin,
            outgoing_tx,
        })
    }

    #[cfg(target_os = "macos")]
    fn create_macos_capture(_fps: u32) -> Result<gst::Element> {
        // avfvideosrc captures screen on macOS
        // capture-screen=true captures the display instead of camera
        // do-timestamp=true is critical for live sources
        // device-index selects which display (0=main, 1=secondary, etc.)
        let display_index: i32 = std::env::var("DISPLAY_INDEX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let src = gst::ElementFactory::make("avfvideosrc")
            .property("capture-screen", true)
            .property("capture-screen-cursor", true)
            .property("do-timestamp", true)
            .property("device-index", display_index)
            .build()
            .context("Failed to create avfvideosrc - make sure GStreamer is installed with applemedia plugin")?;

        tracing::info!("Using macOS screen capture (avfvideosrc) on display {}", display_index);
        Ok(src)
    }

    #[cfg(target_os = "linux")]
    fn create_linux_capture(_fps: u32) -> Result<gst::Element> {
        // ximagesrc captures X11 display
        // For Wayland, use pipewiresrc
        let src = gst::ElementFactory::make("ximagesrc")
            .property("use-damage", false)
            .property("show-pointer", true)
            .property("do-timestamp", true)
            .build()
            .or_else(|_| {
                // Try pipewire for Wayland
                gst::ElementFactory::make("pipewiresrc")
                    .property("do-timestamp", true)
                    .build()
            })
            .context("Failed to create screen capture source")?;

        tracing::info!("Using Linux screen capture (ximagesrc/pipewiresrc)");
        Ok(src)
    }

    #[cfg(target_os = "windows")]
    fn create_windows_capture(fps: u32) -> Result<gst::Element> {
        // dx9screencapsrc or d3d11screencapturesrc for Windows
        let src = gst::ElementFactory::make("d3d11screencapturesrc")
            .property("show-cursor", true)
            .build()
            .or_else(|_| {
                gst::ElementFactory::make("dx9screencapsrc")
                    .property("cursor", true)
                    .build()
            })
            .context("Failed to create Windows screen capture source")?;

        tracing::info!("Using Windows screen capture");
        Ok(src)
    }

    fn create_encoder() -> Result<gst::Element> {
        // Try hardware encoders first, then fall back to software

        // macOS VideoToolbox
        if let Ok(enc) = gst::ElementFactory::make("vtenc_h264")
            .property("realtime", true)
            .property("allow-frame-reordering", false)
            .property("max-keyframe-interval", 30i32)
            .build()
        {
            tracing::info!("Using VideoToolbox hardware encoder");
            return Ok(enc);
        }

        // NVIDIA NVENC
        if let Ok(enc) = gst::ElementFactory::make("nvh264enc")
            .property("preset", 1u32)  // low-latency
            .property("rc-mode", 2u32) // CBR
            .property("zerolatency", true)
            .build()
        {
            tracing::info!("Using NVIDIA NVENC hardware encoder");
            return Ok(enc);
        }

        // Intel/AMD VAAPI
        if let Ok(enc) = gst::ElementFactory::make("vaapih264enc")
            .property("rate-control", 2u32) // CBR
            .build()
        {
            tracing::info!("Using VAAPI hardware encoder");
            return Ok(enc);
        }

        // Intel QuickSync
        if let Ok(enc) = gst::ElementFactory::make("qsvh264enc").build() {
            tracing::info!("Using Intel QuickSync encoder");
            return Ok(enc);
        }

        // Software fallback (x264)
        let enc = gst::ElementFactory::make("x264enc")
            .property("tune", 0x00000004u32) // zerolatency
            .property("speed-preset", 1u32)  // ultrafast
            .property("key-int-max", 30u32)
            .property("bitrate", 4000u32)    // 4 Mbps
            .build()
            .context("No H.264 encoder available")?;

        tracing::info!("Using x264 software encoder");
        Ok(enc)
    }

    /// Start the pipeline
    pub fn start(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Playing)?;
        tracing::info!("Screen capture pipeline started");
        Ok(())
    }

    /// Create and send an SDP offer
    pub fn create_offer(&self) {
        let webrtcbin = self.webrtcbin.clone();
        let tx = self.outgoing_tx.clone();

        let promise = gst::Promise::with_change_func(move |reply| {
            let reply = match reply {
                Ok(Some(reply)) => reply,
                Ok(None) => {
                    tracing::error!("Create offer got no response");
                    return;
                }
                Err(e) => {
                    tracing::error!("Create offer error: {:?}", e);
                    return;
                }
            };

            let offer = reply
                .value("offer")
                .unwrap()
                .get::<gst_webrtc::WebRTCSessionDescription>()
                .unwrap();

            webrtcbin
                .emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);

            let sdp = offer.sdp().to_string();
            let _ = tx.send(SignalingMessage::Offer { sdp });
            tracing::info!("SDP offer sent");
        });

        self.webrtcbin
            .emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);
    }

    /// Handle incoming signaling message
    pub fn handle_signaling(&self, msg: SignalingMessage) -> Result<()> {
        match msg {
            SignalingMessage::Answer { sdp } => {
                tracing::info!("Received SDP answer");
                let sdp = gstreamer_sdp::SDPMessage::parse_buffer(sdp.as_bytes())?;
                let answer = gst_webrtc::WebRTCSessionDescription::new(
                    gst_webrtc::WebRTCSDPType::Answer,
                    sdp,
                );
                self.webrtcbin
                    .emit_by_name::<()>("set-remote-description", &[&answer, &None::<gst::Promise>]);
            }
            SignalingMessage::Ice {
                candidate,
                sdp_mid,
                sdp_m_line_index,
            } => {
                let sdp_m_line_index = sdp_m_line_index.unwrap_or(0);
                self.webrtcbin.emit_by_name::<()>(
                    "add-ice-candidate",
                    &[&sdp_m_line_index, &candidate],
                );
            }
            SignalingMessage::Offer { .. } => {
                // Server doesn't receive offers
            }
        }
        Ok(())
    }

    /// Stop the pipeline
    pub fn stop(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Null)?;
        tracing::info!("Screen capture pipeline stopped");
        Ok(())
    }
}

impl Drop for ScreenStreamer {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
