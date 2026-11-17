// SPDX-License-Identifier: Apache-2.0
//! SigmaOS-omarchy-system-monitor — htop/btop-style system stats.
//!
//! Collects CPU/memory/swap/load/uptime/disk/network/temperature/battery
//! from /proc + /sys on Linux (best-effort, injectable for tests) and
//! keeps a rolling history for sparklines/graphs.

use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Copy, Default)]
pub struct CpuTimes { pub user: u64, pub nice: u64, pub system: u64, pub idle: u64, pub iowait: u64, pub irq: u64, pub softirq: u64, pub steal: u64 }
impl CpuTimes {
    pub fn total(&self) -> u64 { self.user + self.nice + self.system + self.idle + self.iowait + self.irq + self.softirq + self.steal }
    pub fn idle_total(&self) -> u64 { self.idle + self.iowait }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MemInfo { pub total_kb: u64, pub free_kb: u64, pub available_kb: u64, pub buffers_kb: u64, pub cached_kb: u64, pub swap_total_kb: u64, pub swap_free_kb: u64 }
impl MemInfo {
    pub fn used_kb(&self) -> u64 { self.total_kb.saturating_sub(self.available_kb) }
    pub fn used_pct(&self) -> f64 { if self.total_kb == 0 { 0.0 } else { self.used_kb() as f64 / self.total_kb as f64 * 100.0 } }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NetRate { pub rx_bytes: u64, pub tx_bytes: u64 }

#[derive(Debug, Clone, Default)]
pub struct DiskUsage { pub mount: String, pub total_gb: f64, pub used_gb: f64, pub avail_gb: f64 }
impl DiskUsage { pub fn used_pct(&self) -> f64 { if self.total_gb == 0.0 { 0.0 } else { self.used_gb / self.total_gb * 100.0 } } }

#[derive(Debug, Clone)]
pub struct BatteryInfo { pub present: bool, pub percent: u8, pub status: String }

#[derive(Debug, Clone, Default)]
pub struct ProcSnapshot {
    pub cpu: CpuTimes,               // aggregate
    pub per_cpu: Vec<CpuTimes>,
    pub mem: MemInfo,
    pub load: [f64; 3],
    pub uptime_secs: u64,
    pub procs_running: u64,
    pub procs_total: u64,
    pub net: BTreeMap<String, NetRate>,
    pub disks: Vec<DiskUsage>,
    pub temps_c: Vec<(String, f64)>,  // (sensor name, celsius)
    pub battery: Option<BatteryInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct StatsDelta {
    pub cpu_pct: f64,
    pub per_cpu_pct: Vec<f64>,
    pub net_rx_bps: BTreeMap<String, f64>,
    pub net_tx_bps: BTreeMap<String, f64>,
}

pub struct OmarchySystemMonitor {
    pub history: VecDeque<StatsDelta>,
    pub max_history: usize,
    prev: Option<ProcSnapshot>,
    prev_at: Option<std::time::Instant>,
}

impl OmarchySystemMonitor {
    pub fn new() -> Self {
        Self { history: VecDeque::new(), max_history: 120, prev: None, prev_at: None }
    }

    /// Ingest a snapshot (from /proc or a test), compute delta vs previous.
    pub fn ingest(&mut self, snap: ProcSnapshot) -> StatsDelta {
        let delta = match (&self.prev, self.prev_at) {
            (Some(p), Some(t)) => self.compute_delta(p, &snap, t.elapsed().as_secs_f64().max(0.001)),
            _ => StatsDelta::default(),
        };
        self.prev = Some(snap);
        self.prev_at = Some(std::time::Instant::now());
        self.history.push_back(delta.clone());
        if self.history.len() > self.max_history { self.history.pop_front(); }
        delta
    }

    fn compute_delta(&self, prev: &ProcSnapshot, cur: &ProcSnapshot, dt: f64) -> StatsDelta {
        let busy_pct = |a: CpuTimes, b: CpuTimes| -> f64 {
            let dt_total = (b.total() as i128 - a.total() as i128).max(0) as f64;
            let dt_idle = (b.idle_total() as i128 - a.idle_total() as i128).max(0) as f64;
            if dt_total <= 0.0 { 0.0 } else { ((dt_total - dt_idle) / dt_total * 100.0).clamp(0.0, 100.0) }
        };
        let mut d = StatsDelta { cpu_pct: busy_pct(prev.cpu, cur.cpu), per_cpu_pct: Vec::new(), net_rx_bps: BTreeMap::new(), net_tx_bps: BTreeMap::new() };
        for (i, c) in cur.per_cpu.iter().enumerate() {
            let p = prev.per_cpu.get(i).copied().unwrap_or_default();
            d.per_cpu_pct.push(busy_pct(p, *c));
        }
        for (iface, cur_n) in &cur.net {
            if let Some(prev_n) = prev.net.get(iface) {
                d.net_rx_bps.insert(iface.clone(), (cur_n.rx_bytes as i128 - prev_n.rx_bytes as i128).max(0) as f64 / dt);
                d.net_tx_bps.insert(iface.clone(), (cur_n.tx_bytes as i128 - prev_n.tx_bytes as i128).max(0) as f64 / dt);
            }
        }
        d
    }

