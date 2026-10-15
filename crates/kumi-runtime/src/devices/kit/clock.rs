//! Kumi's timing building blocks: [kumi.transport], [kumi.clock] and [kumi.notes]. Each is a subpatcher of Max's own
//! objects that the producer can open, with its timing decided in gen~ at every sample.
//!
//! Live gives a Max for Live device two views of its song position:
//! - plugphasor~ is the beat's phase, exact at every sample (0 while Live is stopped);
//! - plugsync~ says where the song is in beats, but Max polls it once per signal vector, so while Live plays it's a
//!   vector or two late.
//!
//! Kumi's transport takes the sample from the phase and the whole beats from plugsync~. It sorts out the races between
//! the two: a start, where the phase moves before plugsync~ says Live plays; a stop, where the phase drops to 0 while
//! plugsync~ still moves on; and a jump. When the song position jumps by whole beats, the phase can't show it:
//! Live's own loop is foreseen from its loop settings, and any other jump is put right when plugsync~ shows it.
//!
//! Kumi's clock takes its steps from that position, on the grid of its division. The rules:
//! - A step starts on the sample it's due.
//! - After a start or a jump, the steps follow the song position.
//! - A change of division, swing or length lands on the next step of the new grid. It never doubles a step or puts
//!   one off the grid, and two steps are never closer than the shorter of the old and new grids' shortest steps.
//!
//! Its notes end in milliseconds worked out from the tempo, so no note waits on a transport that's stopped or has
//! jumped. They all end when Live stops, when the device is turned off and when it's deleted.

