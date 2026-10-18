//! The Hardware card on the overview: the Hardwarewerte of GPU and CPU.

use leptos::prelude::*;

use crate::api::{Cpu, Gpu, HardwareView, PcView};

/// "—" for a value the PC could not read.
const NONE: &str = "—";

#[component]
pub fn HardwareCard(hardware: HardwareView, pc: PcView) -> impl IntoView {
    let body = match hardware {
        HardwareView::Values(h) => {
            let gpu = h.gpu.unwrap_or_default();
            view! {
                <Row label="GPU" stats=gpu_stats(&gpu) />
                <Row label="CPU" stats=cpu_stats(&h.cpu) />
            }
            .into_any()
        }
        HardwareView::Unsupported => view! {
            <p class="muted small">
                "Update the PC service to see hardware stats. "
                <a href="/admin/pc">"Update on PC page"</a>
            </p>
        }
        .into_any(),
        HardwareView::Unavailable => {
            let text = match pc {
                PcView::Asleep => "The PC is asleep.",
                PcView::Waking => "The PC is waking up.",
                _ => "No values from the PC service yet.",
            };
            view! { <p class="muted small">{text}</p> }.into_any()
        }
    };
    view! {
        <section class="card hardware" aria-labelledby="hardware-heading">
            <h2 id="hardware-heading">"Hardware"</h2>
            {body}
        </section>
    }
}

#[component]
fn Row(label: &'static str, stats: Vec<(&'static str, String)>) -> impl IntoView {
    view! {
        <div class="hw-row">
            <h3>{label}</h3>
            <dl class="hw-stats">
                {stats
                    .into_iter()
                    .map(|(name, value)| {
                        view! {
                            <div>
                                <dt>{name}</dt>
                                <dd>{value}</dd>
                            </div>
                        }
                    })
                    .collect_view()}
            </dl>
        </div>
    }
}

fn gpu_stats(gpu: &Gpu) -> Vec<(&'static str, String)> {
    vec![
        ("Load", percent(gpu.load_pct)),
        ("Temperature", or_none(gpu.temp_c, |t| format!("{t:.0} °C"))),
        ("VRAM", memory(gpu.vram_used_bytes, gpu.vram_total_bytes)),
        ("Fan", or_none(gpu.fan_rpm, |r| format!("{r} rpm"))),
        (
            "Power",
            or_none(gpu.power_pct, |p| format!("{p:.0} % of limit")),
        ),
    ]
}

fn cpu_stats(cpu: &Cpu) -> Vec<(&'static str, String)> {
    vec![
        ("Load", percent(cpu.load_pct)),
        ("RAM", memory(cpu.ram_used_bytes, cpu.ram_total_bytes)),
    ]
}

fn or_none<T>(value: Option<T>, text: impl FnOnce(T) -> String) -> String {
    value.map_or_else(|| NONE.to_string(), text)
}

fn percent(value: Option<f32>) -> String {
    or_none(value, |p| format!("{p:.0} %"))
}

/// `11.8 / 16.0 GB`, in GiB as Windows counts them.
fn memory(used: Option<u64>, total: Option<u64>) -> String {
    let gb = |b: Option<u64>| or_none(b, |b| format!("{:.1}", b as f64 / (1u64 << 30) as f64));
    if used.is_none() && total.is_none() {
        return NONE.to_string();
    }
    format!("{} / {} GB", gb(used), gb(total))
}
