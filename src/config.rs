use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppConfig {
    pub platform_preset: String,
    pub server_url: String,
    pub stream_key: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    #[serde(default = "default_quality_preset")]
    pub quality_preset: String, // "Auto", "1080p60", "1080p30", "720p60", "720p30", "540p30"
    pub desktop_volume: u32,
    pub desktop_muted: bool,
    pub mic_volume: u32,
    pub mic_muted: bool,
    #[serde(default = "default_capture_mode")]
    pub capture_mode: String, // "Desktop" or "Window"
    #[serde(default)]
    pub target_window_title: String,
    #[serde(default)]
    pub target_window_hwnd: isize,
}

fn default_capture_mode() -> String {
    "Desktop".to_string()
}

fn default_quality_preset() -> String {
    "Auto".to_string()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            platform_preset: "Facebook".to_string(),
            server_url: "rtmps://live-api-s.facebook.com:443/rtmp/".to_string(),
            stream_key: String::new(),
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 3000,
            quality_preset: "Auto".to_string(),
            desktop_volume: 100,
            desktop_muted: false,
            mic_volume: 100,
            mic_muted: false,
            capture_mode: "Desktop".to_string(),
            target_window_title: String::new(),
            target_window_hwnd: 0,
        }
    }
}

impl AppConfig {
    pub const FILE_NAME: &'static str = "obslite.json";

    pub fn load() -> Self {
        let path = Path::new(Self::FILE_NAME);
        if path.exists() {
            if let Ok(data) = fs::read_to_string(path) {
                if let Ok(mut cfg) = serde_json::from_str::<AppConfig>(&data) {
                    cfg.sanitize();
                    return cfg;
                }
            }
        }
        Self::default()
    }

    pub fn sanitize(&mut self) {
        if self.width < 320 || self.height < 240 {
            self.width = 1280;
            self.height = 720;
        }
        // Ensure dimensions are even for YUV420 encoding
        self.width = (self.width / 2) * 2;
        self.height = (self.height / 2) * 2;
        self.fps = self.fps.clamp(15, 60);
        self.bitrate_kbps = self.bitrate_kbps.clamp(500, 12000);
        self.desktop_volume = self.desktop_volume.min(100);
        self.mic_volume = self.mic_volume.min(100);
        if self.capture_mode != "Desktop" && self.capture_mode != "Window" {
            self.capture_mode = "Desktop".to_string();
        }
    }

    pub fn apply_quality_preset(&mut self, hw_width: u32, hw_height: u32, hw_fps: u32, hw_bitrate: u32) {
        match self.quality_preset.as_str() {
            "1080p60" => {
                self.width = 1920;
                self.height = 1080;
                self.fps = 60;
                self.bitrate_kbps = 6000;
            }
            "1080p30" => {
                self.width = 1920;
                self.height = 1080;
                self.fps = 30;
                self.bitrate_kbps = 4000;
            }
            "720p60" => {
                self.width = 1280;
                self.height = 720;
                self.fps = 60;
                self.bitrate_kbps = 3500;
            }
            "720p30" => {
                self.width = 1280;
                self.height = 720;
                self.fps = 30;
                self.bitrate_kbps = 2500;
            }
            "540p30" => {
                self.width = 960;
                self.height = 540;
                self.fps = 30;
                self.bitrate_kbps = 1500;
            }
            _ => {
                // "Auto" uses hardware pre-flight recommended profile
                self.width = hw_width;
                self.height = hw_height;
                self.fps = hw_fps;
                self.bitrate_kbps = hw_bitrate;
            }
        }
    }

    pub fn save(&self) -> Result<(), std::io::Error> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        fs::write(Self::FILE_NAME, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_sanitize_and_presets() {
        let mut cfg = AppConfig {
            width: 100,
            height: 101,
            fps: 120,
            bitrate_kbps: 100,
            capture_mode: "InvalidMode".into(),
            ..Default::default()
        };
        cfg.sanitize();
        assert_eq!(cfg.width, 1280);
        assert_eq!(cfg.height, 720);
        assert_eq!(cfg.fps, 60);
        assert_eq!(cfg.bitrate_kbps, 500);
        assert_eq!(cfg.capture_mode, "Desktop");

        cfg.quality_preset = "1080p30".into();
        cfg.apply_quality_preset(1280, 720, 30, 2500);
        assert_eq!((cfg.width, cfg.height, cfg.fps, cfg.bitrate_kbps), (1920, 1080, 30, 4000));

        cfg.quality_preset = "Auto".into();
        cfg.apply_quality_preset(1920, 1080, 60, 5000);
        assert_eq!((cfg.width, cfg.height, cfg.fps, cfg.bitrate_kbps), (1920, 1080, 60, 5000));
    }
}
