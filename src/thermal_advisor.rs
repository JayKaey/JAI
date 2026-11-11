//! The thermal advisor: an hour of CPU and GPU temperatures against their load, a verdict in
//! plain words and a fan curve to match ("how are my temperatures", "is my PC running hot",
//! "fan curve advice").
//!
//! A sample is taken every ten seconds from the telemetry JAI reads anyway and kept in
//! memory for an hour. The verdict looks at what the numbers mean together: hot under load is
//! one thing, hot while idle another, and a card that sits at its limit while the fans could
//! still turn faster is a fan curve that starts too late. The advice is arithmetic on Sir's
//! own measurements; JAI changes no fan setting himself.

use crate::features::{spoken, str_arg};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Seconds between two samples, and how many are kept (one hour).
pub const SAMPLE_SECS: u64 = 10;
pub const KEEP: usize = 360;

/// Where a part runs hot, and where it throttles itself.
pub const CPU_WARM: f32 = 80.0;
pub const CPU_LIMIT: f32 = 92.0;
pub const GPU_WARM: f32 = 78.0;
pub const GPU_LIMIT: f32 = 86.0;
/// Load above which a part is working (percent), and below which it is resting.
pub const BUSY: f32 = 60.0;
pub const RESTING: f32 = 20.0;
/// A resting part warmer than this has an airflow problem.
pub const IDLE_WARM: f32 = 58.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub cpu_temp: Option<f32>,
    pub cpu_load: f32,
    pub gpu_temp: Option<f32>,
    pub gpu_load: Option<f32>,
}

/// What an hour of one part's temperatures says.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub part: &'static str,
    pub now: f32,
    pub peak: f32,
    /// Mean temperature while working / while resting (None: it never did).
    pub busy_mean: Option<f32>,
    pub rest_mean: Option<f32>,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Comfortable.
    Cool,
    /// Warm under load: fine, with little headroom.
    Warm,
    /// At its limit under load: it is throttling or about to.
    Hot,
    /// Warm while resting: dust, airflow or a fan that is not turning.
    WarmAtRest,
}

fn mean(values: &[f32]) -> Option<f32> {
    (!values.is_empty()).then(|| values.iter().sum::<f32>() / values.len() as f32)
}

/// One part's reading from its (temperature, load) pairs, oldest first.
pub fn read(part: &'static str, pairs: &[(f32, f32)], warm: f32, limit: f32) -> Option<Reading> {
    let (now, _) = *pairs.last()?;
    let peak = pairs.iter().map(|p| p.0).fold(f32::MIN, f32::max);
    let busy: Vec<f32> = pairs.iter().filter(|p| p.1 >= BUSY).map(|p| p.0).collect();
    let rest: Vec<f32> = pairs.iter().filter(|p| p.1 <= RESTING).map(|p| p.0).collect();
    let (busy_mean, rest_mean) = (mean(&busy), mean(&rest));
    // A handful of samples at the limit is a spike; a sustained share of the working time is heat.
    let at_limit = busy.iter().filter(|t| **t >= limit).count();
    let verdict = if at_limit >= 3 && at_limit * 5 >= busy.len() {
        Verdict::Hot
    } else if rest_mean.is_some_and(|t| t >= IDLE_WARM) && rest.len() >= 6 {
        Verdict::WarmAtRest
    } else if busy_mean.is_some_and(|t| t >= warm) || peak >= limit {
        Verdict::Warm
    } else {
        Verdict::Cool
    };
    Some(Reading { part, now, peak, busy_mean, rest_mean, verdict })
}

/// A fan curve for a part that peaked at `peak` under load with the limit `limit`: five points
/// (temperature in °C, fan speed in percent). Quiet while it rests, full speed five degrees
/// before the limit; the hotter the part ran, the earlier the ramp begins.
pub fn fan_curve(peak: f32, limit: f32) -> [(u32, u32); 5] {
    let full_at = (limit - 5.0).round();
    // The ramp's start: 50 °C for a part that reached its limit, up to 60 °C for a cool one.
    let headroom = (limit - peak).clamp(0.0, 20.0);
    let start = (50.0 + headroom * 0.5).round();
    let step = (full_at - start) / 3.0;
    [(40, 25), (start as u32, 35), ((start + step).round() as u32, 55), ((start + 2.0 * step).round() as u32, 78), (full_at as u32, 100)]
}

/// The samples of the last hour.
#[derive(Clone, Default)]
pub struct History {
    samples: Arc<Mutex<VecDeque<Sample>>>,
}

impl History {
    pub fn push(&self, sample: Sample) {
        let mut s = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        if s.len() >= KEEP {
            s.pop_front();
        }
        s.push_back(sample);
    }

