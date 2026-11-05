//! CPU, memory, system disk, network throughput and GPU temperature for the "PC" view.
//! Sampled on demand: the frontend asks only while that view is open.
//!
//! Temperatures: Windows gives no CPU temperature to a normal user (the ACPI thermal
//! zone needs admin rights and is not supported on many laptops; real CPU sensors need a
//! kernel driver), so only the NVIDIA GPU is read, through nvidia-smi. It runs on its own
//! thread at most every 3 s so the 1.5 s sampling is never slowed down.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use sysinfo::{Disks, Networks, System};

struct Sampler {
    sys: System,
    disks: Disks,
    nets: Networks,
    last: Instant,
}

static SAMPLER: Mutex<Option<Sampler>> = Mutex::new(None);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PcStats {
    cpu: f32,
    cpu_name: String,
    cores: usize,
    ram_used: u64,
    ram_total: u64,
    disk_name: String,
    disk_used: u64,
    disk_total: u64,
    /// bytes per second
    net_down: f64,
    net_up: f64,
    gpu_name: Option<String>,
    /// °C
    gpu_temp: Option<f32>,
}

struct Gpu {
    checked: Option<Instant>,
    /// nvidia-smi missing or failing: stop trying
    unavailable: bool,
    reading: Option<(String, f32)>,
}
static GPU: Mutex<Gpu> = Mutex::new(Gpu { checked: None, unavailable: false, reading: None });
static GPU_QUERYING: AtomicBool = AtomicBool::new(false);

/// Last GPU reading; starts a new one in the background when it is older than 3 s.
fn gpu_temperature() -> Option<(String, f32)> {
    let mut g = GPU.lock().ok()?;
    let stale = g.checked.is_none_or(|t| t.elapsed() > Duration::from_secs(3));
    if !g.unavailable && stale && !GPU_QUERYING.swap(true, Ordering::SeqCst) {
        g.checked = Some(Instant::now());
        std::thread::spawn(|| {
            let r = query_nvidia();
            if let Ok(mut g) = GPU.lock() {
                match r {
                    Some(v) => g.reading = Some(v),
                    None => {
                        g.reading = None;
                        g.unavailable = true;
                    }
                }
            }
            GPU_QUERYING.store(false, Ordering::SeqCst);
        });
    }
    g.reading.clone()
}

fn query_nvidia() -> Option<(String, f32)> {
    let out = Command::new("nvidia-smi")
        .args(["--query-gpu=name,temperature.gpu", "--format=csv,noheader,nounits"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_nvidia(&String::from_utf8_lossy(&out.stdout))
}

/// "NVIDIA RTX 1000 Ada Generation Laptop GPU, 57" → (name, 57.0); first GPU only.
fn parse_nvidia(s: &str) -> Option<(String, f32)> {
    let line = s.lines().find(|l| !l.trim().is_empty())?;
    let (name, temp) = line.rsplit_once(',')?;
    let temp: f32 = temp.trim().parse().ok()?;
    let name = name.trim().trim_start_matches("NVIDIA ").replace(" Generation", "").replace(" Laptop GPU", "");
    Some((name, temp))
}

/// Virtual adapters (WSL, Hyper-V, VPN tunnels) would count the same traffic twice.
fn is_virtual(name: &str) -> bool {
    let n = name.to_lowercase();
    ["vethernet", "loopback", "fortinet", "vpn", "virtual", "hyper-v", "wsl", "tap", "tun"]
        .iter()
        .any(|k| n.contains(k))
}

#[tauri::command]
pub async fn pc_stats() -> Result<PcStats, String> {
    let mut guard = SAMPLER.lock().map_err(|e| e.to_string())?;
    let s = guard.get_or_insert_with(|| {
        let mut sys = System::new();
        sys.refresh_cpu_all();
        Sampler {
            sys,
            disks: Disks::new_with_refreshed_list(),
            nets: Networks::new_with_refreshed_list(),
            last: Instant::now(),
        }
    });

    s.sys.refresh_cpu_usage();
    s.sys.refresh_memory();
    s.disks.refresh(true);
    s.nets.refresh(true);
    let secs = s.last.elapsed().as_secs_f64().max(0.2);
    s.last = Instant::now();

    let (down, up) = s
        .nets
        .iter()
        .filter(|(name, _)| !is_virtual(name))
        .fold((0u64, 0u64), |(d, u), (_, n)| (d + n.received(), u + n.transmitted()));

    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let disk = s
        .disks
        .iter()
        .find(|d| d.mount_point().to_string_lossy().to_uppercase().starts_with(&system_drive.to_uppercase()))
        .or_else(|| s.disks.iter().next());
    let (disk_name, disk_used, disk_total) = disk
        .map(|d| {
            let mount = d.mount_point().to_string_lossy().trim_end_matches('\\').to_string();
            (mount, d.total_space() - d.available_space(), d.total_space())
        })
        .unwrap_or_default();

    let gpu = gpu_temperature();
    Ok(PcStats {
        cpu: s.sys.global_cpu_usage(),
        cpu_name: s.sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default(),
        cores: s.sys.cpus().len(),
        ram_used: s.sys.used_memory(),
        ram_total: s.sys.total_memory(),
        disk_name,
        disk_used,
        disk_total,
        net_down: down as f64 / secs,
        net_up: up as f64 / secs,
        gpu_name: gpu.as_ref().map(|g| g.0.clone()),
        gpu_temp: gpu.map(|g| g.1),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_nvidia;

    #[test]
    fn nvidia_output() {
        assert_eq!(
            parse_nvidia("NVIDIA RTX 1000 Ada Generation Laptop GPU, 57\r\n"),
            Some(("RTX 1000 Ada".to_string(), 57.0))
        );
        assert_eq!(parse_nvidia("NVIDIA GeForce RTX 4070, 41\nNVIDIA GeForce RTX 4070, 39\n").map(|g| g.1), Some(41.0));
        assert_eq!(parse_nvidia("NVIDIA X, [N/A]"), None);
        assert_eq!(parse_nvidia(""), None);
    }
}