/// The transport's GenExpr: in1 is plugphasor~, in2 plugsync~'s song position in beats; Params say Live's loop.
/// out1: the song position in beats; out2: 1 while Live plays; out3: 1 while the position is sure; out4: how many
/// samples late a decision made now is; out5: the tempo (BPM); out6: 1 where Live started or moved the song position,
/// 2 where plugsync~ showed a jump by whole beats that the phase couldn't.
pub const TRANSPORT: &str = r#"// Made by Kumi: Live's transport at every sample (fixed).
// in1: plugphasor~, the beat's phase, exact at every sample (0 while Live is stopped).
// in2: plugsync~'s song position in beats, polled once per signal vector, so a vector or two late while Live plays:
// it gives the whole beats, and the phase gives the rest.
Param loop_on(0);
Param loop_start(0);
Param loop_length(4);
History t_phase(0);
History t_position(0);
History t_advance(0);
History t_playing(0);
History t_sure(0);
History t_heard(0);
History t_waited(0);
History t_zero(0);
History t_starting(0);
History t_offset(0);
History t_confirmed(0);
History t_held(0);
phase = in1;
heard = in2;
was_phase = t_phase;
was_position = t_position;
advance = t_advance;
playing = t_playing;
sure = t_sure;
was_heard = t_heard;
waited = t_waited;
zero = t_zero;
starting = t_starting;
offset = t_offset;
confirmed = t_confirmed;
held = t_held;
hold = 0;
moved = 0;
late = 0;
position = was_position;
fresh = heard != was_heard;
whole = floor(heard - phase + 0.5);
behind = whole - (heard - phase);
agrees = behind > -0.0005 && behind < vectorsize * 3 * max(advance, 0.00002) + 0.0005;
// apart: the whole beats plugsync~'s latest poll puts the song position off by. A poll taken before a jump can still
// agree with the phase after it (two can, when plugsync~ is two vectors late), so a jump counts once three polls in a
// row say the same.
apart = 0;
loop_end = loop_start + loop_length;
if (playing < 0.5) {
    // Stopped: plugsync~ says at once where Live will start.
    position = heard;
    sure = 0;
    if (phase != 0) {
        playing = 1;
        starting = 1;
        position = whole + phase;
        zero = 0;
        waited = 0;
    }
} else if (starting > 0.5) {
    // The first sample after the start: the phase's advance is the tempo.
    advance = phase - was_phase;
    advance = advance - floor(advance + 0.5);
    advance = max(advance, 0.0000001);
    position = was_position + advance;
    starting = 0;
    if (agrees > 0.5) {
        sure = 1;
        moved = 1;
        late = 2;
    } else {
        sure = 0;
        waited = 2;
    }
} else {
    predicted = was_position + advance;
    // Live's arrangement loop jumps back where it ends, for a song position that reached it from inside.
    looped = loop_on > 0.5 && loop_length > 0 && was_position >= loop_start && was_position < loop_end && predicted >= loop_end;
    if (looped > 0.5) {
        predicted = predicted - loop_length;
    }
    off = phase - (predicted - floor(predicted));
    off = off - floor(off + 0.5);
    if (phase == 0) {
        // The phase is exactly 0: Live stopped (plugphasor~ is 0 while stopped), or a beat's line fell on this very
        // sample. The song position moves on as it was, nothing starts on this sample, and the next sample says which:
        // still 0 is stopped.
        zero = zero + 1;
        position = predicted;
        hold = 1;
        if (zero > 1.5) {
            playing = 0;
            sure = 0;
            position = heard;
        }
    } else if (abs(off) < min(0.0002, advance * 0.5)) {
        // Moving as it was; a tempo change moves the phase on a little more or less.
        position = predicted + off;
        advance = max(advance + off, 0.0000001);
        zero = 0;
        if (held > 0.5) {
            // A step due on the sample held at 0 starts now, a sample late.
            late = 1;
        }
        if (looped > 0.5 && sure > 0.5) {
            // The steps follow the song position back to the loop's start.
            moved = 1;
        }
        apart = whole - floor(position - phase + 0.5);
        if (fresh > 0.5) {
            if (agrees > 0.5 && (sure < 0.5 || apart != 0)) {
                if (apart == offset && confirmed > 0.5) {
                    confirmed = confirmed + 1;
                } else {
                    offset = apart;
                    confirmed = 1;
                }
            } else {
                confirmed = 0;
            }
        }
        if (sure < 0.5) {
            waited = waited + 1;
            if (confirmed > 2.5) {
                // Three polls agree where the song position jumped to.
                position = position + offset;
                sure = 1;
                moved = 1;
                late = waited;
                waited = 0;
                confirmed = 0;
            }
        } else if (confirmed > 2.5) {
            // plugsync~ has caught up with a jump by whole beats, which the phase couldn't show.
            position = position + offset;
            moved = 2;
            late = vectorsize * 4;
            confirmed = 0;
        }
    } else {
        // The phase jumped: Live moved the song position. plugsync~ gives its whole beats a vector or two later.
        position = whole + phase;
        sure = 0;
        waited = 0;
        zero = 0;
        confirmed = 0;
    }
}
t_phase = phase;
t_position = position;
t_advance = advance;
t_playing = playing;
t_sure = sure;
t_heard = heard;
t_waited = waited;
t_zero = zero;
t_starting = starting;
t_offset = offset;
t_confirmed = confirmed;
t_held = hold;
out1 = position;
out2 = playing;
out3 = sure * (1 - hold);
out4 = late;
out5 = advance * 60 * samplerate;
out6 = moved;
"#;

