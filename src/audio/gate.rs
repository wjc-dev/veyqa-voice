//! RMS speech gate from `main.py::audio_chunks_with_levels`.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkMetrics {
    pub samples: usize,
    pub rms: f64,
    pub total_audio_ms: f64,
    pub voiced_audio_ms: f64,
    pub gate_open: bool,
    pub level: f64,
}

#[derive(Debug, Clone)]
pub struct VoiceGate {
    total_audio_ms: f64,
    voiced_audio_ms: f64,
    gate_open: bool,
    rms_threshold: f64,
    minimum_total_ms: f64,
    minimum_voiced_ms: f64,
}

impl VoiceGate {
    pub fn new(
        rms_threshold: f64,
        minimum_total_ms: f64,
        minimum_voiced_ms: f64,
    ) -> Self {
        Self {
            total_audio_ms: 0.0,
            voiced_audio_ms: 0.0,
            gate_open: false,
            rms_threshold,
            minimum_total_ms,
            minimum_voiced_ms,
        }
    }

    pub fn total_audio_ms(&self) -> f64 {
        self.total_audio_ms
    }

    pub fn voiced_audio_ms(&self) -> f64 {
        self.voiced_audio_ms
    }

    pub fn is_open(&self) -> bool {
        self.gate_open
    }

    /// Feed one 16 kHz mono PCM16 little-endian chunk.
    pub fn push_pcm16_le(&mut self, bytes: &[u8]) -> ChunkMetrics {
        let samples = bytes.len() / 2;
        let mut rms = 0.0;
        if samples > 0 {
            let mut sum = 0.0f64;
            for pair in bytes.chunks_exact(2) {
                let sample = i16::from_le_bytes([pair[0], pair[1]]) as f64;
                sum += sample * sample;
            }
            rms = (sum / samples as f64).sqrt();
            let chunk_ms = samples as f64 / 16.0;
            self.total_audio_ms += chunk_ms;
            if rms >= self.rms_threshold {
                self.voiced_audio_ms += chunk_ms;
            }
            self.gate_open = self.total_audio_ms >= self.minimum_total_ms
                && self.voiced_audio_ms >= self.minimum_voiced_ms;
        }
        ChunkMetrics {
            samples,
            rms,
            total_audio_ms: self.total_audio_ms,
            voiced_audio_ms: self.voiced_audio_ms,
            gate_open: self.gate_open,
            level: display_level(rms),
        }
    }
}

/// Map RMS to the 0..1 level used by the floating preview.
pub fn display_level(rms: f64) -> f64 {
    let normalized = ((rms - 35.0) / 850.0).clamp(0.0, 1.0);
    if normalized > 0.0 {
        normalized.sqrt()
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(value: i16) -> Vec<u8> {
        value.to_le_bytes().repeat(1600)
    }

    #[test]
    fn gate_opens_only_after_both_minimums() {
        let mut gate = VoiceGate::new(60.0, 180.0, 100.0);
        assert!(!gate.is_open());
        let first = gate.push_pcm16_le(&chunk(1000));
        assert_eq!(first.samples, 1600);
        assert_eq!(first.total_audio_ms, 100.0);
        assert!(!first.gate_open);
        let second = gate.push_pcm16_le(&chunk(1000));
        assert_eq!(second.total_audio_ms, 200.0);
        assert!(second.gate_open);
        assert!(gate.is_open());
    }

    #[test]
    fn silence_counts_toward_total_but_not_voiced() {
        let mut gate = VoiceGate::new(60.0, 180.0, 100.0);
        gate.push_pcm16_le(&chunk(0));
        gate.push_pcm16_le(&chunk(0));
        assert_eq!(gate.total_audio_ms(), 200.0);
        assert_eq!(gate.voiced_audio_ms(), 0.0);
        assert!(!gate.is_open());
    }

    #[test]
    fn level_is_quiet_below_the_floor_and_monotonic() {
        assert_eq!(display_level(0.0), 0.0);
        assert_eq!(display_level(35.0), 0.0);
        let low = display_level(300.0);
        let high = display_level(600.0);
        assert!(low > 0.0 && low < high && high <= 1.0);
        assert_eq!(display_level(10_000.0), 1.0);
    }

    #[test]
    fn empty_chunk_reports_a_zero_sample_without_changing_totals() {
        let mut gate = VoiceGate::new(60.0, 180.0, 100.0);
        let metrics = gate.push_pcm16_le(&[]);
        assert_eq!(metrics.samples, 0);
        assert_eq!(metrics.total_audio_ms, 0.0);
        assert_eq!(metrics.level, 0.0);
    }
}