    pub fn len(&self) -> usize {
        self.samples.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The CPU's and the GPU's readings (a part without a temperature sensor has none).
    pub fn readings(&self) -> Vec<Reading> {
        let s = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        let cpu: Vec<(f32, f32)> = s.iter().filter_map(|x| Some((x.cpu_temp?, x.cpu_load))).collect();
        let gpu: Vec<(f32, f32)> = s.iter().filter_map(|x| Some((x.gpu_temp?, x.gpu_load.unwrap_or(0.0)))).collect();
        [read("CPU", &cpu, CPU_WARM, CPU_LIMIT), read("GPU", &gpu, GPU_WARM, GPU_LIMIT)].into_iter().flatten().collect()
    }

    /// The last `count` temperatures of a part as a strip of bars (40-100 °C).
    fn strip(&self, gpu: bool, count: usize) -> String {
        let s = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        let temps: Vec<f32> = s.iter().filter_map(|x| if gpu { x.gpu_temp } else { x.cpu_temp }).collect();
        let step = (temps.len() / count.max(1)).max(1);
        temps.chunks(step).map(|c| c.iter().copied().fold(f32::MIN, f32::max)).map(|t| ['.', ':', '-', '=', '+', '*', '#', '@'][(((t - 40.0) / 60.0).clamp(0.0, 0.999) * 8.0) as usize]).collect()
    }

    /// The `thermal_advisor` tool.
    pub fn tool(&self, args: &Value) -> Result<String, String> {
        let readings = self.readings();
        if readings.is_empty() {
            return Ok(spoken("No temperature readings yet.", if self.is_empty() { "I have only just started watching the temperatures, Sir. Ask me again in a minute." } else { "This machine reports no temperature sensors to me, Sir." }));
        }
        let minutes = (self.len() as u64 * SAMPLE_SECS / 60).max(1);
        let mut lines: Vec<String> = vec![format!("Thermals, last {minutes} min")];
        let mut advice: Vec<String> = Vec::new();
        for r in &readings {
            let limit = if r.part == "CPU" { CPU_LIMIT } else { GPU_LIMIT };
            lines.push(format!("{}  now {:.0} °C, peak {:.0} °C (limit {limit:.0} °C){}{}", r.part, r.now, r.peak, r.busy_mean.map(|t| format!(", {t:.0} °C under load")).unwrap_or_default(), r.rest_mean.map(|t| format!(", {t:.0} °C at rest")).unwrap_or_default()));
            lines.push(format!("     {}", self.strip(r.part == "GPU", 48)));
            match r.verdict {
                Verdict::Hot => {
                    let curve = fan_curve(r.peak, limit);
                    advice.push(format!("{} sits at its limit under load. Fan curve: {}.", r.part, curve.iter().map(|(t, pct)| format!("{t} °C → {pct} %")).collect::<Vec<_>>().join(", ")));
                }
                Verdict::WarmAtRest => advice.push(format!("{} is {:.0} °C while it rests: check the dust filters and that every fan turns.", r.part, r.rest_mean.unwrap_or(r.now))),
                Verdict::Warm => {
                    let curve = fan_curve(r.peak, limit);
                    advice.push(format!("{} runs warm under load ({:.0} °C peak). An earlier ramp gives it headroom: {}.", r.part, r.peak, curve.iter().map(|(t, pct)| format!("{t} °C → {pct} %")).collect::<Vec<_>>().join(", ")));
                }
                Verdict::Cool => {}
            }
        }
        let wants_curve = matches!(str_arg(args, &["action"]).unwrap_or("status").to_lowercase().as_str(), "advice" | "fan_curve" | "curve");
        if advice.is_empty() && wants_curve {
            for r in &readings {
                let limit = if r.part == "CPU" { CPU_LIMIT } else { GPU_LIMIT };
                advice.push(format!("{} has headroom; a quiet curve is enough: {}.", r.part, fan_curve(r.peak, limit).iter().map(|(t, pct)| format!("{t} °C → {pct} %")).collect::<Vec<_>>().join(", ")));
            }
        }
        lines.extend(advice.iter().cloned());
        let worst = readings.iter().max_by_key(|r| match r.verdict {
            Verdict::Hot => 3,
            Verdict::WarmAtRest => 2,
            Verdict::Warm => 1,
            Verdict::Cool => 0,
        });
        let line = match worst.map(|r| (r.part, r.verdict, r.peak, r.rest_mean.unwrap_or(r.now))) {
            Some((part, Verdict::Hot, peak, _)) => format!("The {part} is at its limit, Sir: {peak:.0} degrees under load. I've put a fan curve on the board."),
            Some((part, Verdict::WarmAtRest, _, rest)) => format!("The {part} is {rest:.0} degrees while it rests, Sir. That is airflow, not load: the filters and the fans are worth a look."),
            Some((part, Verdict::Warm, peak, _)) => format!("Warm but within limits, Sir. The {part} peaked at {peak:.0} degrees."),
            _ => {
                let said: Vec<String> = readings.iter().map(|r| format!("{} at {:.0}", r.part, r.now)).collect();
                format!("All cool, Sir: {} degrees.", said.join(", "))
            }
        };
        Ok(spoken(lines.join("\n"), line))
    }
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "thermal_advisor",
        "description": "An hour of CPU and GPU temperatures against their load: a verdict (cool, warm, at the limit, warm while resting) and, with action advice, a fan curve fitted to the measurements. Use it for 'how are my temperatures', 'is my PC running hot', 'fan curve'.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["status", "advice"]}
        }, "required": []}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(history: &History, n: usize, cpu: (f32, f32), gpu: (f32, f32)) {
        for _ in 0..n {
            history.push(Sample { cpu_temp: Some(cpu.0), cpu_load: cpu.1, gpu_temp: Some(gpu.0), gpu_load: Some(gpu.1) });
        }
    }

    #[test]
    fn the_verdict_reads_temperature_against_load() {
        // Resting cool, gaming warm on the CPU and at the limit on the GPU.
        let h = History::default();
        fill(&h, 60, (42.0, 8.0), (45.0, 5.0));
        fill(&h, 120, (83.0, 85.0), (87.0, 98.0));
        let readings = h.readings();
        assert_eq!(readings.iter().map(|r| (r.part, r.verdict)).collect::<Vec<_>>(), vec![("CPU", Verdict::Warm), ("GPU", Verdict::Hot)]);
        assert_eq!((readings[1].peak, readings[1].busy_mean, readings[1].rest_mean), (87.0, Some(87.0), Some(45.0)));
        let said = h.tool(&json!({})).unwrap();
        assert!(said.contains("GPU sits at its limit under load. Fan curve:") && said.contains("81 °C → 100 %"), "{said}");
        assert!(said.ends_with("Spoken: The GPU is at its limit, Sir: 87 degrees under load. I've put a fan curve on the board."), "{said}");
        // Everything comfortable.
        let h = History::default();
        fill(&h, 100, (48.0, 30.0), (52.0, 40.0));
        assert!(h.readings().iter().all(|r| r.verdict == Verdict::Cool));
        assert!(h.tool(&json!({})).unwrap().ends_with("Spoken: All cool, Sir: CPU at 48, GPU at 52 degrees."));
        assert!(h.tool(&json!({"action": "advice"})).unwrap().contains("CPU has headroom; a quiet curve is enough:"));
        // Warm while it rests is airflow, not load.
        let h = History::default();
        fill(&h, 40, (63.0, 6.0), (44.0, 3.0));
        assert_eq!(h.readings()[0].verdict, Verdict::WarmAtRest);
        assert!(h.tool(&json!({})).unwrap().contains("airflow, not load"));
        // A two-sample spike at the limit is not heat.
        let h = History::default();
        fill(&h, 60, (70.0, 80.0), (70.0, 90.0));
        fill(&h, 2, (93.0, 99.0), (70.0, 90.0));
        assert_eq!(h.readings()[0].verdict, Verdict::Warm, "the peak is noted, the verdict stays below Hot");
    }

    #[test]
    fn the_history_keeps_an_hour_and_a_machine_without_sensors_says_so() {
        let h = History::default();
        assert!(h.tool(&json!({})).unwrap().contains("only just started"));
        for _ in 0..KEEP + 50 {
            h.push(Sample { cpu_temp: None, cpu_load: 10.0, gpu_temp: None, gpu_load: None });
        }
        assert_eq!(h.len(), KEEP);
        assert!(h.tool(&json!({})).unwrap().contains("no temperature sensors"));
    }

    #[test]
    fn a_fan_curve_rises_and_starts_earlier_for_a_hotter_part() {
        for (peak, limit) in [(87.0, GPU_LIMIT), (60.0, GPU_LIMIT), (95.0, CPU_LIMIT), (70.0, CPU_LIMIT)] {
            let curve = fan_curve(peak, limit);
            assert!(curve.windows(2).all(|w| w[1].0 > w[0].0 && w[1].1 > w[0].1), "{peak}/{limit}: {curve:?}");
            assert_eq!(curve[4], ((limit - 5.0) as u32, 100));
            assert_eq!(curve[0], (40, 25));
        }
        assert!(fan_curve(87.0, GPU_LIMIT)[1].0 < fan_curve(60.0, GPU_LIMIT)[1].0, "a card at its limit ramps up earlier");
        assert_eq!(fan_curve(86.0, GPU_LIMIT), [(40, 25), (50, 35), (60, 55), (71, 78), (81, 100)]);
    }
}
