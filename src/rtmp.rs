use bytes::Bytes;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, RateControlMode,
    UsageType,
};
use openh264::OpenH264API;
use rtmp_rs::amf::{Amf0Encoder, AmfValue};
use rtmp_rs::client::config::ClientConfig;
use rtmp_rs::client::publisher::{PublishEvent, RtmpPublisher};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::sleep;

use crate::capture::{FastYuvBuffer, VideoFrameSource};
use crate::ui::LiveTelemetry;

/// Test if the streaming destination and stream key can successfully connect and sync.
pub async fn test_stream_sync(server_url: &str, stream_key: &str) -> Result<String, String> {
    let clean_url = server_url.trim();
    let clean_key = stream_key.trim();

    if clean_url.is_empty() {
        return Err("Ingest Server URL cannot be empty.".to_string());
    }
    if clean_key.is_empty() {
        return Err("Stream Key cannot be empty. Please enter your stream key.".to_string());
    }

    let full_url = format!("{}/{}", clean_url.trim_end_matches('/'), clean_key);

    let mut config = ClientConfig::new(&full_url);
    config.connect_timeout = Duration::from_secs(8);
    config.enhanced_rtmp = rtmp_rs::protocol::enhanced::EnhancedRtmpMode::LegacyOnly;
    config.flash_ver = "FMLE/3.0 (compatible; FMSc/1.0)".to_string();

    let parsed = config.parse_url().ok_or_else(|| {
        "Invalid RTMP/RTMPS URL format. Must start with rtmp:// or rtmps://".to_string()
    })?;

    // 1. DNS Resolution Check
    let addr_str = format!("{}:{}", parsed.host, parsed.port);
    let _ = tokio::net::lookup_host(&addr_str)
        .await
        .map_err(|e| format!("DNS resolution failed for {}: {}", parsed.host, e))?;

    // 2. RTMP Handshake & Publish Sync
    let (mut publisher, mut events) = RtmpPublisher::new(config);
    let error_holder = Arc::new(tokio::sync::Mutex::new(None));
    let err_clone = error_holder.clone();

    tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            if let PublishEvent::Error(err) = ev {
                *err_clone.lock().await = Some(err);
            }
        }
    });

    match publisher.connect().await {
        Ok(_) => {
            let meta = build_on_metadata(1280.0, 720.0, 30.0, 2500.0);
            let _ = publisher.send_metadata(Bytes::from(meta)).await;

            let aac_seq = vec![0xAF, 0x00, 0x11, 0x90];
            let _ = publisher.send_audio(Bytes::from(aac_seq), 0).await;

            let sps = [0x67, 0x42, 0xc0, 0x1f, 0x8c, 0x8d, 0x40, 0x50, 0x1e, 0x88];
            let pps = [0x68, 0xce, 0x3c, 0x80];
            let avc_seq = build_avcc_sequence_header(&sps, &pps);
            let _ = publisher.send_video(Bytes::from(avc_seq), 0).await;

            publisher.disconnect().await;
            Ok("✔ Stream Sync Successful! Ingest server acknowledged RTMP handshake and verified stream key.".to_string())
        }
        Err(e) => {
            let detail = error_holder.lock().await.clone();
            let err_str = format!("{:?}", e);
            if err_str.contains("10054")
                || err_str.contains("ConnectionReset")
                || err_str.contains("ConnectionClosed")
            {
                Err(format!(
                    "✖ Sync Rejected: Server at '{}' closed connection. Your Stream Key may be invalid, expired, or rejected.",
                    parsed.host
                ))
            } else if err_str.contains("Timeout") {
                Err(format!(
                    "✖ Sync Timed Out: Server at '{}' did not respond within 8 seconds.",
                    parsed.host
                ))
            } else if let Some(d) = detail {
                Err(format!("✖ Sync Failed: {}", d))
            } else {
                Err(format!("✖ Sync Failed: {}", err_str))
            }
        }
    }
}

pub struct StreamSession {
    pub server_url: String,
    pub stream_key: String,
    pub width: u32,
    pub height: u32,
    pub target_fps: u32,
    pub target_bitrate_kbps: u32,
    pub capture_mode: String,
    pub target_window_title: String,
    pub target_window_hwnd: isize,
    pub telemetry: LiveTelemetry,
    pub status_tx: mpsc::UnboundedSender<String>,
}