/// The clock's GenExpr: in1–in6 are the transport's outlets; Params set a step's length in beats (division), how late
/// every second step comes as a fraction of a step (swing, 0 to 0.5) and the steps before the pattern starts again
/// (length). out1: 1 on the sample a step starts; out2: that step's index, from 0; out3: its length in milliseconds at
/// the tempo then; out4: 1 while Live plays.
///
/// Each step's index and length also go into the Buffer `memory` (samples 0 and 1) on the sample it starts, for the
/// patch to read with [peek~] when edge~ answers the pulse. snapshot~ can't be used for them: in Max for Live it reads
/// the first sample of its vector, before a step that starts inside it.
pub const CLOCK: &str = r#"// Made by Kumi: the clock's steps, each on the sample it's due (fixed).
// in1 to in6: Kumi's transport (the song position in beats, playing, sure, how late, the tempo, moved).
Param division(0.25);
Param swing(0);
Param length(16);
Buffer memory;
History c_count(-1);
History c_index(0);
History c_shortest(0.25);
History c_follow(1);
History c_keep(0);
History c_pair(-1);
History c_second(0);
History c_length(0);
History c_beats(1000000000);
History c_each(0);
History c_fired_pair(-1);
position = in1;
playing = in2;
sure = in3;
late = in4;
tempo = in5;
moved = in6;
was_count = c_count;
index = c_index;
was_shortest = c_shortest;
follow = c_follow;
keep = c_keep;
was_pair = c_pair;
was_second = c_second;
step_ms = c_length;
was_each = c_each;
fired_pair = c_fired_pair;
if (playing < 0.5) {
    follow = 1;
    keep = 0;
}
if (moved > 0.5) {
    follow = 1;
    keep = moved > 1.5;
    fired_pair = -1;
}
each = max(division, 0.001);
amount = clamp(swing, 0, 0.5);
steps = max(floor(length + 0.5), 1);
pair = each * 2;
offbeat = each + each * amount;
shortest = pair - offbeat;
pairs = floor(position / pair);
into = position - pairs * pair;
second = into >= offbeat;
count = pairs * 2 + second;
begun = pairs * pair + second * offbeat;
since = position - begun;
per_sample = max(tempo, 1) / 60 / samplerate;
tolerance = (late + 1.5) * per_sample;
// How far apart in beats steps are kept, and how many beats have passed since the last: beats, not samples, so a
// tempo that falls between two steps doesn't hold the second back.
since_beats = c_beats + per_sample;
fewest = min(shortest, was_shortest) - 1.5 * per_sample;
same_grid = abs(each - was_each) < 0.0000001;
if (same_grid < 0.5) {
    fired_pair = -1;
}
// A swing change can move the second step of a pair to before the sample now: it starts now, not never. Only with
// the division as it was: another division's pairs are other pairs.
swung = sure > 0.5 && same_grid > 0.5 && pairs == was_pair && second > 0.5 && was_second < 0.5;
due = since < tolerance || swung;
// A pair's second step starts once, however the swing moves it.
again = second > 0.5 && pairs == fired_pair;
fire = 0;
if (playing > 0.5 && sure > 0.5 && due && again < 0.5 && (count != was_count || follow > 0.5)) {
    if ((follow > 0.5 && keep < 0.5) || since_beats >= fewest) {
        fire = 1;
    }
}
if (fire > 0.5) {
    if (follow > 0.5) {
        index = count - floor(count / steps) * steps;
    } else {
        index = index + 1;
        if (index >= steps) {
            index = 0;
        }
    }
    // Measured from the step's line, not the sample it started on (a step after a start or a jump can start late);
    // a second step the swing moved behind the sample now counts from now.
    since_beats = 0;
    if (since < tolerance) {
        since_beats = since;
    }
    was_shortest = shortest;
    follow = 0;
    keep = 0;
    if (second > 0.5) {
        fired_pair = pairs;
    }
    step_ms = offbeat * 60000 / max(tempo, 1);
    if (second > 0.5) {
        step_ms = shortest * 60000 / max(tempo, 1);
    }
    poke(memory, index, 0, 0);
    poke(memory, step_ms, 1, 0);
}
if (sure > 0.5) {
    was_count = count;
    was_pair = pairs;
    was_second = second;
}
c_count = was_count;
c_index = index;
c_shortest = was_shortest;
c_follow = follow;
c_keep = keep;
c_pair = was_pair;
c_second = was_second;
c_length = step_ms;
c_beats = since_beats;
c_each = each;
c_fired_pair = fired_pair;
out1 = fire;
out2 = index;
out3 = step_ms;
out4 = playing;
"#;

#[cfg(test)]
mod tests {
    use super::super::genexpr::Gen;
    use super::*;

    const RATE: f64 = 44_100.0;
    const VECTOR: usize = 64;

    /// Live as a Max for Live device sees it, one sample at a time: the song position moves at the tempo while Live
    /// plays, plugphasor~ is its phase (0 while stopped), and plugsync~'s position is polled once a vector, `lag` vectors
    /// late (0 or 1).
    struct Live {
        position: f64,
        bpm: f64,
        playing: bool,
        heard: f64,
        lag: usize,
        polls: Vec<f64>,
        loop_on: bool,
        loop_start: f64,
        loop_length: f64,
        sample: usize,
    }

    impl Live {
        fn new(lag: usize) -> Live {
            Live {
                position: 0.0,
                bpm: 120.0,
                playing: false,
                heard: 0.0,
                lag,
                polls: Vec::new(),
                loop_on: false,
                loop_start: 0.0,
                loop_length: 4.0,
                sample: 0,
            }
        }

