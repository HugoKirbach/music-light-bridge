//! Vérification du flux : arc-en-ciel à 60 Hz et mesure de la régularité d'envoi.

use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::runtime::Runtime;

use crate::config::Config;
use crate::hue::api::{self, EntertainmentArea};
use crate::hue::stream::{FRAME_PERIOD, HueStream, Rgb};
use crate::timing;

const CYCLE_SECS: f32 = 4.0;

#[derive(Default)]
struct Stats {
    frames: u32,
    total_late: Duration,
    max_late: Duration,
    max_send: Duration,
}

pub fn run(rt: &Runtime, cfg: &Config, duration: Duration) -> Result<()> {
    let bridge = cfg.bridge()?;
    let area = rt.block_on(api::select_area(bridge, cfg.area_id.as_deref()))?;
    println!(
        "Zone « {} » ({} canal(aux)) : ouverture du flux…",
        area.metadata.name,
        area.channels.len()
    );
    let mut stream = rt.block_on(HueStream::open(bridge, &area))?;
    println!("Flux DTLS établi. Arc-en-ciel pendant {} s.", duration.as_secs());

    let result = rainbow(rt, &mut stream, &area, duration);
    rt.block_on(stream.close())?;
    let stats = result?;

    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    println!(
        "{} paquets envoyés. Retard de réveil : moyen {:.2} ms, max {:.2} ms. Envoi DTLS : max {:.2} ms.",
        stats.frames,
        ms(stats.total_late) / f64::from(stats.frames.max(1)),
        ms(stats.max_late),
        ms(stats.max_send),
    );
    Ok(())
}

fn rainbow(rt: &Runtime, stream: &mut HueStream, area: &EntertainmentArea, duration: Duration) -> Result<Stats> {
    let mut frame: Vec<(u8, Rgb)> = area.channels.iter().map(|c| (c.channel_id, Rgb::OFF)).collect();
    let spread = frame.len() as f32;
    let mut stats = Stats::default();
    let start = Instant::now();
    let mut next = start;
    while next < start + duration {
        timing::sleep_until(next);
        let woke = Instant::now();
        let t = (woke - start).as_secs_f32();
        for (i, (_, color)) in frame.iter_mut().enumerate() {
            *color = Rgb::from_hsv(t / CYCLE_SECS + i as f32 / spread, 1.0, 1.0);
        }
        rt.block_on(stream.send(&frame))?;

        let late = woke - next;
        stats.frames += 1;
        stats.total_late += late;
        stats.max_late = stats.max_late.max(late);
        stats.max_send = stats.max_send.max(woke.elapsed());
        next += FRAME_PERIOD;
    }
    Ok(stats)
}