impl StreamSession {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        server_url: String,
        stream_key: String,
        width: u32,
        height: u32,
        target_fps: u32,
        target_bitrate_kbps: u32,
        capture_mode: String,
        target_window_title: String,
        target_window_hwnd: isize,
        telemetry: LiveTelemetry,
        status_tx: mpsc::UnboundedSender<String>,
    ) -> Self {
        Self {
            server_url,
            stream_key,
            width: (width / 2) * 2,
            height: (height / 2) * 2,
            target_fps: target_fps.clamp(15, 60),
            target_bitrate_kbps: target_bitrate_kbps.clamp(500, 12000),
            capture_mode,
            target_window_title,
            target_window_hwnd,
            telemetry,
            status_tx,
        }
    }

    pub async fn run(self, stop_signal: Arc<AtomicBool>) -> Result<(), String> {
        let full_url = format!(
            "{}/{}",
            self.server_url.trim_end_matches('/'),
            self.stream_key.trim()
        );
        tracing::info!("Connecting to RTMP target: {}", self.server_url);
        let _ = self
            .status_tx
            .send("Connecting to ingest server...".to_string());

        let mut config = ClientConfig::new(&full_url);
        config.connect_timeout = Duration::from_secs(12);
        config.tcp_nodelay = true;
        config.enhanced_rtmp = rtmp_rs::protocol::enhanced::EnhancedRtmpMode::LegacyOnly;
        config.flash_ver = "FMLE/3.0 (compatible; FMSc/1.0)".to_string();

        let (mut publisher, mut events) = RtmpPublisher::new(config);

        let stop_clone = stop_signal.clone();
        let status_tx_clone = self.status_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                tracing::info!("RTMP Event: {:?}", event);
                match event {
                    PublishEvent::Error(err) => {
                        tracing::error!("RTMP Publishing error: {}", err);
                        let _ = status_tx_clone.send(format!("Streaming error: {}", err));
                        stop_clone.store(false, Ordering::Relaxed);
                    }
                    PublishEvent::Disconnected => {
                        tracing::warn!("RTMP Disconnected from server.");
                        let _ = status_tx_clone.send("Disconnected from server.".to_string());
                        stop_clone.store(false, Ordering::Relaxed);
                    }
                    _ => {}
                }
            }
        });

        // 1. Establish RTMP/RTMPS Connection & Handshake
        if let Err(e) = publisher.connect().await {
            let err_str = format!("{:?}", e);
            let user_msg = if err_str.contains("10054")
                || err_str.contains("ConnectionReset")
                || err_str.contains("ConnectionClosed")
            {
                "Server rejected connection. Please check that your Stream Key is valid and active."
                    .to_string()
            } else if err_str.contains("Timeout") {
                "Connection timed out. Ingest server did not respond.".to_string()
            } else {
                format!("Connection error: {}", err_str)
            };

            tracing::error!("{}", user_msg);
            let _ = self.status_tx.send(user_msg.clone());
            self.telemetry.is_streaming.store(false, Ordering::Relaxed);
            stop_signal.store(false, Ordering::Relaxed);
            return Err(user_msg);
        }

        tracing::info!("Successfully connected and publishing to RTMP server!");
        let _ = self
            .status_tx
            .send("LIVE: Broadcasting stable video & audio stream...".to_string());
        self.telemetry.is_streaming.store(true, Ordering::Relaxed);

        let fps = self.target_fps;

        // 2. Transmit Canonical AMF0 onMetaData (@setDataFrame)
        let metadata_bytes = build_on_metadata(
            self.width as f64,
            self.height as f64,
            fps as f64,
            self.target_bitrate_kbps as f64,
        );
        if let Err(e) = publisher.send_metadata(Bytes::from(metadata_bytes)).await {
            tracing::warn!("Failed to send onMetaData: {:?}", e);
        }

        // 3. Transmit initial Audio Specific Config (AAC-LC 48kHz Stereo)
        let aac_seq_header = vec![0xAF, 0x00, 0x11, 0x90];
        if let Err(e) = publisher.send_audio(Bytes::from(aac_seq_header), 0).await {
            tracing::warn!("Failed to send AAC sequence header: {:?}", e);
        }

        // 4. Initialize OpenH264 Real-Time Encoder
        // Note: UsageType::CameraVideoRealTime with Complexity::Low and skip_frames(false)
        // is 4x-6x faster on game/motion frames than ScreenContentRealTime and never skips frames after IDR keyframes.
        let keyframe_frames = (fps * 2).max(30);
        let enc_config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(self.target_bitrate_kbps * 1000))
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            .rate_control_mode(RateControlMode::Bitrate)
            .usage_type(UsageType::CameraVideoRealTime)
            .complexity(Complexity::Low)
            .skip_frames(false)
            .adaptive_quantization(false)
            .background_detection(false)
            .scene_change_detect(false)
            .qp(openh264::encoder::QpRange::new(22, 42))
            .num_threads(0)
            .intra_frame_period(IntraFramePeriod::from_num_frames(keyframe_frames));

        let mut h264_encoder = Encoder::with_api_config(OpenH264API::from_source(), enc_config)
            .map_err(|e| format!("Failed to create H.264 encoder: {:?}", e))?;

        struct EncodedFrame {
            data: Vec<u8>,
            timestamp_ms: u32,
        }

        // Buffered channel (24 frames = ~800ms cushion) so transient network jitter never stalls capture
        let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<EncodedFrame>(24);
        let stop_worker = stop_signal.clone();
        let telemetry_clone = self.telemetry.clone();
        let cap_w = self.width;
        let cap_h = self.height;
        let cap_mode = self.capture_mode.clone();
        let cap_title = self.target_window_title.clone();
        let cap_hwnd = self.target_window_hwnd;

        // Dedicated capture + encode OS thread paced to real wall-clock time
        std::thread::spawn(move || {
            crate::win_enum::attach_to_input_desktop();

            let mut frame_source = VideoFrameSource::new(
                cap_w,
                cap_h,
                cap_mode,
                cap_title,
                cap_hwnd,
            );
            let mut yuv_buffer = FastYuvBuffer::new(cap_w as usize, cap_h as usize);

            let frame_interval_us = 1_000_000u64 / (fps as u64);
            let target_interval = Duration::from_micros(frame_interval_us);
            let stream_epoch = Instant::now();
            let mut next_frame_deadline = stream_epoch;
            let mut last_timestamp_ms = 0u32;
            let mut frame_count = 0u64;

            while stop_worker.load(Ordering::Relaxed) {
                if frame_count % (keyframe_frames as u64) == 0 {
                    h264_encoder.force_intra_frame();
                }
                frame_count += 1;

                // Real wall-clock timestamp prevents 4x video speed-up and server buffer underruns!
                let raw_elapsed_ms = stream_epoch.elapsed().as_millis() as u32;
                let timestamp_ms = if frame_count == 1 {
                    0
                } else {
                    raw_elapsed_ms.max(last_timestamp_ms.saturating_add(1))
                };
                last_timestamp_ms = timestamp_ms;

                // 1. Capture desktop or target window directly into planar I420 buffer in a single pass
                frame_source.capture_into_yuv(&mut yuv_buffer);

                // 2. Encode I420 frame to H.264 Annex-B
                let encoded_data = match h264_encoder.encode(&yuv_buffer) {
                    Ok(stream) => stream.to_vec(),
                    Err(_) => Vec::new(),
                };

                if !encoded_data.is_empty() {
                    let frame = EncodedFrame {
                        data: encoded_data,
                        timestamp_ms,
                    };
                    if frame_tx.try_send(frame).is_err() {
                        telemetry_clone
                            .dropped_frames
                            .fetch_add(1, Ordering::Relaxed);
                    }
                }

                // Drift-compensated frame pacing
                next_frame_deadline += target_interval;
                let now = Instant::now();
                if next_frame_deadline > now {
                    std::thread::sleep(next_frame_deadline - now);
                } else {
                    // If behind schedule, reset deadline to `now` so we don't burst-speed frames
                    next_frame_deadline = now;
                }
            }
        });

        let mut last_stats_time = Instant::now();
        let mut last_ping_poll = Instant::now();
        let mut frames_this_second = 0u32;
        let mut total_frames_sent = 0u64;
        let mut bytes_sent_recent = 0u64;
        let mut seq_header_sent = false;
        let mut audio_frame_index = 0u64;
        let aac_frame_bytes = Bytes::from(build_aac_audio_frame());

        // 5. Active Live Streaming Loop (Transmits real-time video & 48kHz AAC audio frames + handles RTMP Pings)
        while stop_signal.load(Ordering::Relaxed) {
            tokio::select! {
                frame_opt = frame_rx.recv() => {
                    let frame = match frame_opt {
                        Some(f) => f,
                        None => break,
                    };

                    frames_this_second += 1;
                    total_frames_sent += 1;

                    let nals = split_annex_b(&frame.data);

                    let sps_opt = nals.iter().find(|n| !n.is_empty() && (n[0] & 0x1F) == 7);
                    let pps_opt = nals.iter().find(|n| !n.is_empty() && (n[0] & 0x1F) == 8);

                    if let (Some(sps), Some(pps)) = (sps_opt, pps_opt) {
                        if !seq_header_sent {
                            let avc_seq = build_avcc_sequence_header(sps, pps);
                            let seq_len = avc_seq.len() as u64;
                            if let Err(e) = publisher.send_video(Bytes::from(avc_seq), frame.timestamp_ms).await {
                                tracing::warn!("Failed sending AVC sequence header: {:?}", e);
                                break;
                            }
                            bytes_sent_recent += seq_len;
                            seq_header_sent = true;
                        }
                    }

                    // Send continuous 48kHz AAC-LC audio frames (1024 samples = 21.333ms per frame)
                    // BEFORE the video frame at `frame.timestamp_ms` so multiplexed A/V timestamps
                    // are strictly monotonic across the RTMP stream!
                    let mut audio_err = false;
                    while ((audio_frame_index * 1024 * 1000) / 48_000) as u32 <= frame.timestamp_ms {
                        let audio_ts = ((audio_frame_index * 1024 * 1000) / 48_000) as u32;
                        audio_frame_index += 1;
                        let audio_len = aac_frame_bytes.len() as u64;
                        if let Err(e) = publisher.send_audio(aac_frame_bytes.clone(), audio_ts).await {
                            tracing::warn!("Failed sending audio frame: {:?}", e);
                            audio_err = true;
                            break;
                        }
                        bytes_sent_recent += audio_len;
                    }
                    if audio_err {
                        break;
                    }

                    // Collect video slice NALs (type 5 = IDR, type 1 = Non-IDR)
                    let vcl_nals: Vec<&[u8]> = nals
                        .into_iter()
                        .filter(|n| !n.is_empty() && ((n[0] & 0x1F) == 5 || (n[0] & 0x1F) == 1))
                        .collect();

                    if !vcl_nals.is_empty() {
                        let has_idr = vcl_nals.iter().any(|n| (n[0] & 0x1F) == 5);
                        let mut flv_video = Vec::with_capacity(5 + vcl_nals.len() * 4 + frame.data.len());
                        flv_video.push(if has_idr { 0x17 } else { 0x27 });
                        flv_video.push(0x01); // AVC NALU
                        flv_video.extend_from_slice(&[0x00, 0x00, 0x00]); // CompositionTime = 0

                        for nal in vcl_nals {
                            let len = nal.len() as u32;
                            flv_video.extend_from_slice(&len.to_be_bytes());
                            flv_video.extend_from_slice(nal);
                        }

                        let flv_len = flv_video.len() as u64;
                        if let Err(e) = publisher.send_video(Bytes::from(flv_video), frame.timestamp_ms).await {
                            tracing::warn!("Failed sending video frame: {:?}", e);
                            let _ = self.status_tx.send(format!("Network error while sending video: {}", e));
                            break;
                        }
                        bytes_sent_recent += flv_len;
                    }

                    // Poll incoming RTMP control messages (PingRequest -> PingResponse) zero-wait
                    if last_ping_poll.elapsed() >= Duration::from_millis(100) {
                        last_ping_poll = Instant::now();
                        if let Err(e) = publisher.poll_incoming().await {
                            tracing::warn!("RTMP control channel closed or errored: {:?}", e);
                            let _ = self.status_tx.send(format!("Ingest server disconnected: {}", e));
                            break;
                        }
                    }

                    // Telemetry calculation per second
                    self.telemetry
                        .total_frames
                        .store(total_frames_sent, Ordering::Relaxed);
                    let stats_elapsed = last_stats_time.elapsed();
                    if stats_elapsed >= Duration::from_secs(1) {
                        let secs = stats_elapsed.as_secs_f64().max(0.1);
                        let kbps = (((bytes_sent_recent as f64) * 8.0) / (1000.0 * secs)) as u32;
                        let actual_fps = ((frames_this_second as f64) / secs).round() as u32;
                        self.telemetry.current_bitrate_kbps.store(kbps, Ordering::Relaxed);
                        self.telemetry.fps.store(actual_fps, Ordering::Relaxed);

                        frames_this_second = 0;
                        bytes_sent_recent = 0;
                        last_stats_time = Instant::now();
                    }
                }
                _ = sleep(Duration::from_millis(40)) => {
                    if last_ping_poll.elapsed() >= Duration::from_millis(100) {
                        last_ping_poll = Instant::now();
                        if publisher.poll_incoming().await.is_err() {
                            break;
                        }
                    }
                }
            }
        }

        // 6. Clean Teardown
        publisher.disconnect().await;
        self.telemetry.is_streaming.store(false, Ordering::Relaxed);
        self.telemetry
            .current_bitrate_kbps
            .store(0, Ordering::Relaxed);
        self.telemetry.fps.store(0, Ordering::Relaxed);
        let _ = self.status_tx.send("Stream stopped.".to_string());
        tracing::info!("RTMP stream session ended cleanly.");

        Ok(())
    }
}

