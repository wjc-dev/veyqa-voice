/// Stateful linear resampler matching the Python recorder's chunk-continuous
/// interpolation: the last source sample and fractional position carry across
/// chunks so boundaries do not click.
pub struct LinearResampler {
    src_rate: u32,
    dst_rate: u32,
    previous: Option<f32>,
    position: f64,
}

impl LinearResampler {
    pub fn new(src_rate: u32, dst_rate: u32) -> Self {
        Self {
            src_rate,
            dst_rate,
            previous: None,
            position: 0.0,
        }
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if (self.src_rate as f64 / self.dst_rate as f64 - 1.0).abs() <= 0.01 {
            return input.to_vec();
        }
        if input.is_empty() {
            return Vec::new();
        }

        let mut source: Vec<f32> = Vec::with_capacity(input.len() + 1);
        if let Some(previous) = self.previous {
            source.push(previous);
        }
        source.extend_from_slice(input);

        if source.len() < 2 {
            self.previous = source.last().copied();
            return Vec::new();
        }

        let step = self.src_rate as f64 / self.dst_rate as f64;
        let limit = (source.len() - 1) as f64;
        let count = if limit > self.position {
            ((limit - self.position) / step).ceil() as usize
        } else {
            0
        };
        let positions: Vec<f64> = (0..count)
            .map(|index| self.position + index as f64 * step)
            .collect();

        if positions.is_empty() {
            self.position = (self.position - limit).max(0.0);
            self.previous = source.last().copied();
            return Vec::new();
        }

        let output: Vec<f32> = positions
            .iter()
            .map(|&position| {
                let left = position.floor() as usize;
                let fraction = (position - left as f64) as f32;
                source[left] * (1.0 - fraction) + source[left + 1] * fraction
            })
            .collect();

        self.position = ((positions.last().copied().unwrap_or(0.0) + step) - limit).max(0.0);
        self.previous = source.last().copied();
        output
    }
}

/// Streaming Kaiser-windowed sinc low-pass applied before decimation.
///
/// Linear interpolation alone folds everything above the target Nyquist back
/// into the speech band; at 48 kHz that is audible noise on quiet speech and
/// measurably hurts recognition.
pub struct AntiAliasFilter {
    taps: Vec<f32>,
    history: Vec<f32>,
    write: usize,
}

impl AntiAliasFilter {
    pub fn new(src_rate: u32, dst_rate: u32) -> Self {
        const TAPS: usize = 191;
        const BETA: f64 = 8.0;
        let src = src_rate as f64;
        let cutoff = (dst_rate as f64 * 0.5 * 0.85).min(src * 0.45);
        let middle = (TAPS - 1) as f64 / 2.0;
        let normalized = 2.0 * cutoff / src;
        let mut taps = Vec::with_capacity(TAPS);
        let mut sum = 0.0;
        for index in 0..TAPS {
            let offset = index as f64 - middle;
            let argument = std::f64::consts::PI * normalized * offset;
            let sinc = if argument.abs() < 1e-9 {
                1.0
            } else {
                argument.sin() / argument
            };
            let ratio = offset / middle;
            let window = bessel_i0(BETA * (1.0 - ratio * ratio).max(0.0).sqrt())
                / bessel_i0(BETA);
            let value = normalized * sinc * window;
            taps.push(value as f32);
            sum += value;
        }
        let scale = (1.0 / sum) as f32;
        for tap in &mut taps {
            *tap *= scale;
        }
        Self {
            taps,
            history: vec![0.0; TAPS * 2],
            write: 0,
        }
    }

    pub fn process_in_place(&mut self, samples: &mut [f32]) {
        let length = self.taps.len();
        for sample in samples.iter_mut() {
            let value = *sample;
            self.history[self.write] = value;
            self.history[self.write + length] = value;
            let base = self.write + length;
            let mut sum = 0.0f32;
            for (index, tap) in self.taps.iter().enumerate() {
                sum += tap * self.history[base - index];
            }
            *sample = sum;
            self.write += 1;
            if self.write == length {
                self.write = 0;
            }
        }
    }
}

fn bessel_i0(value: f64) -> f64 {
    let half = value / 2.0;
    let mut sum = 1.0;
    let mut term = 1.0;
    for order in 1..64 {
        term *= (half / order as f64) * (half / order as f64);
        sum += term;
        if term < sum * 1e-12 {
            break;
        }
    }
    sum
}

/// Gentle software AGC for quiet speech, framed at 10 ms with a noise floor
/// estimate so silence is never amplified. Gain is applied before resampling
/// so the ASR stream and the preview level both see the boosted signal.
pub struct AutomaticGain {
    frame_len: usize,
    frame: Vec<f32>,
    noise_floor: f32,
    gain: f32,
    target: f32,
    down: f32,
    up: f32,
    max_gain: f32,
    target_rms: f32,
}

