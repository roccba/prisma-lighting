//! Group presets (LIVE-MODEL-SPEC §8.5 step 3): `/group-preset/{i}/apply` sets the
//! group masters it stores, with a fade.
//!
//! One small thread, **parked while nothing fades** (a blocking wait on the presets'
//! triggers); while a fade runs it ticks every [`TICK`] and writes each fading master.
//!
//! - *A preset always lands:* the fade writes past arbitration
//!   ([`ParameterTree::write_unarbitrated`]), so it reaches a master even while a
//!   controller owns it.
//! - *A hand stops it:* the fade never takes ownership, so the next accepted write (a
//!   fader, a click) replaces its value. On the following tick the master no longer
//!   holds what the fade wrote, and the fade stops for that group only.
//!
//! The fade runs on wall time (seconds), in the fader's own units (the normalized
//! level, so the fader on screen moves at a steady speed). It never touches the render
//! or output threads: they read the masters wait-free as always.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use prisma_core::tree::ParameterTree;
use prisma_core::{Filter, ParamHandle, ParamId, SubscriptionCloser, TriggerCursor};

/// How often a running fade writes its masters (50 Hz: smoother than the 44 Hz output).
pub const TICK: Duration = Duration::from_millis(20);

/// One preset as the fader runs it: `(group master, target level)` pairs and the fade.
pub struct PresetPlan {
    pub trigger: ParamHandle,
    pub levels: Vec<(ParamHandle, f32)>,
    pub fade: Duration,
}

/// The running fader thread; dropping it stops and joins the thread.
pub struct GroupFader {
    closer: SubscriptionCloser,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// One master's fade in progress.
struct Fade {
    master: ParamHandle,
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
    /// What the fade last wrote (bits), to notice a hand write in between.
    last: u32,
}

impl GroupFader {
    /// Start the thread on `presets`. Triggers fired before this are not replayed.
    pub fn start(tree: Arc<ParameterTree>, presets: Vec<PresetPlan>) -> GroupFader {
        let sub = tree.subscribe(Filter::Ids(
            presets.iter().map(|p| p.trigger.id()).collect(),
        ));
        let closer = sub.closer();
        let stop = Arc::new(AtomicBool::new(false));
        let mut cursors: HashMap<ParamId, (usize, TriggerCursor)> = HashMap::new();
        for (i, p) in presets.iter().enumerate() {
            let mut c = TriggerCursor::new();
            c.poll(p.trigger.fire_count().unwrap_or(0));
            cursors.insert(p.trigger.id(), (i, c));
        }
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("prisma-group-fader".into())
                .spawn(move || {
                    // Keyed by the master's id: a new preset on the same group replaces
                    // that group's fade, other groups keep fading.
                    let mut fades: HashMap<ParamId, Fade> = HashMap::new();
                    loop {
                        let fired = if fades.is_empty() {
                            let changed = sub.recv(); // parked until a preset fires
                            if changed.is_empty() {
                                break; // closed
                            }
                            changed
                        } else {
                            std::thread::sleep(TICK);
                            sub.try_recv()
                        };
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        for h in fired {
                            let Some((i, cursor)) = cursors.get_mut(&h.id()) else {
                                continue;
                            };
                            if !cursor.poll_edge(h.fire_count().unwrap_or(0)) {
                                continue;
                            }
                            apply(&tree, &presets[*i], &mut fades);
                        }
                        advance(&tree, &mut fades, Instant::now());
                    }
                })
                .expect("spawn group fader thread")
        };
        GroupFader {
            closer,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for GroupFader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.closer.close();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Start a preset: each of its masters fades from where it is now (a cut when the
/// preset has no fade).
fn apply(tree: &ParameterTree, preset: &PresetPlan, fades: &mut HashMap<ParamId, Fade>) {
    for (master, to) in &preset.levels {
        let to = to.clamp(0.0, 1.0);
        if preset.fade.is_zero() {
            fades.remove(&master.id());
            tree.write_unarbitrated(master, to);
            continue;
        }
        let from = master.read_norm().unwrap_or(1.0);
        fades.insert(
            master.id(),
            Fade {
                master: master.clone(),
                from,
                to,
                start: Instant::now(),
                duration: preset.fade,
                last: from.to_bits(),
            },
        );
    }
}

/// One tick: every fade whose master still holds the fade's last value moves on; one
/// that does not was moved by hand and stops there; a finished one lands exactly.
fn advance(tree: &ParameterTree, fades: &mut HashMap<ParamId, Fade>, now: Instant) {
    fades.retain(|_, f| {
        if f.master.read_norm().map(f32::to_bits) != Some(f.last) {
            return false; // a hand took it
        }
        let p = (now.saturating_duration_since(f.start).as_secs_f64() / f.duration.as_secs_f64())
            .clamp(0.0, 1.0) as f32;
        // The last step writes the target itself: `from + (to − from) · 1` is not
        // always `to` in floating point, and a preset must land exactly.
        let v = if p >= 1.0 {
            f.to
        } else {
            (f.from + (f.to - f.from) * p).clamp(0.0, 1.0)
        };
        tree.write_unarbitrated(&f.master, v);
        f.last = v.to_bits();
        p < 1.0
    });
}