/// Builds canonical AMF0 `@setDataFrame` `onMetaData`
pub fn build_on_metadata(width: f64, height: f64, fps: f64, video_bitrate: f64) -> Vec<u8> {
    let mut encoder = Amf0Encoder::new();
    encoder.encode(&AmfValue::String("@setDataFrame".into()));
    encoder.encode(&AmfValue::String("onMetaData".into()));

    let mut map = HashMap::new();
    map.insert("duration".to_string(), AmfValue::Number(0.0));
    map.insert("width".to_string(), AmfValue::Number(width));
    map.insert("height".to_string(), AmfValue::Number(height));
    map.insert("videodatarate".to_string(), AmfValue::Number(video_bitrate));
    map.insert("framerate".to_string(), AmfValue::Number(fps));
    map.insert("videocodecid".to_string(), AmfValue::Number(7.0)); // H.264
    map.insert("audiodatarate".to_string(), AmfValue::Number(128.0));
    map.insert("audiosamplerate".to_string(), AmfValue::Number(48000.0));
    map.insert("audiosamplesize".to_string(), AmfValue::Number(16.0));
    map.insert("stereo".to_string(), AmfValue::Boolean(true));
    map.insert("audiocodecid".to_string(), AmfValue::Number(10.0)); // AAC
    map.insert(
        "encoder".to_string(),
        AmfValue::String("ObsLite v0.5".into()),
    );

    encoder.encode(&AmfValue::Object(map));
    encoder.finish().to_vec()
}

