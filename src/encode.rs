//! Parallel AAC encoding.
//!
//! ffmpeg's AAC encoder is single threaded, so the book is cut into one segment per worker and every
//! segment is encoded by its own ffmpeg process. To make the joins inaudible, each worker also encodes
//! a little audio before and after its segment, and only the AAC frames that cover the segment itself
//! are kept. Neighbouring frames then come from encoders that saw the same signal, so the decoder's
//! overlap-add across a join reconstructs the audio just like in a single encode.
//!
//! That only works if every worker agrees on sample positions to the sample, so the files are first
//! decoded once to measure their exact length, and all later decodes use the identical command.

use std::{
    fs,
    io::{Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
};

use anyhow::{Context, Result, bail, ensure};

use crate::ffmpeg::{self, Tools};

/// Samples per AAC-LC frame.
pub const FRAME: u64 = 1024;
/// ffmpeg's AAC encoder starts every stream with one frame of priming.
pub const PRIMING: u64 = FRAME;
/// Extra audio each worker encodes before its segment. A multiple of `FRAME`.
const PRE_ROLL: u64 = 2 * FRAME;
/// Extra audio each worker encodes after its segment.
const POST_ROLL: u64 = 4 * FRAME;
/// Segments aren't made shorter than this, so short books use fewer workers.
const MIN_SEGMENT_SECS: u64 = 90;
/// How far a cut may move from its ideal position to find a quiet spot.
const CUT_SEARCH_SECS: u64 = 15;
const BYTES_PER_SAMPLE: u64 = 2;

#[derive(Clone, Copy, Debug)]
pub struct PcmFormat {
    pub sample_rate: u32,
    pub channels: u32,
}

impl PcmFormat {
    /// Bytes per sample frame (one sample for every channel).
    fn stride(self) -> u64 {
        BYTES_PER_SAMPLE * u64::from(self.channels)
    }
}

pub struct Settings {
    pub format: PcmFormat,
    pub bitrate_k: u32,
    pub workers: usize,
    pub low_priority: bool,
}

/// Decodes `file` to interleaved 16 bit PCM, handing chunks to `sink` until it returns false.
fn decode(
    tools: &Tools,
    file: &Path,
    format: PcmFormat,
    low_priority: bool,
    cancel: &AtomicBool,
    mut sink: impl FnMut(&[u8]) -> Result<bool>,
) -> Result<()> {
    let mut child = ffmpeg::command(&tools.ffmpeg, low_priority)
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(file)
        .args(["-map", "0:a:0", "-c:a", "pcm_s16le", "-f", "s16le"])
        .args([
            "-ar",
            &format.sample_rate.to_string(),
            "-ac",
            &format.channels.to_string(),
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Could not start ffmpeg")?;
    let errors = ffmpeg::drain_stderr(child.stderr.take());
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut buffer = vec![0; 1 << 20];
    let outcome = loop {
        if cancel.load(Ordering::Relaxed) {
            break Err(anyhow::anyhow!("Cancelled"));
        }
        let read = match stdout.read(&mut buffer) {
            Ok(0) => break Ok(true),
            Ok(read) => read,
            Err(error) => break Err(error.into()),
        };
        match sink(&buffer[..read]) {
            Ok(true) => {}
            Ok(false) => break Ok(false),
            Err(error) => break Err(error),
        }
    };
    let finished = match outcome {
        Ok(finished) => finished,
        Err(error) => {
            child.kill().ok();
            child.wait().ok();
            return Err(error);
        }
    };
    if !finished {
        // The sink has all it needs; the rest of the file doesn't matter.
        child.kill().ok();
        child.wait().ok();
        return Ok(());
    }
    let status = child.wait()?;
    let errors = errors.join().unwrap_or_default();
    ensure!(
        status.success(),
        "Could not decode {}: {}",
        file.display(),
        errors.trim()
    );
    Ok(())
}

/// Runs `job` for every index in `0..count` on `workers` threads, stopping at the first error.
fn for_each_parallel(
    count: usize,
    workers: usize,
    cancel: &AtomicBool,
    job: impl Fn(usize) -> Result<()> + Sync,
) -> Result<()> {
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    thread::scope(|scope| {
        let handles: Vec<_> = (0..workers.clamp(1, count.max(1)))
            .map(|_| {
                scope.spawn(|| -> Result<()> {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= count || failed.load(Ordering::Relaxed) {
                            return Ok(());
                        }
                        if let Err(error) = job(index) {
                            failed.store(true, Ordering::Relaxed);
                            return Err(error);
                        }
                    }
                })
            })
            .collect();
        let mut first_error = None;
        for handle in handles {
            if let Err(error) = handle.join().expect("worker thread panicked") {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(_) if cancel.load(Ordering::Relaxed) => bail!("Cancelled"),
            Some(error) => Err(error),
            None => Ok(()),
        }
    })
}

/// What decoding every file once tells us about the book.
pub struct Measurement {
    /// The exact length of every file in samples.
    pub lengths: Vec<u64>,
    /// The RMS level of every `FRAME` samples of every file, for finding quiet places to cut.
    levels: Vec<Vec<f32>>,
}

impl Measurement {
    pub fn total(&self) -> u64 {
        self.lengths.iter().sum()
    }

    /// Where every file starts, plus the end of the last one.
    fn offsets(&self) -> Vec<u64> {
        std::iter::once(0)
            .chain(self.lengths.iter().scan(0, |start, length| {
                *start += length;
                Some(*start)
            }))
            .collect()
    }

    /// Cuts the book into `count` segments at quiet places.
    pub fn segments(&self, count: usize, sample_rate: u32) -> Vec<Range<u64>> {
        let offsets = self.offsets();
        plan_segments(self.total(), count, sample_rate, &|position| {
            self.level_at(&offsets, position)
        })
    }

    /// The level of the frame around `position`, give or take the frame alignment of each file.
    fn level_at(&self, offsets: &[u64], position: u64) -> f32 {
        let file = offsets
            .partition_point(|&start| start <= position)
            .saturating_sub(1);
        let file = file.min(self.levels.len().saturating_sub(1));
        let levels = &self.levels[file];
        let frame = ((position - offsets[file].min(position)) / FRAME) as usize;
        levels.get(frame).or(levels.last()).copied().unwrap_or(0.0)
    }
}

/// Tracks the RMS level of each `FRAME` of interleaved 16 bit PCM.
struct LevelMeter {
    samples_per_frame: u64,
    odd_byte: Option<u8>,
    sum: f64,
    count: u64,
    levels: Vec<f32>,
}

impl LevelMeter {
    fn new(channels: u32) -> Self {
        Self {
            samples_per_frame: FRAME * u64::from(channels),
            odd_byte: None,
            sum: 0.0,
            count: 0,
            levels: Vec::new(),
        }
    }

    fn feed(&mut self, mut chunk: &[u8]) {
        if let Some(low) = self.odd_byte.take() {
            let Some((&high, rest)) = chunk.split_first() else {
                self.odd_byte = Some(low);
                return;
            };
            self.add(i16::from_le_bytes([low, high]));
            chunk = rest;
        }
        let mut pairs = chunk.chunks_exact(2);
        for pair in &mut pairs {
            self.add(i16::from_le_bytes([pair[0], pair[1]]));
        }
        self.odd_byte = pairs.remainder().first().copied();
    }

    fn add(&mut self, sample: i16) {
        let sample = f64::from(sample);
        self.sum += sample * sample;
        self.count += 1;
        if self.count == self.samples_per_frame {
            self.push();
        }
    }

    fn push(&mut self) {
        self.levels
            .push((self.sum / self.count as f64).sqrt() as f32);
        self.sum = 0.0;
        self.count = 0;
    }

    fn finish(mut self) -> Vec<f32> {
        if self.count > 0 {
            self.push();
        }
        self.levels
    }
}

/// Decodes every file once, in parallel, to learn its exact length and where it is quiet.
pub fn measure(
    tools: &Tools,
    files: &[PathBuf],
    settings: &Settings,
    cancel: &AtomicBool,
    progress: &(dyn Fn(f32) + Sync),
) -> Result<Measurement> {
    let results: Vec<Mutex<(u64, Vec<f32>)>> = files.iter().map(|_| Mutex::default()).collect();
    let done = AtomicUsize::new(0);
    for_each_parallel(files.len(), settings.workers, cancel, |index| {
        let mut bytes = 0u64;
        let mut meter = LevelMeter::new(settings.format.channels);
        decode(
            tools,
            &files[index],
            settings.format,
            settings.low_priority,
            cancel,
            |chunk| {
                bytes += chunk.len() as u64;
                meter.feed(chunk);
                Ok(true)
            },
        )?;
        *results[index].lock().unwrap() = (bytes / settings.format.stride(), meter.finish());
        progress((done.fetch_add(1, Ordering::Relaxed) + 1) as f32 / files.len() as f32);
        Ok(())
    })?;
    let (lengths, levels) = results
        .into_iter()
        .map(|result| result.into_inner().unwrap())
        .unzip();
    Ok(Measurement { lengths, levels })
}

/// Splits `total` samples into at most `count` segments. Inner cuts are multiples of `FRAME`, placed at
/// the quietest spot near the ideal position so that nothing audible happens at a join.
fn plan_segments(
    total: u64,
    count: usize,
    sample_rate: u32,
    level: &dyn Fn(u64) -> f32,
) -> Vec<Range<u64>> {
    let rate = u64::from(sample_rate);
    let count = (count as u64)
        .min(total / (MIN_SEGMENT_SECS * rate).max(1))
        .max(1);
    let reach = (total / count / 4).min(CUT_SEARCH_SECS * rate) / FRAME * FRAME;
    let loudness = |cut: u64| -> f32 {
        (0..4)
            .map(|k| level((cut + k * FRAME).saturating_sub(2 * FRAME)))
            .sum()
    };
    let mut cuts = vec![0];
    for i in 1..count {
        let ideal = total * i / count / FRAME * FRAME;
        let earliest = cuts[cuts.len() - 1] + PRE_ROLL + FRAME;
        let quietest = (ideal.saturating_sub(reach)..=ideal + reach)
            .step_by(FRAME as usize)
            .filter(|&cut| cut >= earliest && cut + POST_ROLL < total)
            .min_by(|&a, &b| loudness(a).total_cmp(&loudness(b)));
        if let Some(cut) = quietest {
            cuts.push(cut);
        }
    }
    cuts.push(total);
    cuts.windows(2).map(|pair| pair[0]..pair[1]).collect()
}

/// Encodes the samples `segments` cover and hands the joined stream to `sink`, one ADTS frame at a
/// time. The stream starts with `PRIMING` samples of encoder delay.
#[allow(clippy::too_many_arguments)]
pub fn encode(
    tools: &Tools,
    files: &[PathBuf],
    measurement: &Measurement,
    segments: &[Range<u64>],
    settings: &Settings,
    work_dir: &Path,
    sink: &mut dyn FnMut(&[u8]) -> Result<()>,
    cancel: &AtomicBool,
    progress: &(dyn Fn(f32) + Sync),
) -> Result<()> {
    let total = measurement.total();
    let offsets = measurement.offsets();
    let parts: Vec<PathBuf> = (0..segments.len())
        .map(|i| work_dir.join(format!("part{i:03}.aac")))
        .collect();
    let inputs: Vec<Range<u64>> = segments
        .iter()
        .enumerate()
        .map(|(i, segment)| {
            let before = if i == 0 { 0 } else { PRE_ROLL };
            let after = if i + 1 == segments.len() {
                0
            } else {
                POST_ROLL
            };
            segment.start - before..(segment.end + after).min(total)
        })
        .collect();
    let expected_bytes: u64 = inputs
        .iter()
        .map(|input| (input.end - input.start) * settings.format.stride())
        .sum();
    let fed = AtomicU64::new(0);
    let report = |bytes: u64| {
        let fed = fed.fetch_add(bytes, Ordering::Relaxed) + bytes;
        progress(fed as f32 / expected_bytes.max(1) as f32);
    };

    for_each_parallel(segments.len(), settings.workers, cancel, |i| {
        encode_segment(
            tools,
            files,
            &offsets,
            inputs[i].clone(),
            settings,
            &parts[i],
            cancel,
            &report,
        )
    })?;

    for (i, segment) in segments.iter().enumerate() {
        let data = fs::read(&parts[i])?;
        let frames = adts_frames(&data)?;
        let keep = kept_frames(i, segment, i + 1 == segments.len(), frames.len())?;
        for frame in &frames[keep] {
            sink(&data[frame.clone()])?;
        }
        fs::remove_file(&parts[i]).ok();
    }
    Ok(())
}

/// Streams the samples in `input` from the source files into one AAC encoder.
#[allow(clippy::too_many_arguments)]
fn encode_segment(
    tools: &Tools,
    files: &[PathBuf],
    offsets: &[u64],
    input: Range<u64>,
    settings: &Settings,
    output: &Path,
    cancel: &AtomicBool,
    report: &(dyn Fn(u64) + Sync),
) -> Result<()> {
    let format = settings.format;
    let mut encoder = ffmpeg::command(&tools.ffmpeg, settings.low_priority)
        .args(["-v", "error", "-nostdin", "-y", "-f", "s16le"])
        .args([
            "-ar",
            &format.sample_rate.to_string(),
            "-ac",
            &format.channels.to_string(),
        ])
        .args([
            "-i",
            "pipe:0",
            "-c:a",
            "aac",
            "-b:a",
            &format!("{}k", settings.bitrate_k),
            "-f",
            "adts",
        ])
        .arg(output)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("Could not start ffmpeg")?;
    let errors = ffmpeg::drain_stderr(encoder.stderr.take());
    let mut stdin = encoder.stdin.take().expect("piped stdin");
    let stride = format.stride();

    let fed = (|| -> Result<()> {
        for (i, file) in files.iter().enumerate() {
            let (start, end) = (offsets[i], offsets[i + 1]);
            if end <= input.start || start >= input.end {
                continue;
            }
            let skip = input.start.saturating_sub(start) * stride;
            let wanted = (input.end.min(end) - input.start.max(start)) * stride;
            let mut position = 0u64;
            let mut written = 0u64;
            decode(
                tools,
                file,
                format,
                settings.low_priority,
                cancel,
                |chunk| {
                    let chunk_start = position;
                    position += chunk.len() as u64;
                    let from = skip.saturating_sub(chunk_start).min(chunk.len() as u64) as usize;
                    let to = (skip + wanted)
                        .saturating_sub(chunk_start)
                        .min(chunk.len() as u64) as usize;
                    if from < to {
                        stdin.write_all(&chunk[from..to])?;
                        written += (to - from) as u64;
                        report((to - from) as u64);
                    }
                    Ok(position < skip + wanted)
                },
            )?;
            ensure!(
                written == wanted,
                "{} decoded to a different length than when it was measured",
                file.display()
            );
        }
        Ok(())
    })();
    drop(stdin);

    if let Err(error) = fed {
        encoder.kill().ok();
        encoder.wait().ok();
        let errors = errors.join().unwrap_or_default();
        return Err(if errors.trim().is_empty() {
            error
        } else {
            error.context(errors.trim().to_owned())
        });
    }
    let status = encoder.wait()?;
    let errors = errors.join().unwrap_or_default();
    ensure!(
        status.success(),
        "The AAC encoder failed: {}",
        errors.trim()
    );
    Ok(())
}

/// The byte range of every frame in an ADTS stream.
fn adts_frames(data: &[u8]) -> Result<Vec<Range<usize>>> {
    let mut frames = Vec::new();
    let mut position = 0;
    while position < data.len() {
        let header = &data[position..];
        ensure!(
            header.len() >= 7 && header[0] == 0xFF && header[1] & 0xF6 == 0xF0,
            "Unexpected data in the AAC stream at byte {position}"
        );
        let length = (usize::from(header[3] & 0x03) << 11)
            | (usize::from(header[4]) << 3)
            | (usize::from(header[5]) >> 5);
        let blocks = usize::from(header[6] & 0x03) + 1;
        ensure!(
            blocks == 1 && length >= 7 && position + length <= data.len(),
            "Unsupported AAC frame at byte {position}"
        );
        frames.push(position..position + length);
        position += length;
    }
    Ok(frames)
}

/// Which frames of a segment's encode belong in the joined stream. The first segment keeps its
/// priming frame, later ones drop priming and pre-roll, and all but the last drop their post-roll.
fn kept_frames(
    index: usize,
    segment: &Range<u64>,
    is_last: bool,
    available: usize,
) -> Result<Range<usize>> {
    let first = if index == 0 {
        0
    } else {
        ((PRE_ROLL + PRIMING) / FRAME) as usize
    };
    let own = ((segment.end - segment.start) / FRAME) as usize;
    let end = match (index, is_last) {
        (_, true) => available,
        (0, false) => (PRIMING / FRAME) as usize + own,
        (_, false) => first + own,
    };
    ensure!(
        first < end && end <= available,
        "Segment {index} encoded to only {available} frames"
    );
    Ok(first..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_cover_everything_with_frame_aligned_cuts() {
        let rate = 44100;
        let total = 3600 * u64::from(rate) + 123;
        let segments = plan_segments(total, 8, rate, &|_| 1.0);
        assert_eq!(segments.len(), 8);
        assert_eq!(segments[0].start, 0);
        assert_eq!(segments.last().unwrap().end, total);
        for pair in segments.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
            assert_eq!(pair[1].start % FRAME, 0);
        }
    }

    #[test]
    fn cuts_at_the_quietest_nearby_spot() {
        let rate = 44100;
        let total = 1000 * u64::from(rate);
        let pause = 490 * u64::from(rate) / FRAME * FRAME;
        let level = |position: u64| {
            if position.abs_diff(pause) < 3 * FRAME {
                0.0
            } else {
                1000.0
            }
        };
        let segments = plan_segments(total, 2, rate, &level);
        assert_eq!(segments[1].start, pause);
    }

    #[test]
    fn short_books_use_fewer_segments() {
        let rate = 44100;
        assert_eq!(
            plan_segments(200 * u64::from(rate), 32, rate, &|_| 1.0).len(),
            2
        );
        assert_eq!(
            plan_segments(10 * u64::from(rate), 32, rate, &|_| 1.0).len(),
            1
        );
    }

    #[test]
    fn keeps_the_frames_of_the_segment_itself() {
        let segment = 0..10 * FRAME;
        assert_eq!(kept_frames(0, &segment, false, 20).unwrap(), 0..11);
        let segment = 10 * FRAME..20 * FRAME;
        assert_eq!(kept_frames(1, &segment, false, 20).unwrap(), 3..13);
        assert_eq!(kept_frames(2, &segment, true, 20).unwrap(), 3..20);
    }

    #[test]
    fn parses_adts_frames() {
        let frame = |len: usize| {
            let mut f = vec![0xFF, 0xF1, 0x50, 0x80, 0, 0, 0xFC];
            f[3] |= ((len >> 11) & 0x03) as u8;
            f[4] = ((len >> 3) & 0xFF) as u8;
            f[5] = (((len & 0x07) << 5) | 0x1F) as u8;
            f.resize(len, 0);
            f
        };
        let data = [frame(20), frame(9)].concat();
        assert_eq!(adts_frames(&data).unwrap(), vec![0..20, 20..29]);
        assert!(adts_frames(&data[..25]).is_err());
    }
}

#[cfg(test)]
mod live {
    use super::*;

    fn pcm(tools: &Tools, file: &Path, format: PcmFormat) -> Vec<i16> {
        let mut bytes = Vec::new();
        decode(
            tools,
            file,
            format,
            false,
            &AtomicBool::new(false),
            |chunk| {
                bytes.extend_from_slice(chunk);
                Ok(true)
            },
        )
        .unwrap();
        bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect()
    }

    fn rms(samples: impl Iterator<Item = f64>) -> f64 {
        let (sum, n) = samples.fold((0.0, 0usize), |(s, n), x| (s + x * x, n + 1));
        (sum / n.max(1) as f64).sqrt()
    }

    #[test]
    #[ignore]
    fn joins_are_seamless() {
        let dir = PathBuf::from(std::env::var("FOLIO_SAMPLE").unwrap());
        let tools = ffmpeg::locate().unwrap();
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "mp3"))
            .collect();
        files.sort();
        files.truncate(6);
        let format = PcmFormat {
            sample_rate: 44100,
            channels: 1,
        };
        let work = std::env::temp_dir().join("folio-seam-test");
        fs::create_dir_all(&work).unwrap();
        let cancel = AtomicBool::new(false);
        let mut source = Vec::new();
        for file in &files {
            source.extend(pcm(&tools, file, format));
        }
        let mut baseline: Vec<i16> = Vec::new();
        for workers in [1, 32] {
            let settings = Settings {
                format,
                bitrate_k: 96,
                workers,
                low_priority: false,
            };
            let start = std::time::Instant::now();
            let measurement = measure(&tools, &files, &settings, &cancel, &|_| {}).unwrap();
            let measured = start.elapsed();
            let total = measurement.total();
            assert_eq!(total as usize, source.len());
            let segments = measurement.segments(workers, 44100);
            let m4b = work.join(format!("joined{workers}.m4b"));
            let mut writer = crate::mp4::Writer::create(&m4b).unwrap();
            let mut frames = 0;
            let mut sink = |frame: &[u8]| {
                frames += 1;
                writer.push_adts(frame)
            };
            encode(
                &tools,
                &files,
                &measurement,
                &segments,
                &settings,
                &work,
                &mut sink,
                &cancel,
                &|_| {},
            )
            .unwrap();
            writer.finish(PRIMING, total).unwrap();
            let encoded = start.elapsed();
            let decoded = pcm(&tools, &m4b, format);
            let n = source.len().min(decoded.len());
            let error = |range: Range<usize>| {
                rms(range.map(|i| f64::from(source[i]) - f64::from(decoded[i])))
            };
            let signal = rms(source.iter().map(|&s| f64::from(s)));
            let overall = error(0..n);
            let window = 1024;
            let mut windows: Vec<f64> = (0..n / window)
                .map(|w| error(w * window..(w + 1) * window))
                .collect();
            windows.sort_by(f64::total_cmp);
            let joins: Vec<f64> = segments[1..]
                .iter()
                .map(|s| error(s.start as usize - 2 * window..s.start as usize + 2 * window))
                .collect();
            let worst_join = joins.iter().copied().fold(0.0, f64::max);
            println!(
                "workers {workers}: {} segments, {frames} frames, source {} decoded {} samples, measure {measured:?}, total {encoded:?}",
                segments.len(),
                source.len(),
                decoded.len()
            );
            println!(
                "  signal rms {signal:.0}, error rms {overall:.1} (snr {:.1} dB), window error p50 {:.1} p99 {:.1} max {:.1}, worst join {worst_join:.1}",
                20.0 * (signal / overall).log10(),
                windows[windows.len() / 2],
                windows[windows.len() * 99 / 100],
                windows[windows.len() - 1],
            );
            if baseline.is_empty() {
                baseline = decoded;
                continue;
            }
            let ratios: Vec<String> = segments[1..]
                .iter()
                .zip(&joins)
                .map(|(s, join)| {
                    let range = s.start as usize - 2 * window..s.start as usize + 2 * window;
                    let single = rms(range.map(|i| f64::from(source[i]) - f64::from(baseline[i])));
                    format!("{join:.0}/{single:.0}")
                })
                .collect();
            println!("  join error parallel/single: {}", ratios.join(" "));
        }
        fs::remove_dir_all(&work).ok();
    }
}
