use nvml_wrapper::{
    Nvml,
    enum_wrappers::device::{PcieUtilCounter, TemperatureSensor},
    enums::device::UsedGpuMemory,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

pub struct GpuInfo {
    pub index: u32,
    pub uuid: String,
    pub name: String,
    pub total_mib: u64,
    pub free_mib: u64,
    pub used_mib: u64,
    pub reserved_mib: u64,
}

pub fn gpus() -> anyhow::Result<Vec<GpuInfo>> {
    let nvml = Nvml::init()?;
    (0..nvml.device_count()?)
        .map(|index| {
            let d = nvml.device_by_index(index)?;
            let memory = d.memory_info()?;
            Ok(GpuInfo {
                index,
                uuid: d.uuid()?,
                name: d.name()?,
                total_mib: memory.total >> 20,
                free_mib: memory.free >> 20,
                used_mib: memory.total.saturating_sub(memory.free) >> 20,
                reserved_mib: memory.reserved >> 20,
            })
        })
        .collect()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GpuProcess {
    pub pid: u32,
    pub name: String,
    pub kind: String,
    pub vram: Option<u64>,
    pub rss: u64,
    pub owned: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Hardware {
    pub gpu_name: String,
    pub gpu_total: u64,
    pub gpu_used: u64,
    pub gpu_free: u64,
    #[serde(default)]
    pub gpu_reserved: u64,
    pub gpu_util: Option<u32>,
    pub gpu_temp: Option<u32>,
    pub gpu_power: Option<f64>,
    pub gpu_power_limit: Option<f64>,
    pub pcie_rx: Option<f64>,
    pub pcie_gen: Option<u32>,
    pub pcie_width: Option<u32>,
    pub processes: Vec<GpuProcess>,
    pub process_error: Option<String>,
    pub gpu_error: Option<String>,
    pub ram_total: u64,
    pub ram_used: u64,
    pub ram_available: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub cpu: f64,
    #[serde(default)]
    pub cpu_temp: Option<u32>,
    #[serde(default)]
    pub ram_temp: Option<u32>,
    pub threads: usize,
    pub cpu_name: String,
    pub model_pss: Option<u64>,
    pub model_anon: Option<u64>,
    pub model_file: Option<u64>,
    pub model_shared: Option<u64>,
    pub model_locked: Option<u64>,
    pub model_cgroup: Option<u64>,
    pub disk_read: f64,
    pub disk_write: f64,
}

pub struct Collector {
    nvml: Option<Nvml>,
    gpu: u32,
    pub external: bool,
    cpu_prev: (u64, u64),
    disk_prev: (u64, u64),
    last: Instant,
    memory_at: Option<Instant>,
    model_memory: [Option<u64>; 5],
    temp_paths: (Option<PathBuf>, Vec<PathBuf>),
    temp_at: Option<Instant>,
}

impl Collector {
    pub fn new(gpu: u32) -> Self {
        Self {
            nvml: Nvml::init().ok(),
            gpu,
            external: false,
            cpu_prev: cpu_times(),
            disk_prev: disk_bytes(),
            last: Instant::now(),
            memory_at: None,
            model_memory: [None; 5],
            temp_paths: (None, Vec::new()),
            temp_at: None,
        }
    }
    pub fn sample(&mut self) -> Hardware {
        let elapsed = self.last.elapsed().as_secs_f64().max(0.001);
        self.last = Instant::now();
        let mem = kb_values("/proc/meminfo");
        let mut hw = Hardware {
            ram_total: field(&mem, "MemTotal"),
            ram_available: field(&mem, "MemAvailable"),
            swap_total: field(&mem, "SwapTotal"),
            threads: std::thread::available_parallelism().map_or(0, usize::from),
            cpu_name: fs::read_to_string("/proc/cpuinfo")
                .unwrap_or_default()
                .lines()
                .find_map(|l| l.strip_prefix("model name\t: ").map(str::to_owned))
                .unwrap_or_default(),
            ..Hardware::default()
        };
        hw.ram_used = hw.ram_total.saturating_sub(hw.ram_available);
        hw.swap_used = hw.swap_total.saturating_sub(field(&mem, "SwapFree"));
        let (total, idle) = cpu_times();
        let diff = total.saturating_sub(self.cpu_prev.0);
        hw.cpu = if diff > 0 {
            100.0 * (diff.saturating_sub(idle.saturating_sub(self.cpu_prev.1))) as f64 / diff as f64
        } else {
            0.0
        };
        self.cpu_prev = (total, idle);
        if self.temp_at.is_none_or(|t| t.elapsed().as_secs() >= 30) {
            self.temp_paths = temp_paths();
            self.temp_at = Some(Instant::now());
        }
        hw.cpu_temp = self.temp_paths.0.as_deref().and_then(read_temp);
        hw.ram_temp = self.temp_paths.1.iter().filter_map(|p| read_temp(p)).max();
        let disk = disk_bytes();
        hw.disk_read = disk.0.saturating_sub(self.disk_prev.0) as f64 / elapsed / 1048576.0;
        hw.disk_write = disk.1.saturating_sub(self.disk_prev.1) as f64 / elapsed / 1048576.0;
        self.disk_prev = disk;

        let cgroup = if self.external { None } else { cgroup_dir() };
        let pids = cgroup
            .as_ref()
            .and_then(|p| fs::read_to_string(p.join("cgroup.procs")).ok())
            .unwrap_or_default()
            .lines()
            .filter_map(|s| s.parse().ok())
            .collect::<BTreeSet<u32>>();
        hw.model_cgroup = cgroup
            .and_then(|p| fs::read_to_string(p.join("memory.current")).ok())
            .and_then(|s| s.trim().parse().ok());
        if self.memory_at.is_none_or(|t| t.elapsed().as_secs() >= 5) {
            let mut values = [0u64; 5];
            let mut found = false;
            for pid in &pids {
                let values_kb = kb_values(&format!("/proc/{pid}/smaps_rollup"));
                if values_kb.contains_key("Pss") {
                    found = true;
                }
                for (i, name) in ["Pss", "Pss_Anon", "Pss_File", "Locked", "Pss_Shmem"]
                    .iter()
                    .enumerate()
                {
                    values[i] += field(&values_kb, name);
                }
            }
            self.model_memory = if found {
                [
                    Some(values[0]),
                    Some(values[1]),
                    Some(values[2]),
                    Some(values[3]),
                    Some(values[4]),
                ]
            } else {
                [None; 5]
            };
            self.memory_at = Some(Instant::now());
        }
        [
            hw.model_pss,
            hw.model_anon,
            hw.model_file,
            hw.model_locked,
            hw.model_shared,
        ] = self.model_memory;

        if self.nvml.is_none() {
            self.nvml = Nvml::init().ok();
        }
        let Some(nvml) = &self.nvml else {
            hw.gpu_error = Some("NVIDIA management library unavailable".into());
            return hw;
        };
        let device = match nvml.device_by_index(self.gpu) {
            Ok(dev) => dev,
            Err(error) => {
                hw.gpu_error = Some(error.to_string());
                return hw;
            }
        };
        hw.gpu_name = device.name().unwrap_or_default();
        match device.memory_info() {
            Ok(mem) => {
                hw.set_gpu_memory(mem.total, mem.free, mem.reserved);
            }
            Err(error) => {
                hw.gpu_error = Some(error.to_string());
            }
        }
        hw.gpu_util = device.utilization_rates().ok().map(|u| u.gpu);
        hw.gpu_temp = device.temperature(TemperatureSensor::Gpu).ok();
        hw.gpu_power = device.power_usage().ok().map(|n| n as f64 / 1000.0);
        hw.gpu_power_limit = device
            .power_management_limit()
            .ok()
            .map(|n| n as f64 / 1000.0);
        hw.pcie_rx = device
            .pcie_throughput(PcieUtilCounter::Receive)
            .ok()
            .map(|n| n as f64 / 1024.0);
        hw.pcie_gen = device.current_pcie_link_gen().ok();
        hw.pcie_width = device.current_pcie_link_width().ok();
        let compute = device
            .running_compute_processes()
            .or_else(|_| device.running_compute_processes_v2());
        let graphics = device
            .running_graphics_processes()
            .or_else(|_| device.running_graphics_processes_v2());
        if compute.is_err() && graphics.is_err() {
            hw.process_error = Some("GPU process memory unavailable".into());
        }
        let mut rows = BTreeMap::<u32, GpuProcess>::new();
        for (kind, processes) in [("C", compute), ("G", graphics)] {
            for process in processes.unwrap_or_default() {
                let memory = match process.used_gpu_memory {
                    UsedGpuMemory::Used(bytes) => Some(bytes),
                    _ => None,
                };
                let row = rows.entry(process.pid).or_insert_with(|| {
                    let status = kb_values(&format!("/proc/{}/status", process.pid));
                    let name = nvml
                        .sys_process_name(process.pid, 4096)
                        .unwrap_or_else(|_| {
                            fs::read_to_string(format!("/proc/{}/comm", process.pid))
                                .unwrap_or_else(|_| "exited".into())
                        });
                    let basename = std::path::Path::new(name.trim())
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(name.trim());
                    GpuProcess {
                        pid: process.pid,
                        name: crate::model::sanitize(basename),
                        kind: String::new(),
                        vram: None,
                        rss: field(&status, "VmRSS"),
                        owned: pids.contains(&process.pid),
                    }
                });
                row.kind.push_str(kind);
                row.vram = match (row.vram, memory) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
            }
        }
        hw.processes = rows.into_values().collect();
        hw.processes
            .sort_by(|a, b| b.vram.cmp(&a.vram).then(a.pid.cmp(&b.pid)));
        hw
    }
}

fn kb_values(path: &str) -> BTreeMap<String, u64> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (key, rest) = line.split_once(':')?;
            let value = rest.split_whitespace().next()?.parse::<u64>().ok()?;
            Some((key.to_owned(), value * 1024))
        })
        .collect()
}
fn field(map: &BTreeMap<String, u64>, key: &str) -> u64 {
    *map.get(key).unwrap_or(&0)
}
fn cpu_times() -> (u64, u64) {
    let content = fs::read_to_string("/proc/stat").unwrap_or_default();
    let values: Vec<u64> = content
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse().ok())
        .collect();
    (
        values.iter().take(8).sum(),
        values.get(3).unwrap_or(&0) + values.get(4).unwrap_or(&0),
    )
}
// hwmon names that expose package/CPU temperature: AMD, Intel, and generic
// SoC drivers, plus the older AMD driver.
const CPU_SENSORS: [&str; 6] = [
    "k10temp",
    "coretemp",
    "zenpower",
    "k8temp",
    "cpu_thermal",
    "soc_thermal",
];
// Thermal zone types used when no hwmon driver matched: Intel's legacy
// package driver and the names ARM boards give their CPU zones.
const CPU_ZONES: [&str; 4] = ["x86_pkg_temp", "cpu_thermal", "cpu-thermal", "soc_thermal"];
fn temp_paths() -> (Option<PathBuf>, Vec<PathBuf>) {
    let (cpu, ram) = temp_paths_in(Path::new("/sys/class/hwmon"));
    let cpu = cpu.or_else(|| thermal_cpu_path_in(Path::new("/sys/class/thermal")));
    (cpu, ram)
}
fn temp_paths_in(dir: &Path) -> (Option<PathBuf>, Vec<PathBuf>) {
    let mut cpu = None;
    let mut ram = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return (cpu, ram);
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let name = fs::read_to_string(dir.join("name"))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        if cpu.is_none() && CPU_SENSORS.contains(&name.as_str()) {
            cpu = cpu_sensor_input(&dir);
        } else if name == "spd5118" {
            // DDR5 SPD hub temperature sensor, one hwmon device per DIMM.
            ram.push(dir.join("temp1_input"));
        }
    }
    (cpu, ram)
}
// Drivers label the package reading: k10temp's Tctl, coretemp's package,
// soc_thermal's "physical". Fall back to the primary sensor where unlabeled.
fn cpu_sensor_input(dir: &Path) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let Some(stem) = path
            .file_name()
            .and_then(|f| f.to_str())
            .and_then(|f| f.strip_prefix("temp"))
            .and_then(|f| f.strip_suffix("_label"))
        else {
            continue;
        };
        let label = fs::read_to_string(&path)
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        let package = ["Tctl", "Tdie", "physical"].contains(&label.as_str())
            || label.contains("Package")
            || label.contains("CPU");
        if package {
            let input = dir.join(format!("temp{stem}_input"));
            if input.exists() {
                return Some(input);
            }
        }
    }
    let input = dir.join("temp1_input");
    input.exists().then_some(input)
}
fn thermal_cpu_path_in(dir: &Path) -> Option<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|name| name.starts_with("thermal_zone"))
        {
            continue;
        }
        let kind = fs::read_to_string(dir.join("type"))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        if CPU_ZONES.contains(&kind.as_str()) {
            let input = dir.join("temp");
            if input.exists() {
                return Some(input);
            }
        }
    }
    None
}
fn read_temp(path: &Path) -> Option<u32> {
    fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()
        .map(|milli| (milli as f64 / 1000.).round().max(0.) as u32)
}
fn disk_bytes() -> (u64, u64) {
    let mut result = (0, 0);
    for line in fs::read_to_string("/proc/diskstats")
        .unwrap_or_default()
        .lines()
    {
        let parts: Vec<_> = line.split_whitespace().collect();
        // Whole physical block devices only, excluding partition, dm-crypt and
        // zram counters so the same I/O is not counted several times.
        if parts.len() < 14
            || !PathBuf::from(format!("/sys/block/{}", parts[2])).exists()
            || !(parts[2].starts_with("nvme")
                || parts[2].starts_with("sd")
                || parts[2].starts_with("vd"))
        {
            continue;
        }
        result.0 += parts[5].parse::<u64>().unwrap_or(0) * 512;
        result.1 += parts[9].parse::<u64>().unwrap_or(0) * 512;
    }
    result
}
pub fn cgroup_dir() -> Option<PathBuf> {
    let content = fs::read_to_string("/proc/self/cgroup").ok()?;
    let relative = content.lines().find_map(|l| l.strip_prefix("0::"))?;
    Some(PathBuf::from("/sys/fs/cgroup").join(relative.trim_start_matches('/')))
}