/// Builds FLV AVCDecoderConfigurationRecord from parsed SPS and PPS
pub fn build_avcc_sequence_header(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut flv = Vec::with_capacity(32 + sps.len() + pps.len());
    flv.push(0x17); // Keyframe + AVC
    flv.push(0x00); // AVC sequence header
    flv.extend_from_slice(&[0x00, 0x00, 0x00]); // CompositionTime = 0

    flv.push(0x01); // configurationVersion = 1
    flv.push(sps.get(1).copied().unwrap_or(0x42)); // profile
    flv.push(sps.get(2).copied().unwrap_or(0xC0)); // profile_compat
    flv.push(sps.get(3).copied().unwrap_or(0x1F)); // level
    flv.push(0xFF); // lengthSizeMinusOne = 3 (4 bytes NAL length)

    flv.push(0xE1); // numOfSequenceParameterSets = 1
    flv.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    flv.extend_from_slice(sps);

    flv.push(0x01); // numOfPictureParameterSets = 1
    flv.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    flv.extend_from_slice(pps);

    flv
}

/// Splits Annex-B by 0x00 00 01 or 0x00 00 00 01
pub fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut i = 0;
    let len = data.len();

    while i < len {
        let start = if i + 4 <= len
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            i + 4
        } else if i + 3 <= len && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            i + 3
        } else {
            i += 1;
            continue;
        };

        let mut next_start = len;
        for j in start..len.saturating_sub(2) {
            if data[j] == 0 && data[j + 1] == 0 {
                if data[j + 2] == 1 {
                    next_start = j;
                    break;
                } else if j + 3 < len && data[j + 2] == 0 && data[j + 3] == 1 {
                    next_start = j;
                    break;
                }
            }
        }

        if start < next_start {
            nals.push(&data[start..next_start]);
        }
        i = next_start;
    }

    nals
}