        /// One sample: plugphasor~ and plugsync~'s values, then the song moves on.
        fn tick(&mut self) -> (f64, f64) {
            if self.sample % VECTOR == 0 {
                self.polls.push(self.position);
                let at = self.polls.len().saturating_sub(1 + self.lag);
                self.heard = if self.playing { self.polls[at] } else { self.position };
            }
            let phase = if self.playing { self.position - self.position.floor() } else { 0.0 };
            let out = (phase, self.heard);
            if self.playing {
                let next = self.position + self.bpm / 60.0 / RATE;
                let end = self.loop_start + self.loop_length;
                self.position = if self.loop_on && self.position >= self.loop_start && self.position < end && next >= end {
                    next - self.loop_length
                } else {
                    next
                };
            }
            self.sample += 1;
            out
        }
    }

    /// Kumi's transport and clock, wired as [kumi.clock] wires them.
    struct Device {
        transport: Gen,
        clock: Gen,
    }

    impl Device {
        fn new(delayed: bool, division: f64, length: f64) -> Device {
            let mut device = Device { transport: Gen::new(TRANSPORT, delayed), clock: Gen::new(CLOCK, delayed) };
            device.clock.set("division", division);
            device.clock.set("length", length);
            device
        }
        /// One sample: whether a step starts, its index and its length in ms (read from the clock's buffer, as the
        /// patch's peek~ reads them once the vector is done), and where the true song position was.
        fn tick(&mut self, live: &mut Live) -> (bool, f64, f64, f64) {
            let truth = live.position;
            let (phase, heard) = live.tick();
            let transport = self.transport.tick(&[phase, heard], 6);
            let clock = self.clock.tick(&transport, 4);
            let memory = &self.clock.buffers[0].1;
            if clock[0] > 0.5 {
                assert_eq!((memory[0], memory[1]), (clock[1], clock[2]), "the buffer holds the step that starts");
            }
            (clock[0] > 0.5, memory[0], memory[1], truth)
        }
    }

    /// A step that started: the sample, its index, and the true song position then.
    #[derive(Debug, Clone, PartialEq)]
    struct Step {
        sample: usize,
        index: usize,
        position: f64,
    }

    /// `samples` of the song through Kumi's clock, `between` running before each vector (Live's settings moving).
    fn run(device: &mut Device, live: &mut Live, samples: usize, mut between: impl FnMut(&mut Device, &mut Live, usize)) -> Vec<Step> {
        let mut steps = Vec::new();
        for _ in 0..samples {
            if live.sample % VECTOR == 0 {
                between(device, live, live.sample);
            }
            let sample = live.sample;
            let (fire, index, _, position) = device.tick(live);
            if fire {
                steps.push(Step { sample, index: index as usize, position });
            }
        }
        steps
    }

    /// Every combination the clock has to be right in: plugsync~ on time or a vector late, and a History's new value
    /// read at once or at the next sample. Each gives the same steps.
    fn each_way(test: impl Fn(usize, bool) -> Vec<Step>) -> Vec<Step> {
        let first = test(0, false);
        for (lag, delayed) in [(0, true), (1, false), (1, true), (2, false)] {
            let again = test(lag, delayed);
            assert_eq!(first.len(), again.len(), "lag {lag}, delayed {delayed}: {first:?}\n{again:?}");
            for (a, b) in first.iter().zip(&again) {
                assert_eq!(a.index, b.index, "lag {lag}, delayed {delayed}");
                assert!(a.sample.abs_diff(b.sample) <= 4 * VECTOR, "lag {lag}, delayed {delayed}: {a:?} {b:?}");
            }
        }
        first
    }

    /// How far a step's sample is from the sample its grid line falls in, in samples.
    fn off_grid(step: &Step, division: f64) -> f64 {
        let line = (step.position / division).round() * division;
        (step.position - line) / (120.0 / 60.0 / RATE)
    }

