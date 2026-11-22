//! CPU and memory usage for the bar's `cpu` and `memory` widgets and the
//! system panel.
//!
//! Sampling is two cheap system calls (`GetSystemTimes`,
//! `GlobalMemoryStatusEx`), so it runs on the main thread whenever a bar or
//! the panel asks, at most about once a second. CPU usage is the share of
//! non-idle time since the previous sample.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::mem::{size_of, zeroed};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::GetSystemTimes;

/// Samples kept for the panel's graphs (one per second).
pub const HISTORY: usize = 60;
const MIN_INTERVAL: Duration = Duration::from_millis(900);

/// The latest usage.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// 0.0 - 1.0 of all cores.
    pub cpu: f32,
    /// 0.0 - 1.0 of physical memory.
    pub memory: f32,
    pub memory_used: u64,
    pub memory_total: u64,
}

struct Sampler {
    last: Option<(Instant, u64, u64)>,
    usage: Usage,
    cpu_history: VecDeque<f32>,
    memory_history: VecDeque<f32>,
}

thread_local! {
    static SAMPLER: RefCell<Sampler> = const {
        RefCell::new(Sampler { last: None, usage: Usage { cpu: 0.0, memory: 0.0, memory_used: 0, memory_total: 0 },
            cpu_history: VecDeque::new(), memory_history: VecDeque::new() })
    };
}

fn ticks(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

/// Current usage, re-sampled if the last sample is about a second old.
pub fn usage() -> Usage {
    SAMPLER.with(|s| {
        let mut s = s.borrow_mut();
        if s.last.is_none_or(|(at, _, _)| at.elapsed() >= MIN_INTERVAL) {
            s.sample();
        }
        s.usage
    })
}

/// CPU and memory usage over the last [`HISTORY`] samples, oldest first.
pub fn history() -> (Vec<f32>, Vec<f32>) {
    usage();
    SAMPLER.with(|s| {
        let s = s.borrow();
        (s.cpu_history.iter().copied().collect(), s.memory_history.iter().copied().collect())
    })
}

/// Time since Windows started.
pub fn uptime() -> Duration {
    Duration::from_millis(unsafe { GetTickCount64() })
}

impl Sampler {
    fn sample(&mut self) {
        unsafe {
            let (mut idle, mut kernel, mut user): (FILETIME, FILETIME, FILETIME) = (zeroed(), zeroed(), zeroed());
            if GetSystemTimes(&mut idle, &mut kernel, &mut user) != 0 {
                // Kernel time includes idle time.
                let (busy_total, idle) = (ticks(kernel) + ticks(user), ticks(idle));
                if let Some((_, last_total, last_idle)) = self.last {
                    let total = busy_total.saturating_sub(last_total);
                    if total > 0 {
                        let idle = idle.saturating_sub(last_idle).min(total);
                        self.usage.cpu = 1.0 - idle as f32 / total as f32;
                    }
                }
                self.last = Some((Instant::now(), busy_total, idle));
            }
            let mut mem: MEMORYSTATUSEX = zeroed();
            mem.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut mem) != 0 {
                self.usage.memory_total = mem.ullTotalPhys;
                self.usage.memory_used = mem.ullTotalPhys.saturating_sub(mem.ullAvailPhys);
                self.usage.memory = self.usage.memory_used as f32 / mem.ullTotalPhys.max(1) as f32;
            }
        }
        for (history, value) in [(&mut self.cpu_history, self.usage.cpu), (&mut self.memory_history, self.usage.memory)]
        {
            history.push_back(value.clamp(0.0, 1.0));
            while history.len() > HISTORY {
                history.pop_front();
            }
        }
    }
}