impl AutomaticGain {
    pub fn new(sample_rate: u32) -> Self {
        let rate = sample_rate.max(1) as f64;
        let coefficient = |tau_ms: f64| {
            (1.0 - (-1.0 / (tau_ms * 0.001 * rate)).exp()) as f32
        };
        Self {
            frame_len: (sample_rate as usize / 100).max(1),
            frame: Vec::new(),
            noise_floor: 0.002,
            gain: 1.0,
            target: 1.0,
            down: coefficient(8.0),
            up: coefficient(30.0),
            max_gain: 6.0,
            target_rms: 0.05,
        }
    }

    pub fn process_in_place(&mut self, samples: &mut [f32]) {
        for sample in samples.iter_mut() {
            let value = *sample;
            let coefficient = if self.target < self.gain { self.down } else { self.up };
            self.gain += (self.target - self.gain) * coefficient;
            *sample = (value * self.gain).clamp(-1.0, 1.0);

            self.frame.push(value);
            if self.frame.len() >= self.frame_len {
                self.update_target();
                self.frame.clear();
            }
        }
    }

    fn update_target(&mut self) {
        let mut sum = 0.0f64;
        let mut peak = 0.0f32;
        for sample in &self.frame {
            sum += (*sample as f64) * (*sample as f64);
            peak = peak.max(sample.abs());
        }
        let rms = (sum / self.frame.len() as f64).sqrt() as f32;
        if rms < self.noise_floor {
            self.noise_floor = self.noise_floor * 0.7 + rms * 0.3;
        } else {
            self.noise_floor += (rms - self.noise_floor) * 0.002;
        }
        let speech = rms > (self.noise_floor * 2.0).max(0.0015);
        let mut target = if speech && rms > 1e-6 {
            (self.target_rms / rms).clamp(1.0, self.max_gain)
        } else {
            1.0
        };
        if peak > 1e-6 {
            target = target.min((0.92 / peak).max(1.0));
        }
        self.target = target.clamp(1.0, self.max_gain);
    }
}

/// Buffers captured float samples into fixed-duration PCM16 chunks exactly like
/// `AudioRecorder.stream_audio_chunks`: one chunk per push at most, and the
/// remainder is flushed when recording ends.
pub struct PcmChunker {
    source_rate: u32,
    target_rate: u32,
    samples_per_chunk: usize,
    need_resample: bool,
    accumulated: Vec<f32>,
    previous_sample: Option<f32>,
    source_position: f64,
    filter: Option<AntiAliasFilter>,
    agc: Option<AutomaticGain>,
    scratch: Vec<f32>,
}

impl PcmChunker {
    pub fn new(source_rate: u32, target_rate: u32, chunk_duration_ms: u32) -> Self {
        Self {
            source_rate,
            target_rate,
            samples_per_chunk: (source_rate as f64 * chunk_duration_ms as f64 / 1000.0) as usize,
            need_resample: (target_rate as f64 / source_rate as f64 - 1.0).abs() > 0.01,
            accumulated: Vec::new(),
            previous_sample: None,
            source_position: 0.0,
            filter: None,
            agc: None,
            scratch: Vec::new(),
        }
    }

    pub fn with_processing(
        source_rate: u32,
        target_rate: u32,
        chunk_duration_ms: u32,
        anti_alias: bool,
        agc: bool,
    ) -> Self {
        let mut chunker = Self::new(source_rate, target_rate, chunk_duration_ms);
        if anti_alias && chunker.need_resample {
            chunker.filter = Some(AntiAliasFilter::new(source_rate, target_rate));
        }
        if agc {
            chunker.agc = Some(AutomaticGain::new(source_rate));
        }
        chunker
    }

    pub fn push(&mut self, samples: &[f32]) -> Option<Vec<u8>> {
        if self.filter.is_some() || self.agc.is_some() {
            self.scratch.clear();
            self.scratch.extend_from_slice(samples);
            if let Some(filter) = self.filter.as_mut() {
                filter.process_in_place(&mut self.scratch);
            }
            if let Some(agc) = self.agc.as_mut() {
                agc.process_in_place(&mut self.scratch);
            }
            self.accumulated.extend_from_slice(&self.scratch);
        } else {
            self.accumulated.extend_from_slice(samples);
        }
        if self.accumulated.len() < self.samples_per_chunk {
            return None;
        }
        let chunk: Vec<f32> = self
            .accumulated
            .drain(..self.samples_per_chunk)
            .collect();
        let bytes = self.to_pcm_bytes(&chunk);
        (!bytes.is_empty()).then_some(bytes)
    }