    #[test]
    fn steps_start_on_the_sample_their_grid_line_falls_in_from_the_downbeat_at_play() {
        let steps = each_way(|lag, delayed| {
            let mut live = Live::new(lag);
            let mut device = Device::new(delayed, 0.25, 16.0);
            run(&mut device, &mut live, 1000, |_, _, _| {});
            live.playing = true;
            run(&mut device, &mut live, (RATE * 4.0) as usize, |_, _, _| {})
        });
        // Two bars at 120 BPM: 32 sixteenths, the first on the downbeat, indexed 0 to 15 twice.
        assert_eq!(steps.len(), 32, "{steps:?}");
        assert_eq!(steps.iter().map(|step| step.index).collect::<Vec<_>>(), (0..32).map(|n| n % 16).collect::<Vec<_>>());
        // The first is decided two samples into the song; every other on the sample its sixteenth starts.
        assert!(off_grid(&steps[0], 0.25) <= 2.5, "{:?}", steps[0]);
        for step in &steps[1..] {
            let off = off_grid(step, 0.25);
            assert!((0.0..1.0).contains(&off), "{step:?}: {off} samples");
        }
    }

    #[test]
    fn a_start_inside_a_step_waits_for_the_next_and_one_on_a_line_takes_its_step_there() {
        for (from, first_index, first_line) in [(0.1, 1, 0.25), (1.0, 4, 1.0), (5.3, 6, 5.5)] {
            let steps = each_way(|lag, delayed| {
                let mut live = Live::new(lag);
                live.position = from;
                let mut device = Device::new(delayed, 0.25, 16.0);
                run(&mut device, &mut live, 500, |_, _, _| {});
                live.playing = true;
                run(&mut device, &mut live, 30_000, |_, _, _| {})
            });
            assert_eq!(steps[0].index, first_index, "from {from}: {steps:?}");
            assert!((steps[0].position - first_line).abs() < 0.0002, "from {from}: {:?}", steps[0]);
        }
    }

    #[test]
    fn stopping_ends_the_steps_with_no_step_where_the_phase_drops_to_zero() {
        let steps = each_way(|lag, delayed| {
            let mut live = Live::new(lag);
            let mut device = Device::new(delayed, 0.25, 16.0);
            live.playing = true;
            // Stop at 5.37 beats: the phase drops to 0 there, while plugsync~ still says 5.3, which would round to a
            // step's line at 5.0.
            let stop = (5.37 / 2.0 * RATE) as usize;
            let mut steps = run(&mut device, &mut live, stop, |_, _, _| {});
            live.playing = false;
            steps.extend(run(&mut device, &mut live, 20_000, |_, _, _| {}));
            steps
        });
        assert_eq!(steps.len(), 22, "the sixteenths up to 5.25 beats: {steps:?}");
        assert!(steps.iter().all(|step| step.position < 5.3), "{steps:?}");
    }

    #[test]
    fn lives_loop_starts_its_steps_again_on_the_sample_it_jumps_back() {
        // A three-beat loop with sixteen steps of a sixteenth: the loop isn't the pattern's length, so each pass starts
        // at step 0, not where the pattern would have reached (12).
        let steps = each_way(|lag, delayed| {
            let mut live = Live::new(lag);
            live.loop_on = true;
            live.loop_start = 0.0;
            live.loop_length = 3.0;
            let mut device = Device::new(delayed, 0.25, 16.0);
            device.transport.set("loop_on", 1.0);
            device.transport.set("loop_start", 0.0);
            device.transport.set("loop_length", 3.0);
            live.playing = true;
            run(&mut device, &mut live, (RATE * 4.5) as usize, |_, _, _| {})
        });
        assert_eq!(steps.len(), 36, "{steps:?}");
        for (at, step) in steps.iter().enumerate() {
            assert_eq!(step.index, at % 12, "{step:?}");
            // The first is decided two samples into the song, as at any start.
            let most = if at == 0 { 2.5 } else { 1.0 };
            assert!((0.0..most).contains(&off_grid(step, 0.25)), "{step:?}");
        }
    }

