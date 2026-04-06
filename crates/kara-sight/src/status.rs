/// System status polling with throttled updates.
///
/// Reads volume, network, battery, brightness, media, and memory state
/// from /proc, /sys, and external tools (wpctl, playerctl).

use std::fs;
use std::io::{BufRead, BufReader};
use std::process::Command;
use std::time::Instant;

// ── State types ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct VolumeState {
    pub valid: bool,
    pub muted: bool,
    pub percent: i32,
}

#[derive(Debug, Clone, Default)]
pub struct NetworkState {
    pub valid: bool,
    pub connected: bool,
    pub wireless: bool,
    pub signal_percent: i32,
    pub ifname: String,
    pub ssid: String,
}

#[derive(Debug, Clone, Default)]
pub struct BatteryState {
    pub valid: bool,
    pub charging: bool,
    pub full: bool,
    pub percent: i32,
}

#[derive(Debug, Clone, Default)]
pub struct BrightnessState {
    pub valid: bool,
    pub percent: i32,
}

#[derive(Debug, Clone, Default)]
pub struct MediaState {
    pub valid: bool,
    pub playing: bool,
    pub paused: bool,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryState {
    pub valid: bool,
    pub used_percent: i32,
    pub used_mb: i64,
    pub total_mb: i64,
}

#[derive(Debug, Clone, Default)]
pub struct CpuState {
    pub valid: bool,
    pub usage_percent: i32,
}

// ── Status cache with throttled refresh ─────────────────────────────

pub struct StatusCache {
    pub volume: VolumeState,
    pub network: NetworkState,
    pub battery: BatteryState,
    pub brightness: BrightnessState,
    pub media: MediaState,
    pub memory: MemoryState,
    pub cpu: CpuState,

    last_volume: Option<Instant>,
    last_network: Option<Instant>,
    last_battery: Option<Instant>,
    last_brightness: Option<Instant>,
    last_media: Option<Instant>,
    last_memory: Option<Instant>,
    last_cpu: Option<Instant>,
    // Previous CPU jiffies for delta calculation
    prev_cpu_total: u64,
    prev_cpu_idle: u64,
}

impl StatusCache {
    pub fn new() -> Self {
        Self {
            volume: VolumeState::default(),
            network: NetworkState::default(),
            battery: BatteryState::default(),
            brightness: BrightnessState::default(),
            media: MediaState::default(),
            memory: MemoryState::default(),
            cpu: CpuState::default(),
            last_volume: None,
            last_network: None,
            last_battery: None,
            last_brightness: None,
            last_media: None,
            last_memory: None,
            last_cpu: None,
            prev_cpu_total: 0,
            prev_cpu_idle: 0,
        }
    }

    /// Refresh all status modules with throttling.
    pub fn refresh(&mut self, force: bool) {
        let now = Instant::now();

        if force || self.should_update(&self.last_volume, now, 500) {
            self.volume = poll_volume();
            self.last_volume = Some(now);
        }

        if force || self.should_update(&self.last_network, now, 2000) {
            self.network = poll_network();
            self.last_network = Some(now);
        }

        if force || self.should_update(&self.last_battery, now, 5000) {
            self.battery = poll_battery();
            self.last_battery = Some(now);
        }

        if force || self.should_update(&self.last_brightness, now, 1500) {
            self.brightness = poll_brightness();
            self.last_brightness = Some(now);
        }

        if force || self.should_update(&self.last_media, now, 2000) {
            self.media = poll_media();
            self.last_media = Some(now);
        }

        if force || self.should_update(&self.last_memory, now, 1000) {
            self.memory = poll_memory();
            self.last_memory = Some(now);
        }

        if force || self.should_update(&self.last_cpu, now, 2000) {
            self.cpu = poll_cpu(&mut self.prev_cpu_total, &mut self.prev_cpu_idle);
            self.last_cpu = Some(now);
        }
    }

