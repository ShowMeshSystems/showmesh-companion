const SYNC_WORD: u16 = 0b0011_1111_1111_1101;
const MASK80: u128 = (1u128 << 80) - 1;
const BOOTSTRAP_EDGES: u32 = 180;
const SHORT_MIN_RATIO: f32 = 0.5;
const SHORT_MAX_RATIO: f32 = 1.5;
const LONG_MAX_RATIO: f32 = 2.6;
const LONG_MIN_RATIO: f32 = 1.5;
const HALF_BITS_PER_FRAME: f32 = 160.0;
const FASTEST_FRAME_RATE: f32 = 30.0;
const SLOWEST_FRAME_RATE: f32 = 24.0;
const SPEED_TOLERANCE: f32 = 0.2;
const REACQUIRE_AFTER_FRAMES: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    pub drop_frame: bool,
    pub sample_index: u64,
}

pub struct Decoder {
    sample_rate: u32,
    sample_counter: u64,
    dc_avg: f32,
    amp_avg: f32,
    level_state: Option<bool>,
    last_edge_index: Option<u64>,
    nominal_half: Option<f32>,
    bootstrap_min: f32,
    bootstrap_count: u32,
    pending_half: Option<f32>,
    bits: u128,
    bit_count: u64,
    last_lock_index: u64,
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            sample_rate: 48000,
            sample_counter: 0,
            dc_avg: 0.0,
            amp_avg: 0.01,
            level_state: None,
            last_edge_index: None,
            nominal_half: None,
            bootstrap_min: f32::MAX,
            bootstrap_count: 0,
            pending_half: None,
            bits: 0,
            bit_count: 0,
            last_lock_index: 0,
        }
    }

    // The half-bit lengths, in samples, that LTC at a supported frame rate can have.
    fn half_bit_bounds(&self) -> (f32, f32) {
        let per_second = self.sample_rate as f32 / HALF_BITS_PER_FRAME;
        (
            per_second / FASTEST_FRAME_RATE * (1.0 - SPEED_TOLERANCE),
            per_second / SLOWEST_FRAME_RATE * (1.0 + SPEED_TOLERANCE),
        )
    }

    fn restart_bootstrap(&mut self) {
        self.nominal_half = None;
        self.bootstrap_min = f32::MAX;
        self.bootstrap_count = 0;
        self.reset_frame_sync();
    }

    fn reset_frame_sync(&mut self) {
        self.bits = 0;
        self.bit_count = 0;
        self.pending_half = None;
    }

    fn on_interval(&mut self, d: f32, index: u64, out: &mut Vec<Frame>) {
        if d <= 0.0 {
            return;
        }
        let (shortest, longest) = self.half_bit_bounds();
        // An estimate that has produced no frame for this long is wrong: measure again.
        let reacquire_after = self.sample_rate as f32 / SLOWEST_FRAME_RATE * REACQUIRE_AFTER_FRAMES;
        if self.nominal_half.is_some()
            && index.saturating_sub(self.last_lock_index) as f32 > reacquire_after
        {
            self.restart_bootstrap();
        }
        let nominal = match self.nominal_half {
            None => {
                if d >= shortest {
                    self.bootstrap_min = self.bootstrap_min.min(d);
                }
                self.bootstrap_count += 1;
                if self.bootstrap_count >= BOOTSTRAP_EDGES {
                    let estimate = self.bootstrap_min * 1.05;
                    self.restart_bootstrap();
                    if estimate <= longest {
                        self.nominal_half = Some(estimate);
                        self.last_lock_index = index;
                    }
                }
                return;
            }
            Some(n) => n,
        };
        let ratio = d / nominal;
        if (SHORT_MIN_RATIO..SHORT_MAX_RATIO).contains(&ratio) {
            match self.pending_half {
                None => {
                    self.pending_half = Some(d);
                }
                Some(_) => {
                    self.pending_half = None;
                    self.push_bit(1, index, out);
                    let new_nominal = nominal * 0.95 + d * 0.05;
                    self.nominal_half = Some(new_nominal.clamp(shortest, longest));
                }
            }
        } else if (LONG_MIN_RATIO..=LONG_MAX_RATIO).contains(&ratio) {
            if self.pending_half.is_some() {
                self.reset_frame_sync();
                return;
            }
            self.push_bit(0, index, out);
            let new_nominal = nominal * 0.95 + (d * 0.5) * 0.05;
            self.nominal_half = Some(new_nominal.clamp(shortest, longest));
        } else {
            self.reset_frame_sync();
        }
    }

    fn push_bit(&mut self, bit: u32, index: u64, out: &mut Vec<Frame>) {
        self.bits = ((self.bits << 1) | (bit as u128)) & MASK80;
        self.bit_count = self.bit_count.saturating_add(1);
        if self.bit_count < 80 {
            return;
        }
        if (self.bits & 0xFFFF) as u16 != SYNC_WORD {
            return;
        }
        if let Some(frame) = decode_payload(self.bits, index) {
            self.last_lock_index = index;
            out.push(frame);
        }
    }

    pub fn push(&mut self, samples: &[f32], sample_rate: u32) -> Vec<Frame> {
        let mut out = Vec::new();
        self.push_into(samples, sample_rate, &mut out);
        out
    }

    pub fn push_into(&mut self, samples: &[f32], sample_rate: u32, out: &mut Vec<Frame>) {
        self.sample_rate = sample_rate;
        for &raw in samples {
            let index = self.sample_counter;
            let centered = raw - self.dc_avg;
            self.dc_avg += (raw - self.dc_avg) * 0.0005;
            self.amp_avg += (centered.abs() - self.amp_avg) * 0.01;
            if self.amp_avg < 1e-6 {
                self.amp_avg = 1e-6;
            }
            let hyst = self.amp_avg * 0.25;
            match self.level_state {
                None => {
                    self.level_state = Some(centered >= 0.0);
                }
                Some(true) => {
                    if centered < -hyst {
                        self.level_state = Some(false);
                        self.on_edge(index, out);
                    }
                }
                Some(false) => {
                    if centered > hyst {
                        self.level_state = Some(true);
                        self.on_edge(index, out);
                    }
                }
            }
            self.sample_counter = self.sample_counter.saturating_add(1);
        }
    }

    fn on_edge(&mut self, index: u64, out: &mut Vec<Frame>) {
        match self.last_edge_index {
            None => {
                self.last_edge_index = Some(index);
            }
            Some(prev) => {
                let d = (index.saturating_sub(prev)) as f32;
                self.last_edge_index = Some(index);
                self.on_interval(d, index, out);
            }
        }
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

fn bit_at(bits: u128, i: u32) -> u32 {
    ((bits >> (79 - i)) & 1) as u32
}

fn decode_payload(bits: u128, sample_index: u64) -> Option<Frame> {
    let frame_units =
        bit_at(bits, 0) + bit_at(bits, 1) * 2 + bit_at(bits, 2) * 4 + bit_at(bits, 3) * 8;
    let frame_tens = bit_at(bits, 8) + bit_at(bits, 9) * 2;
    let drop_frame = bit_at(bits, 10) != 0;
    let sec_units =
        bit_at(bits, 16) + bit_at(bits, 17) * 2 + bit_at(bits, 18) * 4 + bit_at(bits, 19) * 8;
    let sec_tens = bit_at(bits, 24) + bit_at(bits, 25) * 2 + bit_at(bits, 26) * 4;
    let min_units =
        bit_at(bits, 32) + bit_at(bits, 33) * 2 + bit_at(bits, 34) * 4 + bit_at(bits, 35) * 8;
    let min_tens = bit_at(bits, 40) + bit_at(bits, 41) * 2 + bit_at(bits, 42) * 4;
    let hour_units =
        bit_at(bits, 48) + bit_at(bits, 49) * 2 + bit_at(bits, 50) * 4 + bit_at(bits, 51) * 8;
    let hour_tens = bit_at(bits, 56) + bit_at(bits, 57) * 2;

    if frame_units > 9 || frame_tens > 3 {
        return None;
    }
    let frames = frame_tens * 10 + frame_units;
    if frames > 29 {
        return None;
    }
    if sec_units > 9 || sec_tens > 5 {
        return None;
    }
    let seconds = sec_tens * 10 + sec_units;
    if seconds > 59 {
        return None;
    }
    if min_units > 9 || min_tens > 5 {
        return None;
    }
    let minutes = min_tens * 10 + min_units;
    if minutes > 59 {
        return None;
    }
    if hour_units > 9 || hour_tens > 2 {
        return None;
    }
    let hours = hour_tens * 10 + hour_units;
    if hours > 23 {
        return None;
    }

    Some(Frame {
        hours: hours as u8,
        minutes: minutes as u8,
        seconds: seconds as u8,
        frames: frames as u8,
        drop_frame,
        sample_index,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameRate {
    Fps24,
    Fps25,
    Fps2997Drop,
    Fps2997NonDrop,
    Fps30,
    Unknown,
}

const RATE_WINDOW: usize = 96;
const RATE_MIN_INTERVALS: usize = 60;

pub struct RateDetector {
    indices: [u64; RATE_WINDOW],
    drops: [bool; RATE_WINDOW],
    len: usize,
    head: usize,
}

impl RateDetector {
    pub fn new() -> Self {
        Self {
            indices: [0; RATE_WINDOW],
            drops: [false; RATE_WINDOW],
            len: 0,
            head: 0,
        }
    }

    pub fn reset(&mut self) {
        self.len = 0;
        self.head = 0;
    }

    fn newest(&self) -> Option<u64> {
        if self.len == 0 {
            return None;
        }
        Some(self.indices[(self.head + RATE_WINDOW - 1) % RATE_WINDOW])
    }

    fn at(&self, offset: usize) -> (u64, bool) {
        let start = (self.head + RATE_WINDOW - self.len) % RATE_WINDOW;
        let slot = (start + offset) % RATE_WINDOW;
        (self.indices[slot], self.drops[slot])
    }

    pub fn push(&mut self, sample_index: u64, drop_frame: bool) {
        if let Some(previous) = self.newest() {
            let gap = sample_index.saturating_sub(previous);
            if self.len >= 2 {
                let (first, _) = self.at(0);
                let average = previous.saturating_sub(first) / (self.len as u64 - 1);
                if average > 0 && gap > average * 3 {
                    self.reset();
                }
            }
        }
        self.indices[self.head] = sample_index;
        self.drops[self.head] = drop_frame;
        self.head = (self.head + 1) % RATE_WINDOW;
        if self.len < RATE_WINDOW {
            self.len += 1;
        }
    }

    pub fn detect(&self, sample_rate: u32) -> FrameRate {
        if self.len <= RATE_MIN_INTERVALS {
            return FrameRate::Unknown;
        }
        let (first, _) = self.at(0);
        let (last, _) = self.at(self.len - 1);
        let intervals = (self.len - 1) as f64;
        let avg = last.saturating_sub(first) as f64 / intervals;
        let sr = sample_rate as f64;

        let candidates = [
            (FrameRate::Fps24, sr / 24.0),
            (FrameRate::Fps25, sr / 25.0),
            (FrameRate::Fps2997NonDrop, sr * 1001.0 / 30000.0),
            (FrameRate::Fps30, sr / 30.0),
        ];

        let mut best: Option<(FrameRate, f64)> = None;
        for &(rate, candidate) in &candidates {
            let rel = (avg - candidate).abs() / candidate;
            if best.map(|(_, best_rel)| rel < best_rel).unwrap_or(true) {
                best = Some((rate, rel));
            }
        }

        match best {
            Some((rate, rel)) if rel <= 0.005 => {
                if rate == FrameRate::Fps2997NonDrop {
                    let drops = (0..self.len).filter(|&i| self.at(i).1).count();
                    if drops * 2 > self.len {
                        FrameRate::Fps2997Drop
                    } else {
                        FrameRate::Fps2997NonDrop
                    }
                } else {
                    rate
                }
            }
            _ => FrameRate::Unknown,
        }
    }
}

impl Default for RateDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn feed_rate(
        detector: &mut RateDetector,
        start: u64,
        spf: u64,
        count: usize,
        drop: bool,
    ) -> u64 {
        let mut index = start;
        for _ in 0..count {
            detector.push(index, drop);
            index += spf;
        }
        index
    }

    #[test]
    fn rate_detection_recovers_after_a_timecode_dropout() {
        let mut detector = RateDetector::new();
        let next = feed_rate(&mut detector, 0, 1600, 70, false);
        assert_eq!(detector.detect(48000), FrameRate::Fps30);

        let after_gap = next + 48000 * 30;
        feed_rate(&mut detector, after_gap, 1920, 70, false);
        assert_eq!(detector.detect(48000), FrameRate::Fps25);
    }

    #[test]
    fn a_dropout_gap_is_not_averaged_into_the_frame_interval() {
        let mut detector = RateDetector::new();
        let next = feed_rate(&mut detector, 0, 1600, 70, false);
        let after_gap = next + 48000 * 600;
        feed_rate(&mut detector, after_gap, 1600, 70, false);
        assert_eq!(detector.detect(48000), FrameRate::Fps30);
    }

    #[test]
    fn the_window_forgets_old_frames_so_a_rate_change_takes_effect() {
        let mut detector = RateDetector::new();
        let mut index = 0u64;
        for _ in 0..200 {
            detector.push(index, false);
            index += 1600;
        }
        assert_eq!(detector.detect(48000), FrameRate::Fps30);
        for _ in 0..200 {
            detector.push(index, false);
            index += 1920;
        }
        assert_eq!(detector.detect(48000), FrameRate::Fps25);
    }

    #[test]
    fn push_into_reuses_the_caller_buffer_and_matches_push() {
        let samples = generate_ltc(
            (1, 2, 3, 4),
            false,
            6,
            25.0,
            25,
            48000,
            1.0,
            0.5,
            0.0,
            false,
            0.0,
            1,
        );
        let mut a = Decoder::new();
        let expected = a.push(&samples, 48000);

        let mut b = Decoder::new();
        let mut out = Vec::new();
        b.push_into(&samples, 48000, &mut out);
        assert_eq!(out, expected);
        assert!(!out.is_empty());
    }

    fn sync_bits() -> [u8; 16] {
        [0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1]
    }

    fn push_nibble(bits: &mut [u8; 80], start: usize, value: u8, width: usize) {
        for i in 0..width {
            bits[start + i] = (value >> i) & 1;
        }
    }

    fn encode_frame_bits(hours: u8, minutes: u8, seconds: u8, frames: u8, drop: bool) -> [u8; 80] {
        let mut bits = [0u8; 80];
        push_nibble(&mut bits, 0, frames % 10, 4);
        push_nibble(&mut bits, 8, frames / 10, 2);
        bits[10] = drop as u8;
        push_nibble(&mut bits, 16, seconds % 10, 4);
        push_nibble(&mut bits, 24, seconds / 10, 3);
        push_nibble(&mut bits, 32, minutes % 10, 4);
        push_nibble(&mut bits, 40, minutes / 10, 3);
        push_nibble(&mut bits, 48, hours % 10, 4);
        push_nibble(&mut bits, 56, hours / 10, 2);
        let sync = sync_bits();
        bits[64..80].copy_from_slice(&sync);
        bits
    }

    fn increment_tc(h: &mut u8, m: &mut u8, s: &mut u8, f: &mut u8, fps: u32, drop: bool) {
        *f += 1;
        if *f >= fps as u8 {
            *f = 0;
            *s += 1;
            if *s >= 60 {
                *s = 0;
                *m += 1;
                if *m >= 60 {
                    *m = 0;
                    *h += 1;
                    if *h >= 24 {
                        *h = 0;
                    }
                }
                if drop && *s == 0 && !(*m).is_multiple_of(10) {
                    *f = 2;
                }
            }
        }
    }

    struct Lcg(u32);
    impl Lcg {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
            ((self.0 >> 8) as f32 / 16777216.0) * 2.0 - 1.0
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn generate_ltc(
        start: (u8, u8, u8, u8),
        drop: bool,
        num_frames: u32,
        fps: f64,
        fps_int: u32,
        sample_rate: u32,
        varispeed: f64,
        amplitude: f32,
        dc_offset: f32,
        invert: bool,
        noise_amp: f32,
        seed: u32,
    ) -> Vec<f32> {
        let mut all_bits: Vec<u8> = Vec::with_capacity(num_frames as usize * 80);
        let (mut h, mut m, mut s, mut f) = start;
        for i in 0..num_frames {
            if i > 0 {
                increment_tc(&mut h, &mut m, &mut s, &mut f, fps_int, drop);
            }
            let bits = encode_frame_bits(h, m, s, f, drop);
            all_bits.extend_from_slice(&bits);
        }

        let samples_per_bit = (sample_rate as f64 / (fps * 80.0)) / varispeed;
        let mut pos = 0.0f64;
        let mut transitions: Vec<f64> = Vec::new();
        let mut first = true;
        for &b in &all_bits {
            let bit_start = pos;
            let bit_end = pos + samples_per_bit;
            if !first {
                transitions.push(bit_start);
            }
            first = false;
            if b == 1 {
                transitions.push(bit_start + samples_per_bit / 2.0);
            }
            pos = bit_end;
        }
        let total_samples = pos.ceil() as usize;
        let mut samples = vec![0f32; total_samples];
        let mut ti = 0usize;
        let mut level = true;
        let mut rng = Lcg(seed | 1);
        for (i, sample) in samples.iter_mut().enumerate().take(total_samples) {
            let t = i as f64;
            while ti < transitions.len() && transitions[ti] <= t {
                level = !level;
                ti += 1;
            }
            let mut ac = if level { amplitude } else { -amplitude };
            if invert {
                ac = -ac;
            }
            let noise = if noise_amp > 0.0 {
                rng.next() * noise_amp
            } else {
                0.0
            };
            *sample = ac + dc_offset + noise;
        }
        samples
    }

    fn fps_pair(fps_name: &str) -> (f64, u32) {
        match fps_name {
            "24" => (24.0, 24),
            "25" => (25.0, 25),
            "2997" => (30000.0 / 1001.0, 30),
            "30" => (30.0, 30),
            _ => unreachable!(),
        }
    }

    #[test]
    fn round_trip_all_rates_both_sample_rates() {
        for &fps_name in &["24", "25", "2997", "30"] {
            for &drop in &[false, true] {
                if fps_name != "2997" && drop {
                    continue;
                }
                for &sr in &[44100u32, 48000u32] {
                    let (fps, fps_int) = fps_pair(fps_name);
                    let samples = generate_ltc(
                        (1, 2, 3, 0),
                        drop,
                        10,
                        fps,
                        fps_int,
                        sr,
                        1.0,
                        0.8,
                        0.0,
                        false,
                        0.0,
                        1,
                    );
                    let mut dec = Decoder::new();
                    let frames = dec.push(&samples, sr);
                    assert!(
                        !frames.is_empty(),
                        "no frames decoded for fps={fps_name} drop={drop} sr={sr}"
                    );
                    let last = frames.last().unwrap();
                    assert!(last.hours == 1 || last.hours == 0);
                }
            }
        }
    }

    fn expected_final(
        start: (u8, u8, u8, u8),
        num_frames: u32,
        fps_int: u32,
        drop: bool,
    ) -> (u8, u8, u8, u8) {
        let (mut h, mut m, mut s, mut f) = start;
        for i in 0..num_frames {
            if i > 0 {
                increment_tc(&mut h, &mut m, &mut s, &mut f, fps_int, drop);
            }
        }
        (h, m, s, f)
    }

    #[test]
    fn decodes_exact_values() {
        let start = (10, 20, 30, 5);
        let num_frames = 10;
        let samples = generate_ltc(
            start,
            false,
            num_frames + 1,
            25.0,
            25,
            48000,
            1.0,
            0.8,
            0.0,
            false,
            0.0,
            2,
        );
        let mut dec = Decoder::new();
        let frames = dec.push(&samples, 48000);
        assert!(!frames.is_empty());
        let expected = expected_final(start, num_frames, 25, false);
        let last = frames.last().unwrap();
        assert_eq!(last.hours, expected.0);
        assert_eq!(last.minutes, expected.1);
        assert_eq!(last.seconds, expected.2);
        assert_eq!(last.frames, expected.3);
        assert!(!last.drop_frame);
    }

    #[test]
    fn inverted_polarity_matches_normal() {
        let normal = generate_ltc(
            (5, 5, 5, 5),
            false,
            10,
            25.0,
            25,
            48000,
            1.0,
            0.8,
            0.0,
            false,
            0.0,
            3,
        );
        let inverted = generate_ltc(
            (5, 5, 5, 5),
            false,
            10,
            25.0,
            25,
            48000,
            1.0,
            0.8,
            0.0,
            true,
            0.0,
            3,
        );
        let mut d1 = Decoder::new();
        let mut d2 = Decoder::new();
        let f1 = d1.push(&normal, 48000);
        let f2 = d2.push(&inverted, 48000);
        assert!(!f1.is_empty());
        assert_eq!(f1.len(), f2.len());
        for (a, b) in f1.iter().zip(f2.iter()) {
            assert_eq!(a.hours, b.hours);
            assert_eq!(a.minutes, b.minutes);
            assert_eq!(a.seconds, b.seconds);
            assert_eq!(a.frames, b.frames);
        }
    }

    #[test]
    fn dc_offset_does_not_break_decode() {
        for &offset in &[0.3f32, -0.3, 0.15] {
            let samples = generate_ltc(
                (0, 0, 0, 0),
                false,
                10,
                25.0,
                25,
                48000,
                1.0,
                0.5,
                offset,
                false,
                0.0,
                4,
            );
            let mut dec = Decoder::new();
            let frames = dec.push(&samples, 48000);
            assert!(!frames.is_empty(), "failed with dc offset {offset}");
        }
    }

    #[test]
    fn tolerates_additive_noise() {
        let mut worked_noise = 0.0f32;
        for &noise in &[0.02f32, 0.05, 0.1, 0.15] {
            let samples = generate_ltc(
                (0, 0, 0, 0),
                false,
                12,
                25.0,
                25,
                48000,
                1.0,
                0.8,
                0.0,
                false,
                noise,
                5,
            );
            let mut dec = Decoder::new();
            let frames = dec.push(&samples, 48000);
            if !frames.is_empty() {
                worked_noise = noise;
            }
        }
        assert!(
            worked_noise > 0.0,
            "decoder never survived any noise level tested"
        );
    }

    #[test]
    fn amplitude_range() {
        let mut min_working = f32::MAX;
        for &amp in &[0.95f32, 0.5, 0.1, 0.02, 0.01] {
            let samples = generate_ltc(
                (0, 0, 0, 0),
                false,
                10,
                25.0,
                25,
                48000,
                1.0,
                amp,
                0.0,
                false,
                0.0,
                6,
            );
            let mut dec = Decoder::new();
            let frames = dec.push(&samples, 48000);
            if !frames.is_empty() && amp < min_working {
                min_working = amp;
            }
        }
        assert!(
            min_working < f32::MAX,
            "decoder never worked at any tested amplitude"
        );
    }

    #[test]
    fn varispeed_tolerance() {
        for &vs in &[0.97f64, 0.98, 1.0, 1.02, 1.03] {
            let samples = generate_ltc(
                (0, 0, 0, 0),
                false,
                10,
                25.0,
                25,
                48000,
                vs,
                0.8,
                0.0,
                false,
                0.0,
                7,
            );
            let mut dec = Decoder::new();
            let frames = dec.push(&samples, 48000);
            assert!(!frames.is_empty(), "failed at varispeed {vs}");
        }
    }

    #[test]
    fn split_buffers_match_unsplit_at_every_offset() {
        let samples = generate_ltc(
            (3, 4, 5, 0),
            false,
            10,
            25.0,
            25,
            48000,
            1.0,
            0.8,
            0.0,
            false,
            0.0,
            8,
        );
        let mut whole = Decoder::new();
        let reference = whole.push(&samples, 48000);
        assert!(!reference.is_empty());

        for split in 0..samples.len().min(400) {
            let (a, b) = samples.split_at(split);
            let mut dec = Decoder::new();
            let mut out = dec.push(a, 48000);
            out.extend(dec.push(b, 48000));
            assert_eq!(out, reference, "mismatch at split offset {split}");
        }
    }

    #[test]
    fn split_at_many_offsets_across_frame() {
        let samples = generate_ltc(
            (0, 1, 0, 0),
            false,
            14,
            25.0,
            25,
            48000,
            1.0,
            0.8,
            0.0,
            false,
            0.0,
            9,
        );
        let mut whole = Decoder::new();
        let reference = whole.push(&samples, 48000);
        assert!(!reference.is_empty());

        let step = (samples.len() / 200).max(1);
        let mut offset = 0;
        while offset < samples.len() {
            let (a, b) = samples.split_at(offset);
            let mut dec = Decoder::new();
            let mut out = dec.push(a, 48000);
            out.extend(dec.push(b, 48000));
            assert_eq!(out, reference, "mismatch at split offset {offset}");
            offset += step;
        }
    }

    #[test]
    fn corrupted_sync_word_yields_no_frame() {
        let mut samples = generate_ltc(
            (0, 0, 0, 0),
            false,
            3,
            25.0,
            25,
            48000,
            1.0,
            0.8,
            0.0,
            false,
            0.0,
            10,
        );
        let n = samples.len();
        for sample in samples.iter_mut().skip(n / 3).take(n / 6) {
            *sample = 0.0;
        }
        let mut dec = Decoder::new();
        let frames = dec.push(&samples, 48000);
        assert!(frames.is_empty());
    }

    #[test]
    fn truncated_frame_yields_no_frame() {
        let samples = generate_ltc(
            (0, 0, 0, 0),
            false,
            2,
            25.0,
            25,
            48000,
            1.0,
            0.8,
            0.0,
            false,
            0.0,
            11,
        );
        let cut = samples.len() * 3 / 4;
        let truncated = &samples[..cut];
        let mut dec = Decoder::new();
        let frames = dec.push(truncated, 48000);
        assert!(frames.is_empty());
    }

    #[test]
    fn out_of_range_bcd_rejected() {
        let mut bits = encode_frame_bits(0, 0, 0, 0, false);
        bits[24] = 1;
        bits[25] = 1;
        bits[26] = 1;
        let frame = decode_full_frame_from_bits(bits);
        assert!(frame.is_none());
    }

    fn decode_full_frame_from_bits(bits: [u8; 80]) -> Option<Frame> {
        let mut acc: u128 = 0;
        for &b in &bits {
            acc = (acc << 1) | (b as u128);
        }
        decode_payload(acc, 0)
    }

    #[test]
    fn reverse_play_does_not_emit_garbage() {
        let samples = generate_ltc(
            (1, 1, 1, 0),
            false,
            5,
            25.0,
            25,
            48000,
            1.0,
            0.8,
            0.0,
            false,
            0.0,
            12,
        );
        let mut reversed = samples.clone();
        reversed.reverse();
        let mut dec = Decoder::new();
        let frames = dec.push(&reversed, 48000);
        for f in &frames {
            assert!(f.hours < 24 && f.minutes < 60 && f.seconds < 60 && f.frames < 30);
        }
    }

    #[test]
    fn rolls_across_second_minute_and_midnight() {
        let cases = [
            (0, 0, 58, 0),
            (0, 0, 59, 0),
            (0, 59, 59, 0),
            (23, 59, 59, 0),
        ];
        for &(h, m, s, f) in &cases {
            let samples = generate_ltc(
                (h, m, s, f),
                false,
                10,
                25.0,
                25,
                48000,
                1.0,
                0.8,
                0.0,
                false,
                0.0,
                13,
            );
            let mut dec = Decoder::new();
            let frames = dec.push(&samples, 48000);
            assert!(!frames.is_empty(), "no frames for start {h}:{m}:{s}:{f}");
        }
    }

    fn timecodes(
        start: (u8, u8, u8, u8),
        count: u32,
        fps_int: u32,
        drop: bool,
    ) -> Vec<(u8, u8, u8, u8)> {
        let (mut h, mut m, mut s, mut f) = start;
        let mut all = Vec::with_capacity(count as usize);
        for i in 0..count {
            if i > 0 {
                increment_tc(&mut h, &mut m, &mut s, &mut f, fps_int, drop);
            }
            all.push((h, m, s, f));
        }
        all
    }

    fn noise(seconds: f64, sample_rate: u32, amplitude: f32, seed: u32) -> Vec<f32> {
        let mut rng = Lcg(seed | 1);
        let len = (seconds * sample_rate as f64) as usize;
        (0..len).map(|_| rng.next() * amplitude).collect()
    }

    // The stream's final frame has no closing edge unless something follows it,
    // so a decoded run may end on the last frame or the one before it.
    fn assert_exact_run(
        decoded: &[Frame],
        expected: &[(u8, u8, u8, u8)],
        drop: bool,
        min_frames: usize,
        label: &str,
    ) {
        assert!(
            decoded.len() >= min_frames && decoded.len() <= expected.len(),
            "{label}: decoded {} frames, expected at least {min_frames} of {}",
            decoded.len(),
            expected.len()
        );
        let got: Vec<_> = decoded
            .iter()
            .map(|f| (f.hours, f.minutes, f.seconds, f.frames))
            .collect();
        let last = expected.len() - 1;
        if got.last() == Some(&expected[last]) {
            assert_eq!(got, expected[last + 1 - got.len()..], "{label}");
        } else {
            assert_eq!(got, expected[last - got.len()..last], "{label}");
        }
        assert!(decoded.iter().all(|f| f.drop_frame == drop), "{label}");
    }

    const NOISE_FLOOR: f32 = 1e-4;

    fn every_rate(mut case: impl FnMut(&str, f64, u32, bool, u32)) {
        for &fps_name in &["24", "25", "2997", "30"] {
            for &drop in &[false, true] {
                if fps_name != "2997" && drop {
                    continue;
                }
                let (fps, fps_int) = fps_pair(fps_name);
                for &sr in &[44100u32, 48000, 96000] {
                    let label = format!("fps={fps_name} drop={drop} sr={sr}");
                    case(&label, fps, fps_int, drop, sr);
                }
            }
        }
    }

    #[test]
    fn locks_onto_ltc_that_starts_after_input_noise() {
        every_rate(|label, fps, fps_int, drop, sr| {
            for &(noise_seconds, floor) in &[(0.05, NOISE_FLOOR), (1.0, NOISE_FLOOR), (4.0, 0.01)] {
                let start = (0, 0, 59, fps_int as u8 - 6);
                let mut samples = noise(noise_seconds, sr, floor, 21);
                samples.extend(generate_ltc(
                    start, drop, 40, fps, fps_int, sr, 1.0, 0.5, 0.0, false, floor, 22,
                ));
                let frames = Decoder::new().push(&samples, sr);
                let expected = timecodes(start, 40, fps_int, drop);
                let label = format!("{label} noise={noise_seconds}s floor={floor}");
                assert_exact_run(&frames, &expected, drop, 28, &label);
            }
        });
    }

    #[test]
    fn locks_again_when_ltc_returns_after_a_noisy_gap() {
        every_rate(|label, fps, fps_int, drop, sr| {
            for &gap_seconds in &[0.05, 2.0] {
                let first_start = (9, 59, 59, fps_int as u8 - 6);
                let second_start = (21, 9, 59, fps_int as u8 - 6);
                let mut samples = generate_ltc(
                    first_start,
                    drop,
                    30,
                    fps,
                    fps_int,
                    sr,
                    1.0,
                    0.5,
                    0.0,
                    false,
                    NOISE_FLOOR,
                    23,
                );
                samples.extend(noise(gap_seconds, sr, NOISE_FLOOR, 24));
                let second_begins = samples.len() as u64;
                samples.extend(generate_ltc(
                    second_start,
                    drop,
                    40,
                    fps,
                    fps_int,
                    sr,
                    1.0,
                    0.5,
                    0.0,
                    false,
                    NOISE_FLOOR,
                    25,
                ));
                let frames = Decoder::new().push(&samples, sr);
                let (before, after): (Vec<Frame>, Vec<Frame>) = frames
                    .iter()
                    .partition(|frame| frame.sample_index < second_begins);
                let label = format!("{label} gap={gap_seconds}s");
                let first = timecodes(first_start, 30, fps_int, drop);
                assert_exact_run(&before, &first, drop, 24, &format!("{label} before gap"));
                let second = timecodes(second_start, 40, fps_int, drop);
                assert_exact_run(&after, &second, drop, 28, &format!("{label} after gap"));
            }
        });
    }

    #[test]
    fn decodes_low_level_ltc_over_a_noise_floor() {
        every_rate(|label, fps, fps_int, drop, sr| {
            let start = (23, 59, 59, fps_int as u8 - 6);
            let mut samples = noise(1.0, sr, NOISE_FLOOR, 26);
            samples.extend(generate_ltc(
                start,
                drop,
                40,
                fps,
                fps_int,
                sr,
                1.0,
                0.002,
                0.0,
                false,
                NOISE_FLOOR,
                27,
            ));
            let frames = Decoder::new().push(&samples, sr);
            let expected = timecodes(start, 40, fps_int, drop);
            assert_exact_run(&frames, &expected, drop, 28, label);
        });
    }

    #[test]
    fn input_noise_alone_never_decodes_a_frame() {
        for &sr in &[44100u32, 48000, 96000] {
            let samples = noise(20.0, sr, NOISE_FLOOR, 28);
            let frames = Decoder::new().push(&samples, sr);
            assert!(frames.is_empty(), "sr={sr}: {frames:?}");
        }
    }

    #[test]
    fn rate_detection_unknown_before_window_fills() {
        let mut rd = RateDetector::new();
        for i in 0..50u64 {
            rd.push(i * 1600, false);
        }
        assert_eq!(rd.detect(48000), FrameRate::Unknown);
    }

    #[test]
    fn rate_detection_distinguishes_2997_from_30() {
        let mut rd30 = RateDetector::new();
        let mut idx = 0u64;
        for _ in 0..70 {
            rd30.push(idx, false);
            idx += 1600;
        }
        assert_eq!(rd30.detect(48000), FrameRate::Fps30);

        let mut rd2997 = RateDetector::new();
        let mut pos = 0.0f64;
        for _ in 0..70 {
            rd2997.push(pos.round() as u64, false);
            pos += 48000.0 * 1001.0 / 30000.0;
        }
        assert_eq!(rd2997.detect(48000), FrameRate::Fps2997NonDrop);
    }

    #[test]
    fn rate_detection_24_and_25() {
        let mut rd24 = RateDetector::new();
        let mut idx = 0u64;
        for _ in 0..70 {
            rd24.push(idx, false);
            idx += 2000;
        }
        assert_eq!(rd24.detect(48000), FrameRate::Fps24);

        let mut rd25 = RateDetector::new();
        let mut idx = 0u64;
        for _ in 0..70 {
            rd25.push(idx, false);
            idx += 1920;
        }
        assert_eq!(rd25.detect(48000), FrameRate::Fps25);
    }

    #[test]
    fn rate_detection_drop_flag_selects_drop_variant() {
        let mut rd = RateDetector::new();
        let mut pos = 0.0f64;
        for _ in 0..70 {
            rd.push(pos.round() as u64, true);
            pos += 48000.0 * 1001.0 / 30000.0;
        }
        assert_eq!(rd.detect(48000), FrameRate::Fps2997Drop);
    }
}