impl Hardware {
    fn set_gpu_memory(&mut self, total: u64, free: u64, reserved: u64) {
        self.gpu_total = total;
        self.gpu_free = free;
        // NVML v2 separates driver-reserved bytes from allocations. Use the
        // non-free total for both the bar and the policy's usage threshold.
        self.gpu_used = total.saturating_sub(free);
        self.gpu_reserved = reserved;
    }

    pub fn gpu_allocated(&self) -> u64 {
        self.gpu_used.saturating_sub(self.gpu_reserved)
    }

    pub fn demo() -> Self {
        let gb = 1u64 << 30;
        Self {
            gpu_name: "NVIDIA GeForce RTX 5080".into(),
            gpu_total: 16 * gb,
            gpu_used: 14 * gb + gb / 4,
            gpu_free: gb + gb * 3 / 4,
            gpu_reserved: 384 << 20,
            gpu_util: Some(87),
            gpu_temp: Some(62),
            gpu_power: Some(241.8),
            gpu_power_limit: Some(400.0),
            pcie_rx: Some(4782.0),
            pcie_gen: Some(5),
            pcie_width: Some(16),
            ram_total: 92 * gb,
            ram_used: 68 * gb,
            ram_available: 24 * gb,
            swap_total: 112 * gb,
            swap_used: 6 * gb,
            cpu: 42.7,
            cpu_temp: Some(64),
            ram_temp: Some(48),
            threads: 16,
            cpu_name: "AMD Ryzen 7 9800X3D".into(),
            model_pss: Some(49 * gb + gb / 2),
            model_anon: Some(46 * gb),
            model_file: Some(3 * gb + gb / 2),
            model_shared: Some(0),
            model_locked: Some(38 * gb),
            model_cgroup: Some(53 * gb),
            disk_read: 148.2,
            disk_write: 4.6,
            processes: vec![
                GpuProcess {
                    pid: 3492,
                    name: "basalt".into(),
                    kind: "C".into(),
                    vram: Some(11264 << 20),
                    rss: 49 * gb,
                    owned: true,
                },
                GpuProcess {
                    pid: 5520,
                    name: "RuneLite.AppImage".into(),
                    kind: "G".into(),
                    vram: Some(768 << 20),
                    ..GpuProcess::default()
                },
                GpuProcess {
                    pid: 1235,
                    name: "niri".into(),
                    kind: "G".into(),
                    vram: Some(512 << 20),
                    ..GpuProcess::default()
                },
                GpuProcess {
                    pid: 1288,
                    name: "ghostty".into(),
                    kind: "G".into(),
                    vram: Some(320 << 20),
                    ..GpuProcess::default()
                },
                GpuProcess {
                    pid: 1408,
                    name: "zen".into(),
                    kind: "G".into(),
                    vram: Some(256 << 20),
                    ..GpuProcess::default()
                },
            ],
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_paths_find_cpu_and_dimm_sensors() {
        let root = std::env::temp_dir().join(format!("basalt-wrap-hwmon-{}", std::process::id()));
        let sensor = |dir: &str, name: &str, temp: u32, label: Option<&str>, millidegrees: i64| {
            let dir = root.join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("name"), format!("{name}\n")).unwrap();
            std::fs::write(
                dir.join(format!("temp{temp}_input")),
                format!("{millidegrees}\n"),
            )
            .unwrap();
            if let Some(label) = label {
                std::fs::write(dir.join(format!("temp{temp}_label")), format!("{label}\n"))
                    .unwrap();
            }
        };
        sensor("hwmon0", "nvme", 1, None, 40_000);
        sensor("hwmon1", "k10temp", 1, Some("Tctl"), 74_125);
        sensor("hwmon1", "k10temp", 3, Some("Tccd1"), 74_625);
        sensor("hwmon2", "spd5118", 1, None, 53_400);
        sensor("hwmon3", "spd5118", 1, None, 51_250);
        let (cpu, ram) = temp_paths_in(&root);
        assert_eq!(read_temp(cpu.as_deref().unwrap()), Some(74));
        let mut ram: Vec<_> = ram.iter().filter_map(|p| read_temp(p)).collect();
        ram.sort();
        assert_eq!(ram, [51, 53]);
        std::fs::remove_dir_all(&root).ok();
    }
    #[test]
    fn temp_paths_prefer_the_labeled_package_sensor() {
        let root = std::env::temp_dir().join(format!("basalt-wrap-package-{}", std::process::id()));
        let dir = root.join("hwmon0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("name"), "coretemp\n").unwrap();
        std::fs::write(dir.join("temp1_input"), "75000\n").unwrap();
        std::fs::write(dir.join("temp1_label"), "Core 0\n").unwrap();
        std::fs::write(dir.join("temp2_input"), "82000\n").unwrap();
        std::fs::write(dir.join("temp2_label"), "Package id 0\n").unwrap();
        let (cpu, ram) = temp_paths_in(&root);
        assert_eq!(read_temp(cpu.as_deref().unwrap()), Some(82));
        assert!(ram.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }
    #[test]
    fn temp_paths_fall_back_to_cpu_thermal_zones() {
        let root = std::env::temp_dir().join(format!("basalt-wrap-zones-{}", std::process::id()));
        let zone = |name: &str, kind: &str, millidegrees: i64| {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("type"), format!("{kind}\n")).unwrap();
            std::fs::write(dir.join("temp"), format!("{millidegrees}\n")).unwrap();
        };
        zone("thermal_zone0", "acpitz", 30_000);
        zone("thermal_zone1", "x86_pkg_temp", 61_400);
        assert_eq!(
            thermal_cpu_path_in(&root).as_deref().and_then(read_temp),
            Some(61)
        );
        std::fs::remove_dir_all(&root).ok();
    }
    #[test]
    #[ignore = "reads this machine's hwmon sensors"]
    fn live_hwmon_sensors_are_readable() {
        let (cpu, ram) = temp_paths();
        assert!(
            cpu.as_deref().and_then(read_temp).is_some(),
            "no cpu temperature sensor"
        );
        assert!(
            ram.iter().filter_map(|p| read_temp(p)).max().is_some(),
            "no dimm temperature sensor"
        );
    }
    #[test]
    fn live_nvml_sample_accounts_for_reserved_memory() {
        // Captured from the affected GPU in one nvmlDeviceGetMemoryInfo_v2 call.
        let mut hw = Hardware::default();
        hw.set_gpu_memory(17_094_934_528, 1_725_890_560, 411_369_472);
        assert_eq!(hw.gpu_used, 15_369_043_968);
        assert_eq!(hw.gpu_allocated(), 14_957_674_496);
        assert_eq!(hw.gpu_used + hw.gpu_free, hw.gpu_total);
        assert_eq!(
            hw.gpu_allocated() + hw.gpu_reserved + hw.gpu_free,
            hw.gpu_total
        );
        let mut policy = crate::policy::Policy::new(1024, 14336, 60., 2.);
        for now in [0., 60., 120.] {
            assert!(
                policy
                    .observe(
                        now,
                        crate::policy::Sample {
                            used: hw.gpu_used >> 20,
                            free: hw.gpu_free >> 20,
                            cache: 6000,
                            full: 8000,
                            activity: "1".into(),
                            busy: false,
                        }
                    )
                    .is_none()
            );
        }
        assert!(policy.quiet_since.is_none());
    }
}