/// Builds a raw AAC-LC audio frame in FLV format
pub fn build_aac_audio_frame() -> Vec<u8> {
    let mut packet = Vec::with_capacity(16);
    packet.push(0xAF); // AAC, 48kHz, 16-bit stereo
    packet.push(0x01); // Raw AAC frame data
    packet.extend_from_slice(&[0x21, 0x10, 0x04, 0x60, 0x8c, 0x1c]);
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_camera_video_realtime_encoding_fps_speed() {
        let width = 1280usize;
        let height = 720usize;
        let enc_config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(3_500_000))
            .max_frame_rate(FrameRate::from_hz(30.0))
            .rate_control_mode(RateControlMode::Bitrate)
            .usage_type(UsageType::CameraVideoRealTime)
            .complexity(Complexity::Low)
            .skip_frames(false)
            .intra_frame_period(IntraFramePeriod::from_num_frames(60));

        let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), enc_config)
            .expect("Encoder should initialize");
        let mut yuv = FastYuvBuffer::new(width, height);

        let start = Instant::now();
        let frames = 30u32;
        for f in 0..frames {
            yuv.render_standby_pattern(f as u64);
            let bitstream = encoder.encode(&yuv).expect("Encode should succeed");
            assert!(!bitstream.to_vec().is_empty());
        }
        let elapsed = start.elapsed().as_secs_f64();
        let achieved_fps = (frames as f64) / elapsed;
        println!(
            "Encoded {} 720p frames in {:.3}s ({:.1} FPS)",
            frames, elapsed, achieved_fps
        );
        assert!(
            achieved_fps >= 30.0,
            "Expected >= 30 FPS encode throughput, got {:.1} FPS",
            achieved_fps
        );
    }

    #[test]
    fn test_split_annex_b_and_avcc_header() {
        let annex_b = [
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xC0, 0x1F, 0x00, 0x00, 0x01, 0x68, 0xCE, 0x3C,
            0x80, 0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84,
        ];
        let nals = split_annex_b(&annex_b);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0][0] & 0x1F, 7); // SPS
        assert_eq!(nals[1][0] & 0x1F, 8); // PPS
        assert_eq!(nals[2][0] & 0x1F, 5); // IDR

        let header = build_avcc_sequence_header(nals[0], nals[1]);
        assert_eq!(header[0], 0x17);
        assert_eq!(header[1], 0x00);
    }

    #[test]
    fn test_live_device_hardware_scan_and_window_enum() {
        let report = crate::detect::run_hardware_scan();
        println!(
            "Live Hardware Scan -> CPU: {} ({}C/{}T, {} MB RAM), GPU: {:?}, Compatibility: {:?}",
            report.cpu.brand,
            report.cpu.physical_cores,
            report.cpu.logical_cores,
            report.cpu.total_memory_mb,
            report.gpu.as_ref().map(|g| (&g.name, g.dedicated_vram_mb, g.shared_memory_mb)),
            report.compatibility
        );
        assert!(report.cpu.logical_cores >= 1);

        // Test high-complexity / game-like moving frame encoding at 720p and 1080p
        for &(w, h, br) in &[(1280usize, 720usize, 3_500_000u32), (1920usize, 1080usize, 4_500_000u32)] {
            let enc_config = EncoderConfig::new()
                .bitrate(BitRate::from_bps(br))
                .max_frame_rate(FrameRate::from_hz(30.0))
                .rate_control_mode(RateControlMode::Bitrate)
                .usage_type(UsageType::CameraVideoRealTime)
                .complexity(Complexity::Low)
                .skip_frames(false)
                .adaptive_quantization(false)
                .background_detection(false)
                .scene_change_detect(false)
                .qp(openh264::encoder::QpRange::new(22, 42))
                .num_threads(0)
                .intra_frame_period(IntraFramePeriod::from_num_frames(60));
            let mut enc = Encoder::with_api_config(OpenH264API::from_source(), enc_config).unwrap();
            let mut yuv = FastYuvBuffer::new(w, h);
            let scene_w = w + 160;
            let mut fake_game_bgra = vec![0u8; scene_w * h * 4];
            for y in 0..h {
                let row = y * scene_w * 4;
                for x in 0..scene_w {
                    let idx = row + x * 4;
                    fake_game_bgra[idx] = ((x / 8 + y / 8) & 0xFF) as u8;
                    fake_game_bgra[idx + 1] = (((x / 16) * 23 + (y / 16) * 31) & 0xFF) as u8;
                    fake_game_bgra[idx + 2] = ((x / 4 + y / 12) & 0xFF) as u8;
                    fake_game_bgra[idx + 3] = 255;
                }
            }
            let start = Instant::now();
            let mut encoded_non_empty = 0u32;
            for f in 0..20u32 {
                // Pan fixed-width camera across game scene (8px per frame) to simulate real 30fps game motion
                let offset = (f as usize) * 8;
                yuv.convert_cropped_bgra_to_i420(
                    &fake_game_bgra,
                    scene_w as u32,
                    h as u32,
                    (offset as i32, 0, w as i32, h as i32),
                );
                let bs = enc.encode(&yuv).unwrap();
                if !bs.to_vec().is_empty() {
                    encoded_non_empty += 1;
                }
            }
            assert_eq!(encoded_non_empty, 20, "Zero frames should be skipped when skip_frames(false) is set");
            let fps = 20.0 / start.elapsed().as_secs_f64();
            println!("Simulated High-Motion Game Capture+Scale+Encode {}x{}: {:.1} FPS (20/20 non-skipped frames)", w, h, fps);
            let min_expected_fps = if w <= 1280 { 35.0 } else { 18.0 };
            assert!(fps >= min_expected_fps, "Expected >= {:.1} FPS real-time encoding throughput at {}x{}, got {:.1}", min_expected_fps, w, h, fps);
        }

        let windows = crate::win_enum::get_available_windows();
        println!("Enumerated {} active/minimized application windows", windows.len());
        for w in windows.iter().take(10) {
            println!("  Window: '{}' (HWND 0x{:X}, {:?}, min={})", w.title, w.hwnd, w.rect, w.is_minimized);
        }

        // Test capturing 'Plants vs. Zombies' (or first available window) from a spawned background worker thread
        if let Some(pvz) = windows.iter().find(|w| w.title.to_lowercase().contains("plants vs")).or_else(|| windows.first()) {
            // Verify that passing a wrong/stale HWND belonging to a different window resolves to the true PvZ HWND!
            if let Some(other) = windows.iter().find(|w| w.hwnd != pvz.hwnd && !w.title.eq_ignore_ascii_case(&pvz.title)) {
                let resolved = crate::win_enum::resolve_target_window(other.hwnd, &pvz.title).unwrap();
                assert_eq!(resolved.hwnd, pvz.hwnd, "resolve_target_window must reject a preferred_hwnd whose title does not match target_title!");
            }

            let title = pvz.title.clone();
            let hwnd = pvz.hwnd;
            std::thread::spawn(move || {
                let mut src = VideoFrameSource::new(1280, 720, "Window".to_string(), title.clone(), hwnd);
                let mut yuv = FastYuvBuffer::new(1280, 720);
                let start = Instant::now();
                for _ in 0..10 {
                    src.capture_into_yuv(&mut yuv);
                }
                let avg_ms = start.elapsed().as_secs_f64() * 100.0;
                println!(
                    "Worker thread Window Capture for '{}' (resolved HWND 0x{:X}, win_capturer {}x{}): has_content={}, avg {:.2} ms/frame",
                    title, src.target_window_hwnd, src.win_capturer.width, src.win_capturer.height, yuv.has_content, avg_ms
                );
                assert_eq!(src.target_window_hwnd, hwnd);
                assert!(yuv.has_content);
            }).join().unwrap();
        }

        // Verify AudioEngine initializes and starts native format audio monitoring
        let audio = crate::audio::AudioEngine::new();
        assert!(audio.is_ok());
    }

    #[test]
    fn test_aac_audio_clock_matches_wall_clock_video_timestamps() {
        // Simulate 10 seconds of wall-clock video timestamps even when frame delivery has jitter
        let mut audio_frame_index = 0u64;
        let simulated_video_timestamps_ms = [0u32, 33, 67, 120, 250, 500, 1000, 5000, 10_000];
        let mut last_audio_ts = 0u32;

        for &v_ts in &simulated_video_timestamps_ms {
            while ((audio_frame_index * 1024 * 1000) / 48_000) as u32 <= v_ts {
                let a_ts = ((audio_frame_index * 1024 * 1000) / 48_000) as u32;
                assert!(a_ts >= last_audio_ts);
                last_audio_ts = a_ts;
                audio_frame_index += 1;
            }
        }
        // In 10.000 seconds of 48kHz audio (1024 samples/frame), exactly 469 frames (0..=468) should be emitted
        assert_eq!(audio_frame_index, 469);
        assert!((9980..=10000).contains(&last_audio_ts));
    }

    #[tokio::test]
    async fn test_stream_sync_validation_edge_cases() {
        assert!(test_stream_sync("", "key123").await.is_err());
        assert!(test_stream_sync("rtmp://localhost/live", "   ").await.is_err());
        assert!(test_stream_sync("http://not-rtmp.com/live", "key123").await.is_err());
    }

    #[tokio::test]
    async fn test_end_to_end_rtmp_stream_session_and_sync() {
        use rtmp_rs::protocol::message::{ConnectParams, PublishParams};
        use rtmp_rs::session::SessionContext;
        use rtmp_rs::{AuthResult, RtmpHandler, RtmpServer, ServerConfig};

        struct AcceptAllHandler;
        impl RtmpHandler for AcceptAllHandler {
            async fn on_connect(&self, _ctx: &SessionContext, _params: &ConnectParams) -> AuthResult {
                AuthResult::Accept
            }
            async fn on_publish(&self, _ctx: &SessionContext, _params: &PublishParams) -> AuthResult {
                AuthResult::Accept
            }
        }

        // Bind an ephemeral localhost port
        let sock = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = sock.local_addr().unwrap();
        drop(sock);

        let server_cfg = ServerConfig::default().bind(addr);
        let server = RtmpServer::new(server_cfg, AcceptAllHandler);
        let server_handle = tokio::spawn(async move {
            let _ = server.run().await;
        });

        tokio::time::sleep(Duration::from_millis(80)).await;

        let server_url = format!("rtmp://{}/live", addr);
        let sync_res = test_stream_sync(&server_url, "alpha_v05_key").await;
        println!("E2E test_stream_sync result: {:?}", sync_res);
        assert!(sync_res.is_ok());

        // Now run a live StreamSession for 1.5 seconds in Window Mode
        let telemetry = LiveTelemetry::default();
        let (status_tx, mut status_rx) = mpsc::unbounded_channel();
        let stop_signal = Arc::new(AtomicBool::new(true));

        let session = StreamSession::new(
            server_url,
            "alpha_v05_key".to_string(),
            1280,
            720,
            30,
            3000,
            "Window".to_string(),
            "Plants vs. Zombies".to_string(),
            0,
            telemetry.clone(),
            status_tx,
        );

        let stop_clone = stop_signal.clone();
        let session_task = tokio::spawn(async move {
            session.run(stop_clone).await
        });

        tokio::time::sleep(Duration::from_millis(1500)).await;
        stop_signal.store(false, Ordering::Relaxed);

        let run_res = session_task.await.unwrap();
        println!("E2E session_task result: {:?}", run_res);
        while let Ok(msg) = status_rx.try_recv() {
            println!("  Session status: {}", msg);
        }
        assert!(run_res.is_ok());

        let total_sent = telemetry.total_frames.load(Ordering::Relaxed);
        let dropped = telemetry.dropped_frames.load(Ordering::Relaxed);
        println!(
            "E2E Live StreamSession (1.5s) -> Total Frames Sent: {}, Dropped: {}",
            total_sent, dropped
        );
        assert!(
            total_sent >= 20,
            "Expected at least 20 frames sent in 1.5s, got {}",
            total_sent
        );
        assert_eq!(dropped, 0);

        server_handle.abort();
    }
}
