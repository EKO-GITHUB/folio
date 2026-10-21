//! A minimal MP4 muxer for one AAC-LC track, fed with ADTS frames.
//!
//! ffmpeg's MP4 muxer slows down quadratically with the number of frames (a 14 hour book takes 25 s
//! just to remux), while writing the sample tables directly takes as long as writing the data.
//! Tags and chapters are added afterwards with mp4ameta.

use std::{
    fs::File,
    io::{BufWriter, Seek, SeekFrom, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};

/// Frames grouped into one chunk, about a second of audio.
const FRAMES_PER_CHUNK: usize = 48;
/// Movie timescale (milliseconds), like ffmpeg uses.
const MOVIE_TIMESCALE: u64 = 1000;
const SAMPLES_PER_FRAME: u64 = 1024;
/// Sample rates in the order of the ADTS `sampling_frequency_index`.
const SAMPLE_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AudioConfig {
    /// MPEG-4 audio object type (2 = AAC-LC).
    object_type: u8,
    rate_index: u8,
    channels: u8,
}

impl AudioConfig {
    fn from_adts(header: &[u8]) -> Self {
        Self {
            object_type: (header[2] >> 6) + 1,
            rate_index: (header[2] >> 2) & 0x0F,
            channels: ((header[2] & 0x01) << 2) | (header[3] >> 6),
        }
    }

    fn sample_rate(self) -> u32 {
        SAMPLE_RATES
            .get(usize::from(self.rate_index))
            .copied()
            .unwrap_or(44100)
    }

    /// The two byte AudioSpecificConfig.
    fn specific_config(self) -> [u8; 2] {
        [
            (self.object_type << 3) | (self.rate_index >> 1),
            ((self.rate_index & 1) << 7) | (self.channels << 3),
        ]
    }
}

pub struct Writer {
    file: BufWriter<File>,
    mdat_start: u64,
    written: u64,
    sizes: Vec<u32>,
    config: Option<AudioConfig>,
}

impl Writer {
    pub fn create(path: &Path) -> Result<Self> {
        let mut file = BufWriter::with_capacity(1 << 20, File::create(path)?);
        let ftyp = atom(
            b"ftyp",
            &[
                b"M4B ".as_slice(),
                &512u32.to_be_bytes(),
                b"M4B M4A mp42isom",
            ]
            .concat(),
        );
        file.write_all(&ftyp)?;
        let mdat_start = ftyp.len() as u64;
        // A 64 bit mdat header, so books over 4 GB work. The size is filled in by `finish`.
        file.write_all(&[0, 0, 0, 1])?;
        file.write_all(b"mdat")?;
        file.write_all(&[0; 8])?;
        Ok(Self {
            file,
            mdat_start,
            written: 16,
            sizes: Vec::new(),
            config: None,
        })
    }

    /// Appends one ADTS frame, without its header.
    pub fn push_adts(&mut self, frame: &[u8]) -> Result<()> {
        ensure!(frame.len() > 7, "Truncated AAC frame");
        let header_len = if frame[1] & 0x01 == 0 { 9 } else { 7 };
        let config = AudioConfig::from_adts(frame);
        ensure!(
            *self.config.get_or_insert(config) == config,
            "The AAC stream changes format midway"
        );
        let payload = &frame[header_len..];
        self.file.write_all(payload)?;
        self.written += payload.len() as u64;
        self.sizes.push(u32::try_from(payload.len())?);
        Ok(())
    }

    /// Writes the sample tables. `priming` samples at the start and everything after `samples` real
    /// samples are hidden with an edit list.
    pub fn finish(mut self, priming: u64, samples: u64) -> Result<()> {
        let config = self.config.context("No audio was encoded")?;
        let rate = u64::from(config.sample_rate());
        let media_duration = self.sizes.len() as u64 * SAMPLES_PER_FRAME;
        ensure!(
            priming + samples <= media_duration,
            "The AAC stream is shorter than the audio"
        );

        let mut offsets = Vec::with_capacity(self.sizes.len() / FRAMES_PER_CHUNK + 1);
        let mut offset = self.mdat_start + 16;
        for chunk in self.sizes.chunks(FRAMES_PER_CHUNK) {
            offsets.push(offset);
            offset += chunk.iter().map(|&size| u64::from(size)).sum::<u64>();
        }
        let movie_duration = samples * MOVIE_TIMESCALE / rate;
        let average_bitrate = (self.written * 8 * rate / media_duration.max(1)) as u32;
        let moov = moov(
            config,
            &self.sizes,
            &offsets,
            priming,
            movie_duration,
            media_duration,
            average_bitrate,
        );
        self.file.write_all(&moov)?;

        let mut file = self.file.into_inner().map_err(|error| error.into_error())?;
        file.seek(SeekFrom::Start(self.mdat_start + 8))?;
        file.write_all(&self.written.to_be_bytes())?;
        file.sync_all()?;
        Ok(())
    }
}

fn moov(
    config: AudioConfig,
    sizes: &[u32],
    chunk_offsets: &[u64],
    priming: u64,
    movie_duration: u64,
    media_duration: u64,
    bitrate: u32,
) -> Vec<u8> {
    let rate = config.sample_rate();
    let matrix: Vec<u8> = [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000]
        .iter()
        .flat_map(|value| value.to_be_bytes())
        .collect();

    let mvhd = full_atom(
        b"mvhd",
        0,
        0,
        &[
            &[0; 8][..], // creation and modification time
            &(MOVIE_TIMESCALE as u32).to_be_bytes(),
            &(movie_duration as u32).to_be_bytes(),
            &0x0001_0000u32.to_be_bytes(), // rate 1.0
            &0x0100u16.to_be_bytes(),      // volume 1.0
            &[0; 10],
            &matrix,
            &[0; 24],            // pre-defined
            &2u32.to_be_bytes(), // next track id
        ]
        .concat(),
    );
    let tkhd = full_atom(
        b"tkhd",
        0,
        0x7,
        &[
            &[0; 8][..],
            &1u32.to_be_bytes(), // track id
            &[0; 4],
            &(movie_duration as u32).to_be_bytes(),
            &[0; 8],
            &0u16.to_be_bytes(),      // layer
            &0u16.to_be_bytes(),      // alternate group
            &0x0100u16.to_be_bytes(), // volume
            &[0; 2],
            &matrix,
            &[0; 8], // width and height
        ]
        .concat(),
    );
    let elst = full_atom(
        b"elst",
        1,
        0,
        &[
            &1u32.to_be_bytes()[..],
            &movie_duration.to_be_bytes(),
            &priming.to_be_bytes(),
            &0x0001_0000u32.to_be_bytes(), // rate 1.0
        ]
        .concat(),
    );
    let long_media = media_duration > u64::from(u32::MAX);
    let mdhd = if long_media {
        full_atom(
            b"mdhd",
            1,
            0,
            &[
                &[0; 16][..],
                &rate.to_be_bytes(),
                &media_duration.to_be_bytes(),
                &[0x55, 0xC4, 0, 0],
            ]
            .concat(),
        )
    } else {
        full_atom(
            b"mdhd",
            0,
            0,
            &[
                &[0; 8][..],
                &rate.to_be_bytes(),
                &(media_duration as u32).to_be_bytes(),
                &[0x55, 0xC4, 0, 0],
            ]
            .concat(),
        )
    };
    let hdlr = full_atom(
        b"hdlr",
        0,
        0,
        &[&[0; 4][..], b"soun", &[0; 12], b"SoundHandler\0"].concat(),
    );
    let smhd = full_atom(b"smhd", 0, 0, &[0; 4]);
    let dinf = atom(
        b"dinf",
        &full_atom(
            b"dref",
            0,
            0,
            &[&1u32.to_be_bytes()[..], &full_atom(b"url ", 0, 1, &[])].concat(),
        ),
    );

    let specific = config.specific_config();
    let decoder_specific = descriptor(0x05, &specific);
    let decoder_config = descriptor(
        0x04,
        &[
            &[0x40, 0x15][..],      // MPEG-4 audio, audio stream
            &[0, 0x18, 0],          // buffer size
            &bitrate.to_be_bytes(), // max bitrate
            &bitrate.to_be_bytes(), // average bitrate
            &decoder_specific,
        ]
        .concat(),
    );
    let es = descriptor(
        0x03,
        &[&[0, 0, 0][..], &decoder_config, &descriptor(0x06, &[0x02])].concat(),
    );
    let mp4a = atom(
        b"mp4a",
        &[
            &[0; 6][..],
            &1u16.to_be_bytes(), // data reference index
            &[0; 8],
            &u16::from(config.channels).to_be_bytes(),
            &16u16.to_be_bytes(), // sample size
            &[0; 4],
            &(rate << 16).to_be_bytes(),
            &full_atom(b"esds", 0, 0, &es),
        ]
        .concat(),
    );
    let stsd = full_atom(b"stsd", 0, 0, &[&1u32.to_be_bytes()[..], &mp4a].concat());
    let stts = full_atom(
        b"stts",
        0,
        0,
        &[
            &1u32.to_be_bytes()[..],
            &(sizes.len() as u32).to_be_bytes(),
            &(SAMPLES_PER_FRAME as u32).to_be_bytes(),
        ]
        .concat(),
    );
    let stsc = full_atom(b"stsc", 0, 0, &sample_to_chunk(sizes.len()));
    let stsz = full_atom(
        b"stsz",
        0,
        0,
        &[
            &0u32.to_be_bytes()[..],
            &(sizes.len() as u32).to_be_bytes(),
            &sizes
                .iter()
                .flat_map(|size| size.to_be_bytes())
                .collect::<Vec<_>>(),
        ]
        .concat(),
    );
    let offsets_fit = chunk_offsets
        .last()
        .is_none_or(|&last| last <= u64::from(u32::MAX));
    let chunk_table = if offsets_fit {
        let entries: Vec<u8> = chunk_offsets
            .iter()
            .flat_map(|&o| (o as u32).to_be_bytes())
            .collect();
        full_atom(
            b"stco",
            0,
            0,
            &[&(chunk_offsets.len() as u32).to_be_bytes()[..], &entries].concat(),
        )
    } else {
        let entries: Vec<u8> = chunk_offsets.iter().flat_map(|o| o.to_be_bytes()).collect();
        full_atom(
            b"co64",
            0,
            0,
            &[&(chunk_offsets.len() as u32).to_be_bytes()[..], &entries].concat(),
        )
    };
    let stbl = atom(b"stbl", &[stsd, stts, stsc, stsz, chunk_table].concat());
    let minf = atom(b"minf", &[smhd, dinf, stbl].concat());
    let mdia = atom(b"mdia", &[mdhd, hdlr, minf].concat());
    let trak = atom(b"trak", &[tkhd, atom(b"edts", &elst), mdia].concat());
    atom(b"moov", &[mvhd, trak].concat())
}

/// Every chunk holds `FRAMES_PER_CHUNK` frames, except possibly a shorter last one.
fn sample_to_chunk(frames: usize) -> Vec<u8> {
    let full_chunks = frames / FRAMES_PER_CHUNK;
    let remainder = frames % FRAMES_PER_CHUNK;
    let mut entries = Vec::new();
    if full_chunks > 0 {
        entries.push((1, FRAMES_PER_CHUNK));
    }
    if remainder > 0 {
        entries.push((full_chunks + 1, remainder));
    }
    let mut body = (entries.len() as u32).to_be_bytes().to_vec();
    for (first_chunk, samples) in entries {
        body.extend_from_slice(&(first_chunk as u32).to_be_bytes());
        body.extend_from_slice(&(samples as u32).to_be_bytes());
        body.extend_from_slice(&1u32.to_be_bytes());
    }
    body
}

fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(body.len() + 8);
    bytes.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(body);
    bytes
}

fn full_atom(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut header = flags.to_be_bytes();
    header[0] = version;
    atom(kind, &[&header[..], body].concat())
}

/// An MPEG-4 descriptor with a one byte length.
fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
    [&[tag, body.len() as u8][..], body].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_audio_config_from_adts() {
        // AAC-LC, 44.1 kHz (index 4), mono.
        let header = [0xFF, 0xF1, 0x50, 0x40, 0, 0, 0xFC];
        let config = AudioConfig::from_adts(&header);
        assert_eq!(
            config,
            AudioConfig {
                object_type: 2,
                rate_index: 4,
                channels: 1
            }
        );
        assert_eq!(config.sample_rate(), 44100);
        assert_eq!(config.specific_config(), [0x12, 0x08]);
    }

    #[test]
    fn groups_frames_into_chunks() {
        let body = sample_to_chunk(FRAMES_PER_CHUNK * 3 + 5);
        assert_eq!(u32::from_be_bytes(body[..4].try_into().unwrap()), 2);
        assert_eq!(u32::from_be_bytes(body[16..20].try_into().unwrap()), 4);
        assert_eq!(u32::from_be_bytes(body[20..24].try_into().unwrap()), 5);
    }
}
