//! Linux system metrics compatible with the former SystemInfoHelper component payload.
//! Collection runs via spawn_blocking; it never blocks the async REST/WS worker threads.
use std::{fs, process::Command, sync::{Mutex, OnceLock}, time::{Duration, Instant}};
use serde_json::{json, Value};

const STATIC_REFRESH: Duration = Duration::from_secs(300);
const STORAGE_REFRESH: Duration = Duration::from_secs(10);
static COMPONENTS: OnceLock<Mutex<Components>> = OnceLock::new();

struct Components {
    data: Value,
    last_cpu: Option<(u64, u64)>,
    static_at: Option<Instant>,
    storage_at: Option<Instant>,
}
impl Default for Components {
    fn default() -> Self {
        Self { data: json!({
            "cpu": {"temp": null}, "gpu": {"controllers": []}, "load": {},
            "memory": {"total":0,"used":0,"available":0,"usage_percent":0},
            "storage": {"internal":null,"external":[]},
            "process": {"rss":0,"heap_total":0,"heap_used":0,"external":0,"array_buffers":0,"heap_usage_percent":0}
        }), last_cpu: None, static_at: None, storage_at: None }
    }
}
fn state() -> &'static Mutex<Components> { COMPONENTS.get_or_init(|| Mutex::new(Components::default())) }
fn percent(used: u64, total: u64) -> f64 {
    if total == 0 { 0.0 } else { ((used as f64 * 1000.0 / total as f64).round()) / 10.0 }
}
fn cpu_counters() -> Option<(u64,u64)> {
    let contents = fs::read_to_string("/proc/stat").ok()?;
    let line = contents.lines().find(|l| l.starts_with("cpu "))?;
    let fields: Vec<u64> = line.split_whitespace().skip(1).filter_map(|s| s.parse().ok()).collect();
    if fields.len() < 4 { return None; }
    let total = fields.iter().copied().sum();
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    Some((total, idle))
}
fn cpu_static() -> Value {
    let info = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let model = info.lines().find_map(|line| line.strip_prefix("model name").and_then(|s| s.split_once(':')).map(|(_,v)| v.trim().to_string())).unwrap_or_default();
    let cores = info.lines().filter(|line| line.starts_with("processor")).count();
    json!({"manufacturer":"", "brand": model, "cores": cores, "physicalCores": cores, "speed": 0})
}
fn temperature() -> Value {
    let mut values = Vec::new();
    if let Ok(devices) = fs::read_dir("/sys/class/hwmon") {
        for device in devices.flatten() {
            if let Ok(entries) = fs::read_dir(device.path()) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("temp") && name.ends_with("_input") {
                        if let Ok(v) = fs::read_to_string(entry.path()) {
                            if let Ok(n) = v.trim().parse::<f64>() { if (-20_000.0..180_000.0).contains(&n) { values.push(n / 1000.0); } }
                        }
                    }
                }
            }
        }
    }
    let main = values.first().copied();
    let max = values.iter().copied().reduce(f64::max);
    json!({"main": main, "max": max, "cores": values})
}
fn meminfo() -> Value {
    let contents = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let read = |key: &str| -> u64 {
        contents.lines().find(|s| s.starts_with(key)).and_then(|s| s.split_whitespace().nth(1)).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024
    };
    let total = read("MemTotal:"); let available = read("MemAvailable:");
    let used = total.saturating_sub(available);
    json!({"total":total,"used":used,"available":available,"usage_percent":percent(used,total)})
}
fn process_memory() -> Value {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let rss = status.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0)*1024;
    // Rust allocator does not expose Node's heapTotal/heapUsed/external/arrayBuffers counters.
    json!({"rss":rss,"heap_total":0,"heap_used":0,"external":0,"array_buffers":0,"heap_usage_percent":0})
}
fn storage() -> Value {
    let mut internal = Value::Null;
    let mut external = Vec::new();
    // df -P returns one row per mounted filesystem; -B1 reports byte counts.
    let output = match Command::new("df").args(["-B1","-P"]).output() { Ok(v) if v.status.success() => v, _ => return json!({"internal":null,"external":[]}) };
    let output = String::from_utf8_lossy(&output.stdout);
    for line in output.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 { continue; }
        let mount = fields[5];
        let total: u64 = fields[1].parse().unwrap_or(0);
        let used: u64 = fields[2].parse().unwrap_or(0);
        let available: u64 = fields[3].parse().unwrap_or(0);
        let item = json!({"fs":fields[0],"type":"","mount":mount,"total":total,"used":used,"available":available,"usage_percent":percent(used,total)});
        if mount == "/" { internal = item; }
        else if total > 0 && ["/mnt", "/media", "/run/media"].iter().any(|prefix| mount == *prefix || mount.starts_with(&format!("{prefix}/"))) { external.push(item); }
    }
    json!({"internal":internal,"external":external})
}
fn collect() -> Value {
    let mut locked = state().lock().unwrap_or_else(|err|err.into_inner());
    let now = Instant::now();
    if locked.static_at.is_none_or(|at| now.duration_since(at)>=STATIC_REFRESH) {
        locked.data["cpu"] = cpu_static();
        locked.static_at = Some(now);
    }
    locked.data["cpu"]["temp"] = temperature();
    if let Some((total,idle)) = cpu_counters() {
        let load = locked.last_cpu.and_then(|(old_total,old_idle)| {
            let elapsed = total.saturating_sub(old_total);
            if elapsed == 0 { None } else { Some(percent(elapsed.saturating_sub(idle.saturating_sub(old_idle)),elapsed)) }
        }).unwrap_or(0.0);
        locked.last_cpu = Some((total,idle));
        locked.data["load"] = json!({"currentLoad":load});
    }
    locked.data["memory"] = meminfo();
    locked.data["process"] = process_memory();
    if locked.storage_at.is_none_or(|at| now.duration_since(at)>=STORAGE_REFRESH) {
        locked.data["storage"] = storage();
        locked.storage_at = Some(now);
    }
    locked.data.clone()
}
pub fn get_system_components() -> Value { state().lock().unwrap_or_else(|e|e.into_inner()).data.clone() }
pub fn get_system_info() -> Value {
    let components = get_system_components();
    let cpu = components["load"]["currentLoad"].as_f64().unwrap_or(0.0).round();
    let temp = components["cpu"]["temp"]["main"].as_f64();
    json!({"config":[
        {"label":"CPU Usage", "short":"CPU", "icon":"cpu-64-bit", "data":format!("{} %",cpu)},
        {"label":"CPU Temp", "short":"CPU", "icon":"thermometer", "data":temp.map(|v|format!("{} °C",v.round())).unwrap_or_else(||"N/A".into())}
    ],"components":components})
}
pub async fn update_system_components() -> std::io::Result<Value> {
    let components = tokio::task::spawn_blocking(collect).await.map_err(std::io::Error::other)?;
    crate::server::registry::notify("notify_system_info", get_system_info());
    Ok(components)
}
/// Initial collection, followed by background refresh every 5 seconds.
pub async fn init() -> std::io::Result<tokio::task::JoinHandle<()>> {
    update_system_components().await?;
    Ok(tokio::spawn(async {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(error) = update_system_components().await {
                crate::internal::log::warn(&format!("System components update failed: {error}"));
            }
        }
    }))
}
#[cfg(test)] mod tests { use super::*; #[test] fn percent_rounding(){ assert_eq!(percent(1,8),12.5); assert_eq!(percent(1,0),0.0); } }
