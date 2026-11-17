//! SigmaOS-omarchy-system-monitor — htop/btop-style stats CLI.
//!
//! Usage:
//!   omarchy-system-monitor once          (single snapshot + delta)
//!   omarchy-system-monitor watch [secs]  (refresh loop, default 1s)
//!   omarchy-system-monitor cpu|mem|load|disk|net|temp|battery

use omarchy_system_monitor::OmarchySystemMonitor;
use std::process::exit;
use std::thread::sleep;
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("once");
    let mut m = OmarchySystemMonitor::new();

    match cmd {
        "once" => {
            let snap = m.collect();
            let d = m.ingest(snap.clone());
            print!("{}", m.status_block(&snap, &d));
        }
        "watch" => {
            let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
            loop {
                let snap = m.collect();
                let d = m.ingest(snap.clone());
                print!("\x1b[2J\x1b[H");
                print!("{}", m.status_block(&snap, &d));
                let _ = std::io::Write::flush(&mut std::io::stdout());
                sleep(Duration::from_secs(secs));
            }
        }
        "cpu" | "mem" | "load" | "disk" | "net" | "temp" | "battery" => {
            // two samples for cpu/net deltas
            let s1 = m.collect(); let _ = m.ingest(s1);
            sleep(Duration::from_millis(300));
            let snap = m.collect();
            let d = m.ingest(snap.clone());
            match cmd {
                "cpu" => println!("{}", m.cpu_line(&d)),
                "mem" => println!("{}", m.mem_line(&snap)),
                "load" => println!("{}", m.load_line(&snap)),
                "disk" => for l in m.disk_lines(&snap) { println!("{l}"); },
                "net" => for l in m.net_lines(&d) { println!("{l}"); },
                "temp" => println!("{}", m.temp_line(&snap)),
                _ => println!("{}", m.battery_line(&snap)),
            }
        }
        _ => { eprintln!("usage: omarchy-system-monitor <once|watch|cpu|mem|load|disk|net|temp|battery>"); exit(2); }
    }
}