    pub fn finish(&mut self) -> Option<Vec<u8>> {
        if self.accumulated.is_empty() {
            return None;
        }
        let chunk = std::mem::take(&mut self.accumulated);
        let bytes = self.to_pcm_bytes(&chunk);
        (!bytes.is_empty()).then_some(bytes)
    }

    pub fn buffered_samples(&self) -> usize {
        self.accumulated.len()
    }

    fn to_pcm_bytes(&mut self, audio: &[f32]) -> Vec<u8> {
        let converted: Vec<f64> = if self.need_resample {
            let mut source: Vec<f32> = Vec::with_capacity(audio.len() + 1);
            if let Some(previous) = self.previous_sample {
                source.push(previous);
            }
            source.extend_from_slice(audio);

            let step = self.source_rate as f64 / self.target_rate as f64;
            let limit = (source.len() as f64 - 1.0).max(0.0);
            let count = if limit > self.source_position {
                ((limit - self.source_position) / step).ceil() as usize
            } else {
                0
            };
            let mut output = Vec::with_capacity(count);
            for index in 0..count {
                let position = self.source_position + index as f64 * step;
                let left = position.floor() as usize;
                let fraction = position - left as f64;
                let first = source[left] as f64;
                let second = source.get(left + 1).map_or(first, |value| *value as f64);
                output.push(first * (1.0 - fraction) + second * fraction);
            }
            if count > 0 {
                let last_position = self.source_position + (count - 1) as f64 * step;
                self.source_position =
                    last_position + step - (source.len() as f64 - 1.0);
            } else {
                self.source_position = (self.source_position - (source.len() as f64 - 1.0))
                    .max(0.0);
            }
            if let Some(last) = source.last() {
                self.previous_sample = Some(*last);
            }
            output
        } else {
            audio.iter().map(|value| *value as f64).collect()
        };

        let mut bytes = Vec::with_capacity(converted.len() * 2);
        for value in converted {
            let scaled = (value * 32767.0).clamp(-32768.0, 32767.0);
            bytes.extend_from_slice(&(scaled as i16).to_le_bytes());
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, seconds: f32, hz: f32) -> Vec<f32> {
        let len = (rate as f32 * seconds) as usize;
        (0..len)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn downsamples_48k_to_16k() {
        let input = sine(48_000, 1.0, 440.0);
        let mut resampler = LinearResampler::new(48_000, 16_000);
        let output = resampler.process(&input);
        assert!((output.len() as i64 - 16_000).abs() <= 2, "len={}", output.len());
        let peak = output.iter().fold(0.0f32, |acc, v| acc.max(v.abs()));
        assert!(peak > 0.4 && peak <= 0.51, "peak={peak}");
    }

    #[test]
    fn chunked_matches_whole_length() {
        let input = sine(48_000, 1.0, 440.0);
        let whole = LinearResampler::new(48_000, 16_000).process(&input);
        let mut chunked = LinearResampler::new(48_000, 16_000);
        let mut pieces = Vec::new();
        for chunk in input.chunks(480) {
            pieces.extend(chunked.process(chunk));
        }
        let diff = (whole.len() as i64 - pieces.len() as i64).abs();
        assert!(diff <= 8, "whole={} chunked={}", whole.len(), pieces.len());
    }

    #[test]
    fn continuous_boundary_has_no_large_jump() {
        let input = sine(44_100, 1.0, 220.0);
        let mut resampler = LinearResampler::new(44_100, 16_000);
        let mut output = Vec::new();
        for chunk in input.chunks(441) {
            output.extend(resampler.process(chunk));
        }
        let max_jump = output
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(max_jump < 0.1, "max_jump={max_jump}");
    }

    #[test]
    fn pcm_chunker_resamples_48k_in_100ms_blocks() {
        let source: Vec<f32> = (0..9600)
            .map(|index| -0.2 + 0.4 * index as f32 / 9599.0)
            .collect();
        let mut chunker = PcmChunker::new(48_000, 16_000, 100);
        let chunks = vec![
            chunker.push(&source[..4800]).unwrap(),
            chunker.push(&source[4800..]).unwrap(),
        ];
        assert!(chunker.finish().is_none());
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks.iter().map(Vec::len).sum::<usize>(), 6400);

        let samples: Vec<i16> = chunks
            .iter()
            .flat_map(|chunk| {
                chunk
                    .chunks_exact(2)
                    .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            })
            .collect();
        assert!(samples.windows(2).all(|pair| pair[1] >= pair[0]));
    }

    #[test]
    fn pcm_chunker_flushes_a_partial_tail_and_clamps() {
        let mut chunker = PcmChunker::new(16_000, 16_000, 100);
        assert!(chunker.push(&[0.5; 800]).is_none());
        assert_eq!(chunker.buffered_samples(), 800);
        let tail = chunker.finish().unwrap();
        assert_eq!(tail.len(), 1600);

        let mut chunker = PcmChunker::new(16_000, 16_000, 100);
        chunker.push(&[2.0, -2.0]);
        let bytes = chunker.finish().unwrap();
        assert_eq!(i16::from_le_bytes([bytes[0], bytes[1]]), 32767);
        assert_eq!(i16::from_le_bytes([bytes[2], bytes[3]]), -32768);
    }

    #[test]
    fn pcm_chunker_emits_at_most_one_block_per_push() {
        let mut chunker = PcmChunker::new(16_000, 16_000, 100);
        assert!(chunker.push(&[0.1; 3200]).is_some());
        assert_eq!(chunker.buffered_samples(), 1600);
        let tail = chunker.finish().unwrap();
        assert_eq!(tail.len(), 3200);
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum: f64 = samples
            .iter()
            .map(|value| (*value as f64) * (*value as f64))
            .sum();
        (sum / samples.len() as f64).sqrt() as f32
    }

    #[test]
    fn anti_alias_filter_keeps_the_passband() {
        let input = sine(48_000, 1.0, 1_000.0);
        let mut filter = AntiAliasFilter::new(48_000, 16_000);
        let mut output = input.clone();
        filter.process_in_place(&mut output);
        let before = rms(&input[2_000..]);
        let after = rms(&output[2_000..]);
        assert!((after / before - 1.0).abs() < 0.02, "ratio={}", after / before);
    }

    #[test]
    fn anti_alias_filter_rejects_energy_above_target_nyquist() {
        let input = sine(48_000, 1.0, 10_000.0);
        let mut filter = AntiAliasFilter::new(48_000, 16_000);
        let mut output = input.clone();
        filter.process_in_place(&mut output);
        let before = rms(&input[4_000..]);
        let after = rms(&output[4_000..]);
        assert!(before > 0.3, "before={before}");
        assert!(after < before * 0.01, "after={after} before={before}");
    }

    #[test]
    fn anti_alias_filter_is_chunk_continuous() {
        let input = sine(44_100, 1.0, 2_000.0);
        let mut whole = input.clone();
        AntiAliasFilter::new(44_100, 16_000).process_in_place(&mut whole);

        let mut filter = AntiAliasFilter::new(44_100, 16_000);
        let mut pieces = Vec::new();
        for chunk in input.chunks(441) {
            let mut block = chunk.to_vec();
            filter.process_in_place(&mut block);
            pieces.extend(block);
        }
        let max_diff = whole
            .iter()
            .zip(pieces.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_diff < 1e-6, "max_diff={max_diff}");
    }

    #[test]
    fn agc_boosts_quiet_speech() {
        let rate = 48_000;
        let mut input = vec![0.001; rate as usize / 2];
        input.extend(sine(rate, 1.0, 440.0).iter().map(|value| value * 0.016));
        let mut output = input.clone();
        let mut agc = AutomaticGain::new(rate);
        agc.process_in_place(&mut output);

        let tail = &output[output.len() - rate as usize / 5..];
        let source = &input[input.len() - rate as usize / 5..];
        assert!(
            rms(tail) > rms(source) * 2.5,
            "source={} output={}",
            rms(source),
            rms(tail)
        );
    }

    #[test]
    fn agc_does_not_amplify_silence() {
        let rate = 48_000;
        let input = vec![0.001; rate as usize];
        let mut output = input.clone();
        let mut agc = AutomaticGain::new(rate);
        agc.process_in_place(&mut output);
        assert!(rms(&output) < 0.002, "rms={}", rms(&output));
    }

    #[test]
    fn agc_never_clips_or_boosts_loud_input() {
        let rate = 48_000;
        let input = sine(rate, 0.5, 440.0).iter().map(|value| value * 0.8).collect::<Vec<_>>();
        let mut output = input.clone();
        let mut agc = AutomaticGain::new(rate);
        agc.process_in_place(&mut output);
        assert!(output.iter().all(|value| value.abs() <= 1.0));
        assert!(rms(&output) > rms(&input) * 0.95);
    }

    #[test]
    fn processed_chunker_keeps_100ms_cadence() {
        let input = sine(48_000, 0.2, 500.0).iter().map(|value| value * 0.05).collect::<Vec<_>>();
        let mut chunker = PcmChunker::with_processing(48_000, 16_000, 100, true, true);
        let first = chunker.push(&input[..4800]).unwrap();
        let second = chunker.push(&input[4800..]).unwrap();
        assert_eq!(first.len(), 3200);
        assert_eq!(second.len(), 3200);
        assert!(chunker.finish().is_none());
    }
}
