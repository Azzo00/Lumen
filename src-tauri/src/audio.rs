//! Real audio visualizer: captures what the speakers are playing (WASAPI loopback),
//! runs an FFT and emits `audio` with `BANDS` log-spaced levels in 0..1 (~30 fps).
//! It goes quiet (one final all-zero frame) when nothing is playing, and restarts
//! itself when the default output device changes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use tauri::{AppHandle, Emitter};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};

pub const BANDS: usize = 12;
const FFT_SIZE: usize = 2048;
const MIN_HZ: f32 = 40.0;
const MAX_HZ: f32 = 14_000.0;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        loop {
            if let Err(e) = unsafe { capture(&app) } {
                eprintln!("[audio] {e:?}");
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

unsafe fn default_device_id(enumerator: &IMMDeviceEnumerator) -> windows::core::Result<String> {
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
    let id = device.GetId()?;
    let s = id.to_string().unwrap_or_default();
    CoTaskMemFree(Some(id.0 as *const _));
    Ok(s)
}

/// Runs until an error happens or the default output device changes.
unsafe fn capture(app: &AppHandle) -> windows::core::Result<()> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
    let device_id = default_device_id(&enumerator)?;
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;

    let fmt = client.GetMixFormat()?;
    let channels = (*fmt).nChannels as usize;
    let bits = (*fmt).wBitsPerSample;
    let tag = (*fmt).wFormatTag;
    let rate = (*fmt).nSamplesPerSec as f32;
    // 200 ms buffer, in 100 ns units.
    let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 2_000_000, 0, fmt, None);
    CoTaskMemFree(Some(fmt as *const _));
    init?;

    // Shared-mode mix formats are float32 in practice; 16-bit PCM is handled as a fallback.
    let is_float = bits == 32 && (tag == WAVE_FORMAT_IEEE_FLOAT || tag == WAVE_FORMAT_EXTENSIBLE);
    if channels == 0 || !(is_float || bits == 16) {
        return Err(windows::core::Error::empty());
    }

    let capture: IAudioCaptureClient = client.GetService()?;
    client.Start()?;

    let mut analyzer = Analyzer::new(rate);
    let mut last_emit = Instant::now();
    let mut last_signal = Instant::now();
    let mut last_device_check = Instant::now();
    let mut quiet_sent = false;

    loop {
        std::thread::sleep(Duration::from_millis(15));

        let mut got_data = false;
        while capture.GetNextPacketSize()? > 0 {
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            let silent = data.is_null() || flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
            for f in 0..frames as usize {
                let mut sum = 0.0f32;
                if !silent {
                    for c in 0..channels {
                        let i = f * channels + c;
                        sum += if is_float {
                            *(data as *const f32).add(i)
                        } else {
                            *(data as *const i16).add(i) as f32 / 32768.0
                        };
                    }
                }
                analyzer.push(sum / channels as f32);
            }
            capture.ReleaseBuffer(frames)?;
            got_data = true;
        }

        if got_data && last_emit.elapsed() >= Duration::from_millis(33) {
            last_emit = Instant::now();
            let bands = analyzer.bands();
            if bands.iter().any(|&b| b > 0.03) {
                last_signal = Instant::now();
                quiet_sent = false;
                let _ = app.emit("audio", &bands);
            }
        }
        if !quiet_sent && last_signal.elapsed() > Duration::from_millis(400) {
            let _ = app.emit("audio", [0.0f32; BANDS]);
            quiet_sent = true;
        }

        if last_device_check.elapsed() > Duration::from_secs(2) {
            last_device_check = Instant::now();
            if default_device_id(&enumerator)? != device_id {
                let _ = client.Stop();
                return Ok(());
            }
        }
    }
}

struct Analyzer {
    ring: Vec<f32>,
    pos: usize,
    fft: Arc<dyn Fft<f32>>,
    buf: Vec<Complex<f32>>,
    window: Vec<f32>,
    /// FFT bin boundaries of each band: band i covers edges[i]..edges[i + 1].
    edges: Vec<usize>,
}

impl Analyzer {
    fn new(rate: f32) -> Self {
        let window = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            .collect();
        let max_bin = FFT_SIZE / 2 - 1;
        let mut edges = Vec::with_capacity(BANDS + 1);
        for i in 0..=BANDS {
            let hz = MIN_HZ * (MAX_HZ / MIN_HZ).powf(i as f32 / BANDS as f32);
            let mut bin = ((hz * FFT_SIZE as f32 / rate).round() as usize).clamp(1, max_bin);
            if let Some(&prev) = edges.last() {
                bin = bin.max(prev + 1).min(max_bin + 1);
            }
            edges.push(bin);
        }
        Self {
            ring: vec![0.0; FFT_SIZE],
            pos: 0,
            fft: FftPlanner::new().plan_fft_forward(FFT_SIZE),
            buf: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            window,
            edges,
        }
    }

    fn push(&mut self, sample: f32) {
        self.ring[self.pos] = sample;
        self.pos = (self.pos + 1) % FFT_SIZE;
    }

    fn bands(&mut self) -> [f32; BANDS] {
        for i in 0..FFT_SIZE {
            let s = self.ring[(self.pos + i) % FFT_SIZE];
            self.buf[i] = Complex::new(s * self.window[i], 0.0);
        }
        self.fft.process(&mut self.buf);

        // Amplitude of a full-scale sine with a Hann window ≈ N/4 in its bin.
        let norm = 4.0 / FFT_SIZE as f32;
        let mut out = [0.0f32; BANDS];
        for (b, level) in out.iter_mut().enumerate() {
            let peak = self.buf[self.edges[b]..self.edges[b + 1]]
                .iter()
                .map(|c| c.norm())
                .fold(0.0f32, f32::max);
            let db = 20.0 * (peak * norm).max(1e-6).log10();
            // -60 dB → 0, -6 dB → 1, with a small tilt so highs aren't always flat.
            let tilt = b as f32 * 1.2;
            *level = ((db + 60.0 + tilt) / 54.0).clamp(0.0, 1.0);
        }
        out
    }
}
