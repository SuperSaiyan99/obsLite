use serde::{Deserialize, Serialize};
use std::time::Instant;
use sysinfo::System;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuInfo {
    pub brand: String,
    pub logical_cores: usize,
    pub physical_cores: usize,
    pub frequency_mhz: u64,
    pub total_memory_mb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuInfo {
    pub name: String,
    pub dedicated_vram_mb: u64,
    pub shared_memory_mb: u64,
    pub dxgi_duplication_supported: bool,
    pub d3d11_feature_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncoderInfo {
    pub hardware_h264_supported: bool,
    pub hardware_encoder_name: Option<String>,
    pub software_encoder_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamProfile {
    pub tier_name: String,
    pub tier_badge: String,
    pub hardware_score: u32,
    pub measured_fps_capacity: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_bitrate_kbps: u32,
    pub audio_bitrate_kbps: u32,
    pub keyframe_interval_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompatibilityResult {
    Compatible {
        profile: StreamProfile,
        summary: String,
    },
    Incompatible {
        reasons: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareScanReport {
    pub cpu: CpuInfo,
    pub gpu: Option<GpuInfo>,
    pub encoder: EncoderInfo,
    pub compatibility: CompatibilityResult,
}

/// Runs a fast live `FastYuvBuffer` (BGRA->I420) + `OpenH264` H.264 encoding benchmark on the host CPU
/// to measure actual real-time frame processing and encoding throughput on this device.
pub fn benchmark_device_fps_capacity() -> u32 {
    use openh264::encoder::{
        BitRate, Complexity, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, QpRange,
        RateControlMode, UsageType,
    };
    use openh264::OpenH264API;

    let w = 1280usize;
    let h = 720usize;
    let mut src_bgra = vec![0u8; w * h * 4];
    for y in 0..h {
        let row = y * w * 4;
        for x in 0..w {
            let idx = row + x * 4;
            src_bgra[idx] = ((x / 8 + y / 8) & 0xFF) as u8;
            src_bgra[idx + 1] = ((x / 4) & 0xFF) as u8;
            src_bgra[idx + 2] = ((y / 4) & 0xFF) as u8;
            src_bgra[idx + 3] = 255;
        }
    }

    let mut yuv = crate::capture::FastYuvBuffer::new(w, h);
    let enc_config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(3_500_000))
        .max_frame_rate(FrameRate::from_hz(30.0))
        .rate_control_mode(RateControlMode::Bitrate)
        .usage_type(UsageType::CameraVideoRealTime)
        .complexity(Complexity::Low)
        .skip_frames(false)
        .adaptive_quantization(false)
        .background_detection(false)
        .scene_change_detect(false)
        .qp(QpRange::new(22, 42))
        .num_threads(0)
        .intra_frame_period(IntraFramePeriod::from_num_frames(60));

    if let Ok(mut encoder) = Encoder::with_api_config(OpenH264API::from_source(), enc_config) {
        // Warm up encoder initialization on frame 0 (SPS/PPS + IDR)
        yuv.convert_cropped_bgra_to_i420(&src_bgra, w as u32, h as u32, (0, 0, w as i32, h as i32));
        let _ = encoder.encode(&yuv);

        let frames = 5u32;
        let start = Instant::now();
        for f in 1..=frames {
            // Simulate moving object across the frame
            let box_x = ((f as usize) * 32) % (w - 128);
            for by in 200..328 {
                let row = by * w * 4;
                for bx in box_x..(box_x + 128) {
                    let idx = row + bx * 4;
                    src_bgra[idx] = 220;
                    src_bgra[idx + 1] = 140;
                    src_bgra[idx + 2] = 60;
                }
            }
            yuv.convert_cropped_bgra_to_i420(&src_bgra, w as u32, h as u32, (0, 0, w as i32, h as i32));
            if let Ok(bs) = encoder.encode(&yuv) {
                std::hint::black_box(bs.raw_info());
            }
        }
        let elapsed_secs = start.elapsed().as_secs_f64().max(0.001);
        let measured_fps = (frames as f64 / elapsed_secs).round() as u32;
        return measured_fps.clamp(24, 240);
    }

    60
}

/// Evaluates device hardware specs and benchmark capacity to assign an accurate hardware profile.
pub fn evaluate_hardware_profile(
    cpu: &CpuInfo,
    gpu: Option<&GpuInfo>,
    encoder: &EncoderInfo,
    measured_fps_capacity: u32,
) -> CompatibilityResult {
    let mut incompatible_reasons = Vec::new();

    if cpu.logical_cores < 2 {
        incompatible_reasons.push(format!(
            "CPU has only {} logical core(s). Minimum 2 cores required for real-time streaming.",
            cpu.logical_cores
        ));
    }

    if let Some(g) = gpu {
        if !g.dxgi_duplication_supported {
            incompatible_reasons.push(
                "Neither DXGI Desktop Duplication nor Windows Graphics Capture is supported by the display driver."
                    .to_string(),
            );
        }
    } else {
        incompatible_reasons.push("No compatible DirectX 11 GPU display adapter found.".to_string());
    }

    if !encoder.hardware_h264_supported && cpu.logical_cores < 4 {
        incompatible_reasons.push(
            "No Hardware H.264 Video Encoder detected and CPU has fewer than 4 logical cores for software fallback."
                .to_string(),
        );
    }

    if !incompatible_reasons.is_empty() {
        return CompatibilityResult::Incompatible {
            reasons: incompatible_reasons,
        };
    }

    let gpu_ref = gpu.unwrap();
    // Effective GPU memory considers both dedicated VRAM and shared system GPU memory (for modern APUs/iGPUs)
    let effective_vram_mb = gpu_ref.dedicated_vram_mb + (gpu_ref.shared_memory_mb / 2);

    // Compute a holistic 0-100 hardware capability score based on actual device capabilities
    let core_score = ((cpu.logical_cores as u32) * 4 + (cpu.physical_cores as u32) * 4).min(36);
    let ram_score = ((cpu.total_memory_mb / 1024) as u32 * 2).min(20);
    let gpu_score = if gpu_ref.dedicated_vram_mb >= 4096 {
        24
    } else if gpu_ref.dedicated_vram_mb >= 2048 || effective_vram_mb >= 4096 {
        18
    } else if effective_vram_mb >= 2048 {
        14
    } else {
        8
    };
    let enc_score = if encoder.hardware_h264_supported { 10 } else { 4 };
    let bench_score = (measured_fps_capacity / 10).min(10);
    let hardware_score = (core_score + ram_score + gpu_score + enc_score + bench_score).min(100);

    let profile = if hardware_score >= 80
        && cpu.logical_cores >= 8
        && (gpu_ref.dedicated_vram_mb >= 3072 || effective_vram_mb >= 6144)
    {
        // 1080p has 2.25x the macroblocks of 720p; require >= 95 FPS 720p software encode capacity
        // to default to 1080p30 so 6-core gaming rigs get a locked 30.0 FPS zero-drop 720p30 Ultra stream by default.
        let use_1080p = measured_fps_capacity >= 95;
        StreamProfile {
            tier_name: if use_1080p {
                "High-Performance Tier (1080p30 Ultra / 60fps Capable)".to_string()
            } else {
                "High-Performance Tier (720p30 Ultra / 1080p Capable)".to_string()
            },
            tier_badge: "HIGH-PERFORMANCE".to_string(),
            hardware_score,
            measured_fps_capacity,
            width: if use_1080p { 1920 } else { 1280 },
            height: if use_1080p { 1080 } else { 720 },
            fps: 30,
            video_bitrate_kbps: if use_1080p { 4500 } else { 4000 },
            audio_bitrate_kbps: 128,
            keyframe_interval_seconds: 2,
        }
    } else if hardware_score >= 55
        || (cpu.logical_cores >= 6 && effective_vram_mb >= 2048)
        || (cpu.physical_cores >= 4 && cpu.total_memory_mb >= 7500 && effective_vram_mb >= 1536)
    {
        StreamProfile {
            tier_name: "Mid-Range Tier (1080p30 / 720p60 Balanced)".to_string(),
            tier_badge: "MID-RANGE READY".to_string(),
            hardware_score,
            measured_fps_capacity,
            width: 1280,
            height: 720,
            fps: 30,
            video_bitrate_kbps: 3500,
            audio_bitrate_kbps: 128,
            keyframe_interval_seconds: 2,
        }
    } else if cpu.logical_cores >= 4 && cpu.total_memory_mb >= 4000 {
        StreamProfile {
            tier_name: "Standard Tier (720p30 HD Stream)".to_string(),
            tier_badge: "STANDARD READY".to_string(),
            hardware_score,
            measured_fps_capacity,
            width: 1280,
            height: 720,
            fps: 30,
            video_bitrate_kbps: 2500,
            audio_bitrate_kbps: 128,
            keyframe_interval_seconds: 2,
        }
    } else {
        StreamProfile {
            tier_name: "Entry Tier (720p30 Efficiency)".to_string(),
            tier_badge: "ENTRY TIER".to_string(),
            hardware_score,
            measured_fps_capacity,
            width: 1280,
            height: 720,
            fps: 30,
            video_bitrate_kbps: 2000,
            audio_bitrate_kbps: 128,
            keyframe_interval_seconds: 2,
        }
    };

    let summary = format!(
        "Score {}/100 ({} FPS capacity) — {} ({}x{} @ {}fps, {} kbps)",
        profile.hardware_score,
        profile.measured_fps_capacity,
        profile.tier_name,
        profile.width,
        profile.height,
        profile.fps,
        profile.video_bitrate_kbps
    );

    CompatibilityResult::Compatible { profile, summary }
}

#[cfg(windows)]
mod win_scanner {
    use super::*;
    use windows::core::Interface;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, ID3D11Device,
    };
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput1};
    use windows::Win32::Media::MediaFoundation::{
        MFShutdown, MFStartup, MFTEnumEx, MFVideoFormat_H264, MFT_CATEGORY_VIDEO_ENCODER,
        MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SYNCMFT, MFT_REGISTER_TYPE_INFO, MFSTARTUP_NOSOCKET,
        MF_VERSION,
    };
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
    };

    pub fn scan_cpu() -> CpuInfo {
        let mut sys = System::new_all();
        sys.refresh_cpu_all();
        sys.refresh_memory();

        let brand = sys
            .cpus()
            .first()
            .map(|c| c.brand().trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "x86_64 Processor".to_string());

        let frequency_mhz = sys.cpus().first().map(|c| c.frequency()).unwrap_or(0);
        let logical_cores = sys.cpus().len();
        let physical_cores = sys.physical_core_count().unwrap_or(logical_cores);
        let total_memory_mb = sys.total_memory() / (1024 * 1024);

        CpuInfo {
            brand,
            logical_cores,
            physical_cores,
            frequency_mhz,
            total_memory_mb,
        }
    }

    pub fn scan_gpu() -> Option<GpuInfo> {
        unsafe {
            let factory: IDXGIFactory1 = match CreateDXGIFactory1() {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!("Failed to create DXGIFactory1: {:?}", e);
                    return None;
                }
            };

            let wgc_supported = windows::Graphics::Capture::GraphicsCaptureSession::IsSupported()
                .unwrap_or(false);

            let mut best_gpu: Option<GpuInfo> = None;
            let mut any_duplication_ok = wgc_supported;

            let mut adapter_index = 0;
            while let Ok(adapter) = factory.EnumAdapters1(adapter_index) {
                adapter_index += 1;
                let desc = match adapter.GetDesc1() {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                // Skip Microsoft Basic Render Driver / software rasterizers (DXGI_ADAPTER_FLAG_SOFTWARE = 2)
                if (desc.Flags & 2) != 0 {
                    continue;
                }

                let name = String::from_utf16_lossy(&desc.Description)
                    .trim_matches('\0')
                    .trim()
                    .to_string();

                let dedicated_vram_mb = (desc.DedicatedVideoMemory as u64) / (1024 * 1024);
                let shared_memory_mb = (desc.SharedSystemMemory as u64) / (1024 * 1024);

                let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
                let mut obtained_feature_level = D3D_FEATURE_LEVEL(0);
                let mut device: Option<ID3D11Device> = None;

                let d3d_res = D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&feature_levels),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    Some(&mut obtained_feature_level),
                    None,
                );

                let (d3d11_ok, feature_str) = match d3d_res {
                    Ok(_) => (
                        true,
                        format!("Direct3D 11 ({:x})", obtained_feature_level.0),
                    ),
                    Err(e) => (false, format!("Failed ({:?})", e)),
                };

                if !d3d11_ok {
                    continue;
                }

                let mut this_dup_ok = wgc_supported;
                if let Some(dev) = device {
                    use windows::Win32::System::StationsAndDesktops::{
                        OpenInputDesktop, SetThreadDesktop, DESKTOP_ACCESS_FLAGS,
                        DESKTOP_CONTROL_FLAGS,
                    };
                    if let Ok(desktop) = OpenInputDesktop(
                        DESKTOP_CONTROL_FLAGS(0),
                        false,
                        DESKTOP_ACCESS_FLAGS(0x01FF),
                    ) {
                        let _ = SetThreadDesktop(desktop);
                    }

                    if let Ok(output) = adapter.EnumOutputs(0) {
                        if let Ok(output1) = output.cast::<IDXGIOutput1>() {
                            if let Ok(dup) = output1.DuplicateOutput(&dev) {
                                this_dup_ok = true;
                                drop(dup);
                            }
                        }
                    }
                }

                if this_dup_ok {
                    any_duplication_ok = true;
                }

                let candidate = GpuInfo {
                    name,
                    dedicated_vram_mb,
                    shared_memory_mb,
                    dxgi_duplication_supported: any_duplication_ok,
                    d3d11_feature_level: feature_str,
                };

                // Keep the strongest hardware GPU (preferring higher dedicated VRAM + shared memory)
                let replace = match &best_gpu {
                    None => true,
                    Some(prev) => {
                        let prev_mem = prev.dedicated_vram_mb * 2 + prev.shared_memory_mb;
                        let cand_mem = candidate.dedicated_vram_mb * 2 + candidate.shared_memory_mb;
                        cand_mem > prev_mem
                    }
                };

                if replace {
                    best_gpu = Some(candidate);
                }
            }

            if let Some(ref mut gpu) = best_gpu {
                gpu.dxgi_duplication_supported = any_duplication_ok;
            }

            best_gpu
        }
    }

    pub fn scan_encoder() -> EncoderInfo {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) {
                tracing::warn!("Failed MFStartup: {:?}", e);
                CoUninitialize();
                return EncoderInfo {
                    hardware_h264_supported: false,
                    hardware_encoder_name: None,
                    software_encoder_name: Some("OpenH264 Built-In Fast SIMD Encoder".to_string()),
                };
            }

            let output_type = MFT_REGISTER_TYPE_INFO {
                guidMajorType: windows::Win32::Media::MediaFoundation::MFMediaType_Video,
                guidSubtype: MFVideoFormat_H264,
            };

            let mut hw_activate_ptrs = std::ptr::null_mut();
            let mut hw_count = 0u32;
            let hw_res = MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE,
                None,
                Some(&output_type),
                &mut hw_activate_ptrs,
                &mut hw_count,
            );

            let mut hw_name: Option<String> = None;
            let hw_supported = if hw_res.is_ok() && hw_count > 0 && !hw_activate_ptrs.is_null() {
                let activates = std::slice::from_raw_parts(hw_activate_ptrs, hw_count as usize);
                if let Some(first) = activates.first().and_then(|a| a.as_ref()) {
                    let mut name_buf = [0u16; 256];
                    let mut name_len = 0u32;
                    if first
                        .GetString(
                            &windows::Win32::Media::MediaFoundation::MFT_FRIENDLY_NAME_Attribute,
                            &mut name_buf,
                            Some(&mut name_len),
                        )
                        .is_ok()
                    {
                        hw_name = Some(
                            String::from_utf16_lossy(&name_buf[..name_len as usize])
                                .trim_matches('\0')
                                .to_string(),
                        );
                    }
                }
                CoTaskMemFree(Some(hw_activate_ptrs as _));
                true
            } else {
                false
            };

            let mut sw_activate_ptrs = std::ptr::null_mut();
            let mut sw_count = 0u32;
            let _ = MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_SYNCMFT,
                None,
                Some(&output_type),
                &mut sw_activate_ptrs,
                &mut sw_count,
            );

            let mut sw_name: Option<String> = None;
            if sw_count > 0 && !sw_activate_ptrs.is_null() {
                let activates = std::slice::from_raw_parts(sw_activate_ptrs, sw_count as usize);
                if let Some(first) = activates.first().and_then(|a| a.as_ref()) {
                    let mut name_buf = [0u16; 256];
                    let mut name_len = 0u32;
                    if first
                        .GetString(
                            &windows::Win32::Media::MediaFoundation::MFT_FRIENDLY_NAME_Attribute,
                            &mut name_buf,
                            Some(&mut name_len),
                        )
                        .is_ok()
                    {
                        sw_name = Some(
                            String::from_utf16_lossy(&name_buf[..name_len as usize])
                                .trim_matches('\0')
                                .to_string(),
                        );
                    }
                }
                CoTaskMemFree(Some(sw_activate_ptrs as _));
            }

            let _ = MFShutdown();
            CoUninitialize();

            EncoderInfo {
                hardware_h264_supported: hw_supported,
                hardware_encoder_name: hw_name.or_else(|| {
                    if hw_supported {
                        Some("DirectX / Windows MFT Hardware H.264 Encoder".to_string())
                    } else {
                        None
                    }
                }),
                software_encoder_name: sw_name
                    .or_else(|| Some("OpenH264 Real-Time SIMD Encoder".to_string())),
            }
        }
    }
}