    #[test]
    fn a_jump_lands_on_its_step_late_by_at_most_four_vectors_and_never_doubles() {
        for (to, index) in [(9.0, 4), (9.1, 5), (2.75, 11)] {
            let steps = each_way(|lag, delayed| {
                let mut live = Live::new(lag);
                let mut device = Device::new(delayed, 0.25, 16.0);
                live.playing = true;
                let mut steps = run(&mut device, &mut live, 30_000, |_, _, _| {});
                live.position = to;
                steps.extend(run(&mut device, &mut live, 30_000, |_, _, _| {}));
                steps
            });
            let jumped = steps.iter().position(|step| step.position >= to - 0.001 && step.position < to + 1.5).unwrap();
            let landed = &steps[jumped];
            assert_eq!(landed.index, index, "to {to}: {steps:?}");
            let line = (to / 0.25).ceil() * 0.25;
            assert!(landed.position >= line - 0.0001 && landed.position - line < 4.0 * VECTOR as f64 * 2.0 / RATE, "to {to}: {landed:?}");
            for pair in steps.windows(2) {
                assert!(pair[1].sample - pair[0].sample > 1000, "to {to}: {pair:?}");
            }
        }
    }

    #[test]
    fn a_jump_by_whole_bars_that_the_phase_cant_show_is_put_right_without_a_second_step() {
        // Live jumps from 6.0 beats back to 1.0 without its loop settings saying so: the phase goes on as if nothing
        // happened, and plugsync~ shows the jump a vector or two later.
        let steps = each_way(|lag, delayed| {
            let mut live = Live::new(lag);
            let mut device = Device::new(delayed, 0.25, 16.0);
            live.playing = true;
            let mut steps = run(&mut device, &mut live, (3.0 * RATE) as usize - 3, |_, _, _| {});
            live.position -= 5.0;
            steps.extend(run(&mut device, &mut live, 30_000, |_, _, _| {}));
            steps
        });
        for pair in steps.windows(2) {
            assert!(pair[1].sample - pair[0].sample > 1000, "{pair:?}");
        }
        // After the jump, the steps follow the song position again within a step.
        let after = steps.iter().filter(|step| step.position > 1.3 && step.position < 2.0).collect::<Vec<_>>();
        assert!(!after.is_empty() && after.iter().all(|step| step.index == ((step.position / 0.25).round() as usize) % 16), "{after:?}");
    }

    #[test]
    fn a_division_or_swing_moved_every_vector_never_doubles_crowds_or_starves_the_steps() {
        let divisions = [4.0, 2.0, 1.0, 0.5, 1.0 / 3.0, 0.375, 0.25, 1.0 / 6.0, 0.1875, 0.125];
        let mut seed: u64 = 7;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let picks: Vec<(f64, f64)> =
            (0..20_000).map(|_| (divisions[(random() % divisions.len() as u64) as usize], (random() % 51) as f64 / 100.0)).collect();
        let mut live = Live::new(1);
        let mut device = Device::new(false, 0.25, 16.0);
        live.playing = true;
        let mut current = Vec::new();
        let steps = run(&mut device, &mut live, 64 * 20_000, |device, _, sample| {
            let (division, swing) = picks[sample / 64];
            device.clock.set("division", division);
            device.clock.set("swing", swing);
            current.push((division, swing));
        });
        assert!(steps.len() > 100, "{}", steps.len());
        let per_beat = RATE / 2.0;
        // The shortest gap a grid makes is the second step of a swung pair: (1 − swing) × division.
        let shortest = divisions.iter().fold(f64::MAX, |low, d| low.min(d * 0.5));
        for pair in steps.windows(2) {
            let gap = (pair[1].sample - pair[0].sample) as f64 / per_beat;
            assert!(gap >= shortest - 0.0002, "{pair:?}: {gap} beats");
            assert!(gap <= 4.0 + 0.0002, "starved: {pair:?}: {gap} beats");
        }
        // Every step is on a line of some grid in play: on a division's line, or its swung second step.
        for step in &steps {
            let on = divisions.iter().any(|d| {
                let pair = d * 2.0;
                let into = step.position - (step.position / pair).floor() * pair;
                into < 0.0002 || (into > *d - 0.0002 && into < d * 1.5 + 0.0002)
            });
            assert!(on, "{step:?}");
        }
    }