    fn should_update(&self, last: &Option<Instant>, now: Instant, interval_ms: u64) -> bool {
        match last {
            None => true,
            Some(t) => now.duration_since(*t).as_millis() >= interval_ms as u128,
        }
    }
}

// ── Polling functions ───────────────────────────────────────────────

fn poll_volume() -> VolumeState {
    let mut st = VolumeState::default();

    // Try wpctl first
    let output = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output();

    if let Ok(out) = output {
        let line = String::from_utf8_lossy(&out.stdout);
        // Format: "Volume: 0.50" or "Volume: 0.50 [MUTED]"
        if let Some(rest) = line.trim().strip_prefix("Volume:") {
            let rest = rest.trim();
            st.muted = rest.contains("[MUTED]");
            let num_part = rest.split_whitespace().next().unwrap_or("0");
            if let Ok(vol) = num_part.parse::<f64>() {
                st.percent = (vol * 100.0).round() as i32;
                st.valid = true;
            }
        }
    }

    st
}

fn poll_network() -> NetworkState {
    let mut st = NetworkState::default();

    let entries = match fs::read_dir("/sys/class/net") {
        Ok(e) => e,
        Err(_) => return st,
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip loopback and virtual interfaces
        if name == "lo"
            || name.starts_with("docker")
            || name.starts_with("veth")
            || name.starts_with("br-")
            || name.starts_with("tailscale")
            || name.starts_with("tun")
        {
            continue;
        }

        let base = format!("/sys/class/net/{name}");
        let operstate = read_sysfs(&format!("{base}/operstate"));
        let carrier = read_sysfs(&format!("{base}/carrier"));

        let connected = carrier == "1"
            || matches!(operstate.as_str(), "up" | "unknown" | "dormant");

        if !connected {
            continue;
        }

        st.valid = true;
        st.connected = true;
        st.ifname = name.clone();
        st.wireless = std::path::Path::new(&format!("{base}/wireless")).exists();

        if st.wireless {
            st.signal_percent = read_wireless_signal(&name);
            st.ssid = read_wireless_ssid(&name);
        }

        break;
    }

    if !st.valid {
        st.valid = true; // Valid but disconnected
    }

    st
}

fn poll_battery() -> BatteryState {
    let mut st = BatteryState::default();

    let entries = match fs::read_dir("/sys/class/power_supply") {
        Ok(e) => e,
        Err(_) => return st,
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let base = format!("/sys/class/power_supply/{name}");

        let ptype = read_sysfs(&format!("{base}/type"));
        if ptype != "Battery" {
            continue;
        }

        let capacity = read_sysfs(&format!("{base}/capacity"));
        let status = read_sysfs(&format!("{base}/status"));

        st.percent = capacity.parse().unwrap_or(0);
        st.charging = status == "Charging";
        st.full = status == "Full";
        st.valid = true;
        break;
    }

    st
}

fn poll_brightness() -> BrightnessState {
    let mut st = BrightnessState::default();

    let entries = match fs::read_dir("/sys/class/backlight") {
        Ok(e) => e,
        Err(_) => return st,
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let base = format!("/sys/class/backlight/{name}");

        let cur: i64 = read_sysfs(&format!("{base}/brightness")).parse().unwrap_or(0);
        let max: i64 = read_sysfs(&format!("{base}/max_brightness")).parse().unwrap_or(0);

        if max > 0 {
            st.percent = ((cur * 100) / max) as i32;
            st.valid = true;
            break;
        }
    }

    st
}

fn poll_media() -> MediaState {
    let mut st = MediaState::default();

    let status_out = Command::new("playerctl").arg("status").output();
    if let Ok(out) = status_out {
        if out.status.success() {
            let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
            st.playing = status == "Playing";
            st.paused = status == "Paused";

            let meta_out = Command::new("playerctl")
                .args(["metadata", "--format", "{{artist}} - {{title}}"])
                .output();
            if let Ok(meta) = meta_out {
                if meta.status.success() {
                    st.text = String::from_utf8_lossy(&meta.stdout).trim().to_string();
                }
            }

            st.valid = true;
        }
    }

    st
}

fn poll_memory() -> MemoryState {
    let mut st = MemoryState::default();

    let file = match fs::File::open("/proc/meminfo") {
        Ok(f) => f,
        Err(_) => return st,
    };

    let reader = BufReader::new(file);
    let mut total_kb: i64 = 0;
    let mut avail_kb: i64 = 0;

    for line in reader.lines().map_while(Result::ok) {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = parse_meminfo_value(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = parse_meminfo_value(rest);
        }

        if total_kb > 0 && avail_kb > 0 {
            break;
        }
    }

    if total_kb > 0 {
        let used_kb = total_kb - avail_kb;
        st.used_percent = ((used_kb * 100) / total_kb) as i32;
        st.total_mb = total_kb / 1024;
        st.used_mb = used_kb / 1024;
        st.valid = true;
    }

    st
}

// ── Helpers ─────────────────────────────────────────────────────────

fn read_sysfs(path: &str) -> String {
    fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn parse_meminfo_value(s: &str) -> i64 {
    s.trim()
        .split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn read_wireless_signal(ifname: &str) -> i32 {
    // Parse /proc/net/wireless for link quality
    let content = match fs::read_to_string("/proc/net/wireless") {
        Ok(c) => c,
        Err(_) => return -1,
    };

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(ifname) {
            // Format: "wlan0: 0000   50.  -60.  -256  ..."
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 3 {
                let quality: f64 = parts[2].trim_end_matches('.').parse().unwrap_or(0.0);
                // Normalize: 70 is max quality for most drivers
                return ((quality / 70.0) * 100.0).min(100.0) as i32;
            }
        }
    }

    -1
}

fn read_wireless_ssid(ifname: &str) -> String {
    let output = Command::new("iwgetid")
        .args(["-r", ifname])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        _ => String::new(),
    }
}

/// Read CPU usage from /proc/stat as delta between samples.
fn poll_cpu(prev_total: &mut u64, prev_idle: &mut u64) -> CpuState {
    let content = match fs::read_to_string("/proc/stat") {
        Ok(c) => c,
        Err(_) => return CpuState::default(),
    };

    let first_line = match content.lines().next() {
        Some(l) if l.starts_with("cpu ") => l,
        _ => return CpuState::default(),
    };

    // cpu  user nice system idle iowait irq softirq steal
    let fields: Vec<u64> = first_line
        .split_whitespace()
        .skip(1)
        .filter_map(|s| s.parse().ok())
        .collect();

    if fields.len() < 4 {
        return CpuState::default();
    }

    let total: u64 = fields.iter().sum();
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0); // idle + iowait

    let usage = if *prev_total > 0 {
        let dtotal = total.saturating_sub(*prev_total);
        let didle = idle.saturating_sub(*prev_idle);
        if dtotal > 0 {
            (((dtotal - didle) as f64 / dtotal as f64) * 100.0) as i32
        } else {
            0
        }
    } else {
        0 // first sample, no delta yet
    };

    *prev_total = total;
    *prev_idle = idle;

    CpuState {
        valid: *prev_total > 0,
        usage_percent: usage.clamp(0, 100),
    }
}
