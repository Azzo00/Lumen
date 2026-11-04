//! CPU, memory, system disk, network throughput and uptime for the "PC" view.
//! Sampled on demand: the frontend asks only while that view is open.

use std::sync::Mutex;
use std::time::Instant;

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
    uptime_secs: u64,
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
        uptime_secs: System::uptime(),
    })
}