    #[test]
    fn a_division_change_on_the_sample_a_line_falls_lands_one_step_there() {
        // From sixteenths to eighths at the start of beat 2, where both grids have a line: one step there, then eighths.
        let mut live = Live::new(1);
        let mut device = Device::new(false, 0.25, 16.0);
        live.playing = true;
        let change = (1.0 / 2.0 * RATE) as usize / 64 * 64;
        let steps = run(&mut device, &mut live, (2.0 * RATE) as usize, |device, _, sample| {
            if sample == change {
                device.clock.set("division", 0.5);
            }
        });
        let after: Vec<f64> = steps.iter().filter(|step| step.sample >= change).map(|step| (step.position * 2.0).round() / 2.0).collect();
        assert_eq!(after.first().copied(), Some(1.0), "{steps:?}");
        assert!(after.windows(2).all(|pair| (pair[1] - pair[0] - 0.5).abs() < 1e-9), "{after:?}");
    }

    #[test]
    fn a_tempo_change_keeps_every_step_on_its_line() {
        let mut live = Live::new(1);
        let mut device = Device::new(false, 0.25, 16.0);
        live.playing = true;
        let steps = run(&mut device, &mut live, (RATE * 8.0) as usize, |_, live, sample| {
            live.bpm = 120.0 + 40.0 * (sample as f64 / (RATE * 8.0));
        });
        assert!(steps.len() > 70);
        for step in &steps[1..] {
            let line = (step.position / 0.25).round() * 0.25;
            assert!(step.position >= line - 1e-9 && step.position - line < 160.0 / 60.0 / RATE, "{step:?}");
        }
        assert!(steps.windows(2).all(|pair| pair[1].index == (pair[0].index + 1) % 16));
    }