pub fn run_hardware_scan() -> HardwareScanReport {
    std::thread::spawn(|| {
        let measured_fps_capacity = benchmark_device_fps_capacity();

        #[cfg(windows)]
        {
            let cpu = win_scanner::scan_cpu();
            let gpu = win_scanner::scan_gpu();
            let encoder = win_scanner::scan_encoder();
            let compatibility =
                evaluate_hardware_profile(&cpu, gpu.as_ref(), &encoder, measured_fps_capacity);

            HardwareScanReport {
                cpu,
                gpu,
                encoder,
                compatibility,
            }
        }

        #[cfg(not(windows))]
        {
            HardwareScanReport {
                cpu: CpuInfo {
                    brand: "Non-Windows Host".to_string(),
                    logical_cores: 1,
                    physical_cores: 1,
                    frequency_mhz: 0,
                    total_memory_mb: 0,
                },
                gpu: None,
                encoder: EncoderInfo {
                    hardware_h264_supported: false,
                    hardware_encoder_name: None,
                    software_encoder_name: None,
                },
                compatibility: CompatibilityResult::Incompatible {
                    reasons: vec![
                        "ObsLite v0.5 currently targets Windows DXGI & Media Foundation.".to_string(),
                    ],
                },
            }
        }
    })
    .join()
    .expect("Hardware scan thread failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mid_range_pc_not_classified_as_low_end() {
        // Decent/mid-level PC: 6-core/12-thread or 4-core/8-thread with 16GB RAM and 1GB dedicated + 8GB shared GPU memory
        let cpu = CpuInfo {
            brand: "AMD Ryzen 5 5600G".to_string(),
            logical_cores: 12,
            physical_cores: 6,
            frequency_mhz: 3900,
            total_memory_mb: 16384,
        };
        let gpu = GpuInfo {
            name: "AMD Radeon Graphics".to_string(),
            dedicated_vram_mb: 512,
            shared_memory_mb: 8192,
            dxgi_duplication_supported: true,
            d3d11_feature_level: "Direct3D 11 (b100)".to_string(),
        };
        let encoder = EncoderInfo {
            hardware_h264_supported: true,
            hardware_encoder_name: Some("AMD H264 Hardware MFT".to_string()),
            software_encoder_name: None,
        };

        let res = evaluate_hardware_profile(&cpu, Some(&gpu), &encoder, 90);
        match res {
            CompatibilityResult::Compatible { profile, .. } => {
                assert!(
                    profile.tier_badge == "MID-RANGE READY"
                        || profile.tier_badge == "HIGH-PERFORMANCE",
                    "Expected mid-range or high-performance tier, got: {}",
                    profile.tier_name
                );
                assert!(profile.hardware_score >= 60);
            }
            CompatibilityResult::Incompatible { reasons } => {
                panic!("Expected compatible, got incompatible: {:?}", reasons);
            }
        }
    }

    #[test]
    fn test_high_spec_pc_classification() {
        let cpu = CpuInfo {
            brand: "Intel Core i7-12700K".to_string(),
            logical_cores: 16,
            physical_cores: 8,
            frequency_mhz: 3600,
            total_memory_mb: 32768,
        };
        let gpu = GpuInfo {
            name: "NVIDIA GeForce RTX 3060".to_string(),
            dedicated_vram_mb: 12288,
            shared_memory_mb: 16384,
            dxgi_duplication_supported: true,
            d3d11_feature_level: "Direct3D 11 (b100)".to_string(),
        };
        let encoder = EncoderInfo {
            hardware_h264_supported: true,
            hardware_encoder_name: Some("NVIDIA H.264 Encoder MFT".to_string()),
            software_encoder_name: None,
        };

        let res = evaluate_hardware_profile(&cpu, Some(&gpu), &encoder, 120);
        match res {
            CompatibilityResult::Compatible { profile, .. } => {
                assert_eq!(profile.tier_badge, "HIGH-PERFORMANCE");
                assert_eq!((profile.width, profile.height), (1920, 1080));
            }
            _ => panic!("Expected high-performance compatible"),
        }
    }
}