    /// Collect a real snapshot from /proc + /sys (Linux). Best-effort.
    pub fn collect(&self) -> ProcSnapshot {
        let mut s = ProcSnapshot::default();
        // CPU from /proc/stat
        if let Ok(stat) = std::fs::read_to_string("/proc/stat") {
            let mut per = Vec::new();
            for line in stat.lines() {
                if line.starts_with("cpu") {
                    let parts: Vec<u64> = line.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
                    if parts.len() >= 8 {
                        let t = CpuTimes { user: parts[0], nice: parts[1], system: parts[2], idle: parts[3], iowait: parts[4], irq: parts[5], softirq: parts[6], steal: parts[7] };
                        if line.starts_with("cpu ") || line == "cpu" { s.cpu = t; }
                        else { per.push(t); }
                    }
                }
                if line.starts_with("procs_running") { s.procs_running = line.split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0); }
            }
            s.per_cpu = per;
        }
        // meminfo
        if let Ok(mi) = std::fs::read_to_string("/proc/meminfo") {
            let get = |k: &str| -> u64 { mi.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0) };
            s.mem = MemInfo { total_kb: get("MemTotal"), free_kb: get("MemFree"), available_kb: get("MemAvailable"),
                buffers_kb: get("Buffers"), cached_kb: get("Cached"), swap_total_kb: get("SwapTotal"), swap_free_kb: get("SwapFree") };
        }
        // loadavg
        if let Ok(la) = std::fs::read_to_string("/proc/loadavg") {
            let p: Vec<f64> = la.split_whitespace().take(3).filter_map(|x| x.parse().ok()).collect();
            for i in 0..p.len().min(3) { s.load[i] = p[i]; }
        }
        // uptime
        if let Ok(u) = std::fs::read_to_string("/proc/uptime") {
            s.uptime_secs = u.split_whitespace().next().and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0) as u64;
        }
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            s.procs_total = status.lines().find(|l| l.starts_with("Threads"))
                .and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0);
        }
        // net dev rx/tx from /proc/net/dev
        if let Ok(nd) = std::fs::read_to_string("/proc/net/dev") {
            for line in nd.lines().skip(2) {
                let Some((iface, rest)) = line.split_once(':') else { continue };
                let nums: Vec<u64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                if nums.len() >= 9 {
                    let iface = iface.trim().to_string();
                    if iface != "lo" {
                        s.net.insert(iface, NetRate { rx_bytes: nums[0], tx_bytes: nums[8] });
                    }
                }
            }
        }
        // temps from /sys/class/thermal/thermal_zone*/temp
        if let Ok(rd) = std::fs::read_dir("/sys/class/thermal") {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(z) = name.strip_prefix("thermal_zone") {
                    if let Ok(t) = std::fs::read_to_string(e.path().join("temp")) {
                        if let Ok(mc) = t.trim().parse::<f64>() { s.temps_c.push((format!("zone{z}"), mc / 1000.0)); }
                    }
                }
            }
        }
        // battery from /sys/class/power_supply
        if let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.contains("BAT") || n.contains("battery") {
                    let cap = std::fs::read_to_string(e.path().join("capacity")).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
                    let st = std::fs::read_to_string(e.path().join("status")).unwrap_or_default();
                    s.battery = Some(BatteryInfo { present: true, percent: cap, status: st.trim().to_string() });
                }
            }
        }
        s
    }

    /// Pretty status lines.
    pub fn cpu_line(&self, d: &StatsDelta) -> String { format!("cpu: {:.1}%", d.cpu_pct) }
    pub fn mem_line(&self, snap: &ProcSnapshot) -> String {
        format!("mem: {:.1}% ({:.1}/{:.1} GiB) swap {:.1}%", snap.mem.used_pct(),
            snap.mem.used_kb() as f64 / 1048576.0, snap.mem.total_kb as f64 / 1048576.0,
            if snap.mem.swap_total_kb > 0 { (snap.mem.swap_total_kb - snap.mem.swap_free_kb) as f64 / snap.mem.swap_total_kb as f64 * 100.0 } else { 0.0 })
    }
    pub fn load_line(&self, snap: &ProcSnapshot) -> String {
        format!("load: {:.2} {:.2} {:.2}  up {}d{}h{}m", snap.load[0], snap.load[1], snap.load[2],
            snap.uptime_secs / 86400, (snap.uptime_secs % 86400) / 3600, (snap.uptime_secs % 3600) / 60)
    }
    pub fn disk_lines(&self, snap: &ProcSnapshot) -> Vec<String> {
        snap.disks.iter().map(|d| format!("{}: {:.1}% ({:.1}/{:.1}G)", d.mount, d.used_pct(), d.used_gb, d.total_gb)).collect()
    }
    pub fn temp_line(&self, snap: &ProcSnapshot) -> String {
        let max = snap.temps_c.iter().map(|(_, t)| *t).fold(0.0_f64, f64::max);
        format!("temp: {:.0}C max", max)
    }
    pub fn battery_line(&self, snap: &ProcSnapshot) -> String {
        snap.battery.as_ref().map(|b| format!("battery: {}% {}", b.percent, b.status)).unwrap_or_else(|| "battery: none".into())
    }
    pub fn net_lines(&self, d: &StatsDelta) -> Vec<String> {
        let mut v: Vec<_> = d.net_rx_bps.iter().map(|(i, r)| (i.clone(), *r, *d.net_tx_bps.get(i).unwrap_or(&0.0))).collect();
        v.sort_by(|a, b| (b.1 + b.2).partial_cmp(&(a.1 + a.2)).unwrap_or(std::cmp::Ordering::Equal));
        v.iter().map(|(i, r, t)| format!("{}: ↓{:.0} B/s ↑{:.0} B/s", i, r, t)).collect()
    }
    pub fn status_block(&self, snap: &ProcSnapshot, d: &StatsDelta) -> String {
        let mut s = String::new();
        s.push_str(&self.cpu_line(d)); s.push('\n');
        s.push_str(&self.mem_line(snap)); s.push('\n');
        s.push_str(&self.load_line(snap)); s.push('\n');
        s.push_str(&self.temp_line(snap)); s.push('\n');
        s.push_str(&self.battery_line(snap)); s.push('\n');
        for l in self.disk_lines(snap) { s.push_str(&l); s.push('\n'); }
        for l in self.net_lines(d) { s.push_str(&l); s.push('\n'); }
        s
    }
}

