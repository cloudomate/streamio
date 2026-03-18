//! Screen capture and WebRTC streaming with audio
//!
//! Captures the screen/display and system audio, streams via WebRTC - like a VDI.
//! Also receives microphone audio from the browser and plays it locally.

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_webrtc as gst_webrtc;
use streamio_types::SignalingMessage;
use tokio::sync::mpsc;

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

        // Video processing: convert colorspace/memory, encode to H.264
        let videoconvert = gst::ElementFactory::make("videoconvert").build()?;
        let queue = gst::ElementFactory::make("queue")
            .property("max-size-buffers", 2u32)
            .build()?;
        let encoder = Self::create_encoder()?;
        let h264parse = gst::ElementFactory::make("h264parse")
            .property("config-interval", -1i32)
            .build()?;
        let rtppay = gst::ElementFactory::make("rtph264pay")
            .property("config-interval", -1i32)
            .property_from_str("aggregate-mode", "zero-latency")
            .build()?;
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
        let webrtcbin = gst::ElementFactory::make("webrtcbin")
            .name("webrtcbin")
            .property_from_str("bundle-policy", "max-bundle")
            .property("stun-server", "stun://stun.l.google.com:19302")
            .build()?;

        // Simplest possible pipeline — gst-launch proves this works on Windows:
        // d3d11screencapturesrc ! videoconvert ! mfh264enc ! h264parse ! rtph264pay ! webrtcbin
        pipeline.add_many([
            &capture_src, &videoconvert, &queue, &encoder,
            &h264parse, &rtppay, &rtpcaps, &webrtcbin,
        ])?;
        gst::Element::link_many([
            &capture_src, &videoconvert, &queue, &encoder,
            &h264parse, &rtppay, &rtpcaps,
        ])?;

        // Link video to webrtcbin
        let rtpcaps_src = rtpcaps.static_pad("src")
            .context("capsfilter missing src pad")?;
        let webrtc_video_sink = webrtcbin.request_pad_simple("sink_%u")
            .context("webrtcbin failed to create sink pad — check that webrtc, srtp, dtls, and nice plugins are loaded")?;
        rtpcaps_src.link(&webrtc_video_sink)?;

        // Add audio pipeline if enabled
        if std::env::var("ENABLE_AUDIO").unwrap_or_default() == "1" {
            if let Err(e) = Self::add_audio_pipeline(&pipeline, &webrtcbin) {
                tracing::warn!("Audio capture not available: {}", e);
            }
        }

        // Set up handler for incoming audio from browser (mic)
        Self::setup_incoming_audio(&pipeline, &webrtcbin);

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
        // Try X11 capture if DISPLAY is set
        if std::env::var("DISPLAY").is_ok() {
            if let Ok(src) = gst::ElementFactory::make("ximagesrc")
                .property("use-damage", false)
                .property("show-pointer", true)
                .property("do-timestamp", true)
                .build()
            {
                tracing::info!("Using Linux screen capture (ximagesrc)");
                return Ok(src);
            }
        }

        // Wayland / PipeWire fallback
        if let Ok(src) = gst::ElementFactory::make("pipewiresrc")
            .property("do-timestamp", true)
            .build()
        {
            tracing::info!("Using Linux screen capture (pipewiresrc)");
            return Ok(src);
        }

        anyhow::bail!("No screen capture source available — set DISPLAY for X11 or ensure PipeWire is running for Wayland")
    }

    #[cfg(target_os = "windows")]
    fn create_windows_capture(_fps: u32) -> Result<gst::Element> {
        let monitor_index: i32 = std::env::var("DISPLAY_INDEX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // Windows Graphics Capture API: captures at the compositor level, works on
        // multi-GPU laptops (NVIDIA + AMD iGPU) where DXGI Desktop Duplication fails
        // because it requires running on the adapter that owns the display output.
        if let Ok(src) = gst::ElementFactory::make("d3d11screencapturesrc")
            .property("show-cursor", true)
            .property("do-timestamp", true)
            .property("monitor-index", monitor_index)
            .property_from_str("capture-api", "wgc")
            .build()
        {
            tracing::info!("Using Windows Graphics Capture API on monitor {}", monitor_index);
            return Ok(src);
        }

        // Fallback: DXGI Desktop Duplication
        if let Ok(src) = gst::ElementFactory::make("d3d11screencapturesrc")
            .property("show-cursor", true)
            .property("do-timestamp", true)
            .property("monitor-index", monitor_index)
            .build()
        {
            tracing::info!("Using DXGI Desktop Duplication capture on monitor {}", monitor_index);
            return Ok(src);
        }

        // Last resort: GDI-based capture
        gst::ElementFactory::make("dx9screencapsrc")
            .property("cursor", true)
            .build()
            .context("Failed to create any Windows screen capture source")
    }

    /// Add audio capture pipeline (system audio → WebRTC)
    fn add_audio_pipeline(pipeline: &gst::Pipeline, webrtcbin: &gst::Element) -> Result<()> {
        // Audio source - platform specific
        #[cfg(target_os = "macos")]
        let audio_src = {
            // On macOS, capturing system audio requires a virtual audio device
            // like BlackHole, Soundflower, or similar. Try to use it if available.
            gst::ElementFactory::make("osxaudiosrc")
                .property("do-timestamp", true)
                .build()
                .context("osxaudiosrc not available - install BlackHole for system audio")?
        };

        #[cfg(target_os = "linux")]
        let audio_src = {
            gst::ElementFactory::make("pulsesrc")
                .property("do-timestamp", true)
                .build()
                .or_else(|_| gst::ElementFactory::make("alsasrc").build())
                .context("No audio source available")?
        };

        #[cfg(target_os = "windows")]
        let audio_src = {
            gst::ElementFactory::make("wasapi2src")
                .property("do-timestamp", true)
                .property("loopback", true)
                .build()
                .or_else(|_| {
                    tracing::warn!("wasapi2src not available, falling back to wasapisrc");
                    gst::ElementFactory::make("wasapisrc")
                        .property("do-timestamp", true)
                        .property("loopback", true)
                        .build()
                })
                .context("No Windows audio source available")?
        };

        // Queue to decouple audio capture from encoding
        let audio_queue = gst::ElementFactory::make("queue")
            .property("max-size-buffers", 10u32)
            .property("max-size-time", 200_000_000u64) // 200ms
            .property_from_str("leaky", "downstream")
            .build()?;

        // Audio conversion and resampling
        let audioconvert = gst::ElementFactory::make("audioconvert").build()?;
        let audioresample = gst::ElementFactory::make("audioresample").build()?;

        // Opus encoder (required for WebRTC)
        let opusenc = gst::ElementFactory::make("opusenc")
            .property("bitrate", 64000i32)
            .build()
            .context("opusenc not available")?;

        // RTP payloader
        let rtpopuspay = gst::ElementFactory::make("rtpopuspay")
            .property("pt", 111u32)
            .build()?;

        // RTP caps
        let audio_rtpcaps = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("application/x-rtp")
                    .field("media", "audio")
                    .field("encoding-name", "OPUS")
                    .field("payload", 111i32)
                    .field("clock-rate", 48000i32)
                    .build(),
            )
            .build()?;

        // Add audio elements to pipeline
        pipeline.add_many([
            &audio_src,
            &audio_queue,
            &audioconvert,
            &audioresample,
            &opusenc,
            &rtpopuspay,
            &audio_rtpcaps,
        ])?;

        // Link audio elements
        gst::Element::link_many([
            &audio_src,
            &audio_queue,
            &audioconvert,
            &audioresample,
            &opusenc,
            &rtpopuspay,
            &audio_rtpcaps,
        ])?;

        // Link to webrtcbin
        let audio_src_pad = audio_rtpcaps.static_pad("src")
            .context("audio capsfilter missing src pad")?;
        let webrtc_audio_sink = webrtcbin.request_pad_simple("sink_%u")
            .context("webrtcbin failed to create audio sink pad")?;
        audio_src_pad.link(&webrtc_audio_sink)?;

        tracing::info!("Audio capture pipeline added");
        Ok(())
    }

    /// Set up handler for incoming audio from browser (mic → local speakers)
    fn setup_incoming_audio(pipeline: &gst::Pipeline, webrtcbin: &gst::Element) {
        let pipeline_weak = pipeline.downgrade();

        webrtcbin.connect_pad_added(move |_webrtc, pad| {
            let pipeline = match pipeline_weak.upgrade() {
                Some(p) => p,
                None => return,
            };

            // Only handle incoming media (not our outgoing streams)
            if pad.direction() != gst::PadDirection::Src {
                return;
            }

            let caps = pad.current_caps().or_else(|| Some(pad.query_caps(None)));
            let caps = match caps {
                Some(c) => c,
                None => return,
            };

            let s = caps.structure(0).unwrap();
            let media = s.get::<&str>("media").unwrap_or("");

            if media == "audio" {
                tracing::info!("Incoming audio stream detected");

                // Create audio playback pipeline
                let depay = gst::ElementFactory::make("rtpopusdepay").build().unwrap();
                let dec = gst::ElementFactory::make("opusdec").build().unwrap();
                let convert = gst::ElementFactory::make("audioconvert").build().unwrap();
                let resample = gst::ElementFactory::make("audioresample").build().unwrap();

                #[cfg(target_os = "macos")]
                let sink = gst::ElementFactory::make("osxaudiosink").build().unwrap();

                #[cfg(target_os = "linux")]
                let sink = gst::ElementFactory::make("pulsesink")
                    .build()
                    .unwrap_or_else(|_| gst::ElementFactory::make("alsasink").build().unwrap());

                #[cfg(target_os = "windows")]
                let sink = gst::ElementFactory::make("wasapisink").build().unwrap();

                pipeline.add_many([&depay, &dec, &convert, &resample, &sink]).unwrap();
                gst::Element::link_many([&depay, &dec, &convert, &resample, &sink]).unwrap();

                for elem in [&depay, &dec, &convert, &resample, &sink] {
                    elem.sync_state_with_parent().unwrap();
                }

                let depay_sink = depay.static_pad("sink").unwrap();
                pad.link(&depay_sink).unwrap();

                tracing::info!("Browser microphone audio routed to local speakers");
            }
        });
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

        // Windows: Media Foundation H.264 encoder — available on all modern Windows systems.
        // Automatically selects the best hardware encoder available (NVIDIA, AMD, Intel).
        // Works with system-memory frames from videoconvert upstream.
        #[cfg(target_os = "windows")]
        if let Ok(enc) = gst::ElementFactory::make("mfh264enc")
            .property("low-latency", true)
            .property("gop-size", 60i32)
            .property("bitrate", 4000u32)  // 4 Mbps
            .build()
        {
            tracing::info!("Using Windows Media Foundation H.264 encoder (hardware)");
            return Ok(enc);
        }

        // Linux/non-Windows: try Intel QuickSync
        #[cfg(not(target_os = "windows"))]
        if let Ok(enc) = gst::ElementFactory::make("qsvh264enc").build() {
            tracing::info!("Using Intel QuickSync encoder");
            return Ok(enc);
        }

        // Linux: VAAPI (AMD/Intel on Linux)
        #[cfg(not(target_os = "windows"))]
        if let Ok(enc) = gst::ElementFactory::make("vaapih264enc")
            .property("rate-control", 2u32)
            .build()
        {
            tracing::info!("Using VAAPI hardware encoder");
            return Ok(enc);
        }

        // Linux: NVIDIA NVENC
        #[cfg(not(target_os = "windows"))]
        if let Ok(enc) = gst::ElementFactory::make("nvh264enc")
            .property_from_str("rc-mode", "cbr")
            .property("gop-size", 60i32)
            .property("zerolatency", true)
            .build()
        {
            tracing::info!("Using NVIDIA NVENC hardware encoder");
            return Ok(enc);
        }

        // Software fallback (x264)
        let enc = gst::ElementFactory::make("x264enc")
            .property_from_str("tune", "zerolatency")
            .property_from_str("speed-preset", "ultrafast")
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

            let offer = match reply.value("offer") {
                Ok(v) => match v.get::<gst_webrtc::WebRTCSessionDescription>() {
                    Ok(o) => o,
                    Err(e) => {
                        tracing::error!("Create offer: failed to get WebRTCSessionDescription: {:?}", e);
                        return;
                    }
                },
                Err(_) => {
                    tracing::warn!("Create offer reply missing 'offer' field (pipeline shut down during negotiation)");
                    return;
                }
            };

            webrtcbin
                .emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);

            // Fix the H.264 fmtp line in the SDP offer. The hardware encoder
            // (mfh264enc) may advertise a high profile-level-id (e.g. 420432 =
            // Level 5.0) that some browsers reject. Replace it with Constrained
            // Baseline Level 3.1 (42e01f) which is universally supported.
            // Also strip sprop-parameter-sets which can confuse browser parsers.
            let mut sdp = offer.sdp().to_string();
            // Replace the entire fmtp line with a clean one
            let mut new_lines = Vec::new();
            for line in sdp.lines() {
                if line.starts_with("a=fmtp:96") {
                    new_lines.push("a=fmtp:96 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_string());
                } else {
                    new_lines.push(line.to_string());
                }
            }
            let sdp = new_lines.join("\r\n") + "\r\n";
            tracing::debug!("SDP offer:\n{}", sdp);
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
                tracing::debug!("SDP answer:\n{}", sdp);
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
