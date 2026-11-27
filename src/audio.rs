use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Stream, StreamConfig};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

#[derive(Clone)]
pub struct AudioMixerControls {
    pub desktop_volume: Arc<AtomicU32>, // 0 - 100 percentage
    pub desktop_muted: Arc<AtomicBool>,
    pub desktop_peak_pct: Arc<AtomicU32>, // 0 - 100 live VU meter level
    pub mic_volume: Arc<AtomicU32>,       // 0 - 100 percentage
    pub mic_muted: Arc<AtomicBool>,
    pub mic_peak_pct: Arc<AtomicU32>, // 0 - 100 live VU meter level
}

impl Default for AudioMixerControls {
    fn default() -> Self {
        Self {
            desktop_volume: Arc::new(AtomicU32::new(100)),
            desktop_muted: Arc::new(AtomicBool::new(false)),
            desktop_peak_pct: Arc::new(AtomicU32::new(0)),
            mic_volume: Arc::new(AtomicU32::new(100)),
            mic_muted: Arc::new(AtomicBool::new(false)),
            mic_peak_pct: Arc::new(AtomicU32::new(0)),
        }
    }
}

pub struct AudioEngine {
    pub controls: AudioMixerControls,
    _desktop_stream: Option<Stream>,
    _mic_stream: Option<Stream>,
}

impl AudioEngine {
    pub fn new() -> Result<Self, String> {
        let controls = AudioMixerControls::default();

        let mut engine = Self {
            controls,
            _desktop_stream: None,
            _mic_stream: None,
        };
        engine.start_streams();
        Ok(engine)
    }

    pub fn start_streams(&mut self) {
        if self._mic_stream.is_some() || self._desktop_stream.is_some() {
            return;
        }

        let host = cpal::default_host();

        // 1. Microphone capture (query native device format so mono / 44.1kHz / i16 mics work)
        if let Some(device) = host.default_input_device() {
            let dev_name = device
                .description()
                .map(|d| d.name().to_string())
                .unwrap_or_else(|_| "Default Microphone".to_string());
            tracing::info!("Starting mic audio stream: {}", dev_name);

            if let Ok(supported_cfg) = device.default_input_config() {
                let sample_format = supported_cfg.sample_format();
                let config: StreamConfig = supported_cfg.into();
                let mic_vol = self.controls.mic_volume.clone();
                let mic_mute = self.controls.mic_muted.clone();
                let mic_peak = self.controls.mic_peak_pct.clone();

                let stream = Self::build_meter_stream(
                    &device,
                    config,
                    sample_format,
                    mic_vol,
                    mic_mute,
                    mic_peak,
                    "Mic",
                );

                if let Some(s) = stream {
                    let _ = s.play();
                    self._mic_stream = Some(s);
                }
            }
        }

        // 2. Desktop audio loopback (query native output config for WASAPI loopback)
        if let Some(device) = host.default_output_device() {
            let dev_name = device
                .description()
                .map(|d| d.name().to_string())
                .unwrap_or_else(|_| "Default Desktop Output".to_string());
            tracing::info!("Starting desktop audio loopback stream: {}", dev_name);

            let supported_opt = device
                .default_output_config()
                .or_else(|_| device.default_input_config());

            if let Ok(supported_cfg) = supported_opt {
                let sample_format = supported_cfg.sample_format();
                let config: StreamConfig = supported_cfg.into();
                let desk_vol = self.controls.desktop_volume.clone();
                let desk_mute = self.controls.desktop_muted.clone();
                let desk_peak = self.controls.desktop_peak_pct.clone();

                let stream = Self::build_meter_stream(
                    &device,
                    config,
                    sample_format,
                    desk_vol,
                    desk_mute,
                    desk_peak,
                    "Desktop loopback",
                );

                if let Some(s) = stream {
                    let _ = s.play();
                    self._desktop_stream = Some(s);
                }
            }
        }
    }

    fn build_meter_stream(
        device: &cpal::Device,
        config: StreamConfig,
        sample_format: cpal::SampleFormat,
        vol_atom: Arc<AtomicU32>,
        mute_atom: Arc<AtomicBool>,
        peak_atom: Arc<AtomicU32>,
        label: &'static str,
    ) -> Option<Stream> {
        let err_fn = move |err| tracing::warn!("{} audio stream error: {:?}", label, err);

        match sample_format {
            cpal::SampleFormat::I16 => device
                .build_input_stream(
                    config,
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        if mute_atom.load(Ordering::Relaxed) {
                            peak_atom.store(0, Ordering::Relaxed);
                            return;
                        }
                        let vol = (vol_atom.load(Ordering::Relaxed) as f32) / 100.0;
                        let mut max_abs = 0.0f32;
                        for &s in data.iter().step_by(4) {
                            let norm = (s as f32) / 32768.0;
                            let v = (norm * vol).abs();
                            if v > max_abs {
                                max_abs = v;
                            }
                        }
                        Self::update_peak(&peak_atom, max_abs);
                    },
                    err_fn,
                    None,
                )
                .ok(),
            cpal::SampleFormat::U16 => device
                .build_input_stream(
                    config,
                    move |data: &[u16], _: &cpal::InputCallbackInfo| {
                        if mute_atom.load(Ordering::Relaxed) {
                            peak_atom.store(0, Ordering::Relaxed);
                            return;
                        }
                        let vol = (vol_atom.load(Ordering::Relaxed) as f32) / 100.0;
                        let mut max_abs = 0.0f32;
                        for &s in data.iter().step_by(4) {
                            let norm = ((s as f32) - 32768.0) / 32768.0;
                            let v = (norm * vol).abs();
                            if v > max_abs {
                                max_abs = v;
                            }
                        }
                        Self::update_peak(&peak_atom, max_abs);
                    },
                    err_fn,
                    None,
                )
                .ok(),
            _ => device
                .build_input_stream(
                    config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        if mute_atom.load(Ordering::Relaxed) {
                            peak_atom.store(0, Ordering::Relaxed);
                            return;
                        }
                        let vol = (vol_atom.load(Ordering::Relaxed) as f32) / 100.0;
                        let mut max_abs = 0.0f32;
                        for &s in data.iter().step_by(4) {
                            let v = (s * vol).abs();
                            if v > max_abs {
                                max_abs = v;
                            }
                        }
                        Self::update_peak(&peak_atom, max_abs);
                    },
                    err_fn,
                    None,
                )
                .ok(),
        }
    }

    #[inline]
    fn update_peak(peak_atom: &AtomicU32, max_abs: f32) {
        let measured = ((max_abs.clamp(0.0, 1.0).sqrt() * 100.0) as u32).min(100);
        let prev = peak_atom.load(Ordering::Relaxed);
        let next = if measured >= prev {
            measured
        } else {
            (prev * 3) / 4
        };
        peak_atom.store(next, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn stop_streams(&mut self) {
        self._mic_stream = None;
        self._desktop_stream = None;
        self.controls.desktop_peak_pct.store(0, Ordering::Relaxed);
        self.controls.mic_peak_pct.store(0, Ordering::Relaxed);
        tracing::info!("Audio streams stopped.");
    }
}