impl Default for OmarchySystemMonitor {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(cpu_total: u64, cpu_idle: u64, rx: u64, mem_used_pct_kb: u64) -> ProcSnapshot {
        ProcSnapshot {
            cpu: CpuTimes { user: cpu_total - cpu_idle, system: 0, idle: cpu_idle, ..Default::default() },
            mem: MemInfo { total_kb: 16_000_000, free_kb: 16_000_000 - mem_used_pct_kb, available_kb: 16_000_000 - mem_used_pct_kb, swap_total_kb: 4_000_000, swap_free_kb: 4_000_000, ..Default::default() },
            load: [0.5, 0.4, 0.3], uptime_secs: 90061,
            net: { let mut m = BTreeMap::new(); m.insert("eth0".to_string(), NetRate { rx_bytes: rx, tx_bytes: rx / 2 }); m },
            disks: vec![DiskUsage { mount: "/".into(), total_gb: 500.0, used_gb: 250.0, avail_gb: 250.0 }],
            temps_c: vec![("zone0".into(), 55.0)],
            battery: Some(BatteryInfo { present: true, percent: 87, status: "Discharging".into() }),
            ..Default::default()
        }
    }

    #[test]
    fn cpu_delta_percent() {
        let mut m = OmarchySystemMonitor::new();
        m.ingest(snap(100, 50, 1000, 8_000_000)); // first: no delta
        let d = m.ingest(snap(200, 120, 3000, 8_000_000)); // +100 total, +70 idle -> 30% busy
        assert!((d.cpu_pct - 30.0).abs() < 0.01, "cpu={} expected 30", d.cpu_pct);
    }

    #[test]
    fn net_rate_bps() {
        let mut m = OmarchySystemMonitor::new();
        m.ingest(snap(100, 50, 1000, 8_000_000));
        std::thread::sleep(std::time::Duration::from_millis(50));
        let d = m.ingest(snap(200, 120, 1100, 8_000_000)); // +100 bytes rx
        let rx = d.net_rx_bps.get("eth0").copied().unwrap_or(0.0);
        assert!(rx > 0.0, "rx rate should be positive, got {rx}");
    }

    #[test]
    fn mem_and_load_lines() {
        let m = OmarchySystemMonitor::new();
        let s = snap(100, 50, 1000, 8_000_000);
        let ml = m.mem_line(&s);
        assert!(ml.contains("mem:") && ml.contains("50.0%")); // 8M/16M used
        let ll = m.load_line(&s);
        assert!(ll.contains("0.50") && ll.contains("1d")); // 90061s = 1d1h1m
    }

    #[test]
    fn disk_temp_battery_lines() {
        let m = OmarchySystemMonitor::new();
        let s = snap(100, 50, 0, 0);
        let dl = m.disk_lines(&s);
        assert!(dl[0].contains("/:") && dl[0].contains("50.0%"));
        assert!(m.temp_line(&s).contains("55C"));
        assert!(m.battery_line(&s).contains("87%"));
    }

    #[test]
    fn status_block_and_history() {
        let mut m = OmarchySystemMonitor::new();
        m.ingest(snap(100, 50, 0, 8_000_000));
        let d = m.ingest(snap(200, 120, 0, 8_000_000));
        let s = snap(200, 120, 0, 8_000_000);
        let block = m.status_block(&s, &d);
        assert!(block.contains("cpu:") && block.contains("mem:") && block.contains("load:"));
        assert_eq!(m.history.len(), 2);
    }

    #[test]
    fn collect_real_proc() {
        let m = OmarchySystemMonitor::new();
        let s = m.collect();
        assert!(s.mem.total_kb > 0, "meminfo should be read");
        assert!(s.uptime_secs > 0);
    }
}