    #[test]
    fn a_shorter_length_starts_the_pattern_again_at_its_next_step() {
        let mut live = Live::new(0);
        let mut device = Device::new(false, 0.25, 16.0);
        live.playing = true;
        let steps = run(&mut device, &mut live, (RATE * 4.0) as usize, |device, _, sample| {
            if sample == (2.6 / 2.0 * RATE) as usize / 64 * 64 {
                device.clock.set("length", 8.0);
            }
        });
        let indices: Vec<usize> = steps.iter().map(|step| step.index).collect();
        assert_eq!(&indices[..11], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(indices[11], 0, "{indices:?}");
        assert!(indices[11..].windows(2).all(|pair| pair[1] == (pair[0] + 1) % 8), "{indices:?}");
    }

    /// The grid lines of `division` the true song position crossed between two samples, as (sample, line).
    fn lines_crossed(positions: &[(usize, f64)], division: f64) -> Vec<(usize, f64)> {
        positions
            .windows(2)
            .filter_map(|pair| {
                let line = (pair[1].1 / division).floor() * division;
                (line > pair[0].1 && line <= pair[1].1).then_some((pair[1].0, line))
            })
            .collect()
    }

    #[test]
    fn a_tempo_that_falls_drops_no_step() {
        // A ramp from 160 to 120 BPM over 8 seconds, and a step from 120 to 119 inside a step: every sixteenth plays.
        for (from, to, over) in [(160.0, 120.0, 8.0), (120.0, 119.0, 0.0)] {
            let mut live = Live::new(1);
            live.bpm = from;
            let mut device = Device::new(false, 0.25, 16.0);
            live.playing = true;
            let mut positions = Vec::new();
            let samples = (RATE * 8.0) as usize;
            let mut steps = Vec::new();
            for _ in 0..samples {
                if live.sample % VECTOR == 0 {
                    let t = live.sample as f64 / RATE;
                    live.bpm = if over > 0.0 {
                        from + (to - from) * (t / over).min(1.0)
                    } else if t > 1.1 {
                        to
                    } else {
                        from
                    };
                }
                let sample = live.sample;
                let (fire, index, _, position) = device.tick(&mut live);
                positions.push((sample, position));
                if fire {
                    steps.push(Step { sample, index: index as usize, position });
                }
            }
            let lines = lines_crossed(&positions, 0.25);
            assert_eq!(steps.len(), lines.len() + 1, "{from}→{to}: a step on every line, and the downbeat's");
            assert!(steps.windows(2).all(|pair| pair[1].index == (pair[0].index + 1) % 16));
        }
    }

    #[test]
    fn a_stop_at_a_beats_line_starts_no_step_and_leaves_the_transport_stopped() {
        for stop in [5.000_02, 4.999_98, 5.0] {
            let mut live = Live::new(1);
            let mut device = Device::new(false, 0.25, 16.0);
            live.playing = true;
            let mut steps = Vec::new();
            while live.position < stop {
                let sample = live.sample;
                let (fire, index, _, position) = device.tick(&mut live);
                if fire {
                    steps.push(Step { sample, index: index as usize, position });
                }
            }
            let before = steps.len();
            live.playing = false;
            for _ in 0..20_000 {
                let (fire, _, _, _) = device.tick(&mut live);
                assert!(!fire, "stopped at {stop}: a step after the stop");
            }
            let transport = device.transport.tick(&[0.0, live.position], 6);
            assert_eq!((transport[1], transport[2]), (0.0, 0.0), "stopped at {stop}: the transport says stopped");
            assert!(before >= 20, "{before}");
            // Moved back to the downbeat while stopped (plugsync~ says so at its next poll), then started: its step on
            // time, the next at a sixteenth, each with its index.
            live.position = 0.0;
            for _ in 0..(4 * VECTOR) {
                device.tick(&mut live);
            }
            live.playing = true;
            let mut again = Vec::new();
            for _ in 0..6_000 {
                let sample = live.sample;
                let (fire, index, _, position) = device.tick(&mut live);
                if fire {
                    again.push(Step { sample, index: index as usize, position });
                }
            }
            assert_eq!(again.iter().map(|step| step.index).collect::<Vec<_>>(), [0, 1], "stopped at {stop}: {again:?}");
            assert!(off_grid(&again[0], 0.25) <= 2.5 && (0.0..1.0).contains(&off_grid(&again[1], 0.25)), "{again:?}");
        }
    }

    #[test]
    fn a_swing_raised_after_the_off_beat_doesnt_play_it_again() {
        let mut live = Live::new(1);
        let mut device = Device::new(false, 0.25, 16.0);
        live.playing = true;
        let raise = (0.26 / 2.0 * RATE) as usize / VECTOR * VECTOR;
        let steps = run(&mut device, &mut live, (RATE * 1.0) as usize, |device, _, sample| {
            if sample == raise {
                device.clock.set("swing", 0.5);
            }
        });
        let first_pair: Vec<f64> = steps.iter().filter(|step| step.position < 0.49).map(|step| step.position).collect();
        assert_eq!(first_pair.len(), 2, "the downbeat and the off-beat, once each: {steps:?}");
        assert!(steps.windows(2).all(|pair| pair[1].index == pair[0].index + 1), "{steps:?}");
        // The next pairs swing: their second steps three quarters of the way in.
        let later: Vec<f64> = steps.iter().filter(|step| step.position > 0.49).map(|step| (step.position * 8.0).round() / 8.0).collect();
        assert_eq!(&later[..4], &[0.5, 0.875, 1.0, 1.375], "{steps:?}");
    }

    #[test]
    fn a_division_change_inside_a_pair_waits_for_the_new_grids_line() {
        // From quarter notes to eighth-note triplets at 0.4 beats: nothing at once, the next step at 2/3.
        let mut live = Live::new(1);
        let mut device = Device::new(false, 1.0, 16.0);
        live.playing = true;
        let change = (0.4 / 2.0 * RATE) as usize / VECTOR * VECTOR;
        let steps = run(&mut device, &mut live, (RATE * 0.6) as usize, |device, _, sample| {
            if sample == change {
                device.clock.set("division", 1.0 / 3.0);
            }
        });
        let positions: Vec<f64> = steps.iter().map(|step| step.position).collect();
        assert_eq!(positions.len(), 3, "the downbeat, 2/3 and 1: {positions:?}");
        assert!(positions[0] < 0.001 && (positions[1] - 2.0 / 3.0).abs() < 0.0002 && (positions[2] - 1.0).abs() < 0.0002, "{positions:?}");
    }

    #[test]
    fn no_name_the_building_blocks_use_is_one_gen_keeps() {
        let names = |code: &str| crate::devices::gen::names(code).declared.into_keys().chain(crate::devices::gen::names(code).read);
        for code in [TRANSPORT, CLOCK] {
            for name in names(code) {
                let allowed = name.starts_with("in")
                    || name.starts_with("out")
                    || ["samplerate", "vectorsize", "if", "else"].contains(&name.as_str());
                assert!(
                    allowed || crate::devices::gen::param_problem(&name).is_none() || name.starts_with("kumi_"),
                    "{name} is one of gen's own names"
                );
            }
        }
    }
}
