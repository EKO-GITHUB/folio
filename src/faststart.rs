//! Copies an MP4 while moving its `moov` atom in front of the media data ("faststart"), so players
//! and servers can start without reading the end of the file.
//!
//! ffmpeg writes `moov` last and the tags are added while it's still last, which only rewrites the
//! small `moov` instead of shifting hundreds of megabytes. The move then happens during the copy
//! into the library, which has to read and write every byte anyway.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, bail, ensure};

const COPY_BUFFER: usize = 8 << 20;

#[derive(Debug)]
struct Atom {
    kind: [u8; 4],
    start: u64,
    size: u64,
}

/// Copies `from` to `to`, placing `moov` before `mdat` when it can.
pub fn copy(from: &Path, to: &Path, progress: impl Fn(f32), cancel: &AtomicBool) -> Result<()> {
    let mut source = File::open(from)?;
    let length = source.metadata()?.len();
    let atoms = top_level_atoms(&mut source, length)?;
    let mut target =
        File::create(to).with_context(|| format!("Could not create {}", to.display()))?;
    let mut copied = 0u64;
    let mut report = |bytes: u64| {
        copied += bytes;
        progress(copied as f32 / length.max(1) as f32);
    };

    let moov = atoms.iter().position(|atom| &atom.kind == b"moov");
    let mdat = atoms.iter().position(|atom| &atom.kind == b"mdat");
    let relocated = match (moov, mdat) {
        (Some(moov), Some(mdat)) if moov > mdat => {
            let mut bytes = vec![0; usize::try_from(atoms[moov].size)?];
            source.seek(SeekFrom::Start(atoms[moov].start))?;
            source.read_exact(&mut bytes)?;
            shift_chunk_offsets(&mut bytes, atoms[moov].size)
                .ok()
                .map(|()| (moov, mdat, bytes))
        }
        _ => None,
    };

    match relocated {
        Some((moov, mdat, bytes)) => {
            for (index, atom) in atoms.iter().enumerate() {
                if index == moov {
                    continue;
                }
                if index == mdat {
                    target.write_all(&bytes)?;
                    report(bytes.len() as u64);
                }
                copy_range(
                    &mut source,
                    &mut target,
                    atom.start,
                    atom.size,
                    &mut report,
                    cancel,
                )?;
            }
        }
        None => copy_range(&mut source, &mut target, 0, length, &mut report, cancel)?,
    }
    target.sync_all()?;
    Ok(())
}

fn copy_range(
    source: &mut File,
    target: &mut File,
    start: u64,
    size: u64,
    report: &mut impl FnMut(u64),
    cancel: &AtomicBool,
) -> Result<()> {
    source.seek(SeekFrom::Start(start))?;
    let mut remaining = size;
    let mut buffer = vec![0; COPY_BUFFER];
    while remaining > 0 {
        if cancel.load(Ordering::Relaxed) {
            bail!("Cancelled");
        }
        let chunk = remaining.min(COPY_BUFFER as u64) as usize;
        source.read_exact(&mut buffer[..chunk])?;
        target.write_all(&buffer[..chunk])?;
        remaining -= chunk as u64;
        report(chunk as u64);
    }
    Ok(())
}

fn top_level_atoms(file: &mut File, length: u64) -> Result<Vec<Atom>> {
    let mut atoms = Vec::new();
    let mut position = 0;
    while position < length {
        file.seek(SeekFrom::Start(position))?;
        let mut header = [0; 16];
        file.read_exact(&mut header[..8])?;
        let size = match u32::from_be_bytes(header[..4].try_into()?) {
            0 => length - position,
            1 => {
                file.read_exact(&mut header[8..])?;
                u64::from_be_bytes(header[8..].try_into()?)
            }
            size => u64::from(size),
        };
        ensure!(
            size >= 8 && position + size <= length,
            "Malformed MP4 atom at byte {position}"
        );
        atoms.push(Atom {
            kind: header[4..8].try_into()?,
            start: position,
            size,
        });
        position += size;
    }
    Ok(atoms)
}

/// Adds `delta` to every chunk offset in a `moov` atom. Fails if an offset would no longer fit.
fn shift_chunk_offsets(moov: &mut [u8], delta: u64) -> Result<()> {
    let header = atom_header_len(moov)?;
    shift_in_children(&mut moov[header..], delta)
}

fn atom_header_len(atom: &[u8]) -> Result<usize> {
    ensure!(atom.len() >= 8, "Truncated MP4 atom");
    Ok(if u32::from_be_bytes(atom[..4].try_into()?) == 1 {
        16
    } else {
        8
    })
}

fn shift_in_children(mut data: &mut [u8], delta: u64) -> Result<()> {
    while data.len() >= 8 {
        let size = match u32::from_be_bytes(data[..4].try_into()?) {
            0 => data.len(),
            1 => usize::try_from(u64::from_be_bytes(
                data.get(8..16).context("Truncated atom")?.try_into()?,
            ))?,
            size => size as usize,
        };
        ensure!(size >= 8 && size <= data.len(), "Malformed MP4 atom");
        let (atom, rest) = data.split_at_mut(size);
        let header = atom_header_len(atom)?;
        let kind: [u8; 4] = atom[4..8].try_into()?;
        let body = &mut atom[header..];
        match &kind {
            b"trak" | b"mdia" | b"minf" | b"stbl" => shift_in_children(body, delta)?,
            b"stco" => shift_table::<4>(body, delta)?,
            b"co64" => shift_table::<8>(body, delta)?,
            _ => {}
        }
        data = rest;
    }
    Ok(())
}

/// Shifts the entries of an `stco` (4 byte) or `co64` (8 byte) table.
fn shift_table<const WIDTH: usize>(body: &mut [u8], delta: u64) -> Result<()> {
    ensure!(body.len() >= 8, "Truncated chunk offset table");
    let count = u32::from_be_bytes(body[4..8].try_into()?) as usize;
    let entries = body
        .get_mut(8..8 + count * WIDTH)
        .context("Truncated chunk offset table")?;
    for entry in entries.chunks_exact_mut(WIDTH) {
        let mut bytes = [0; 8];
        bytes[8 - WIDTH..].copy_from_slice(entry);
        let offset = u64::from_be_bytes(bytes) + delta;
        if WIDTH == 4 {
            let offset =
                u32::try_from(offset).map_err(|_| io::Error::other("chunk offset overflow"))?;
            entry.copy_from_slice(&offset.to_be_bytes());
        } else {
            entry.copy_from_slice(&offset.to_be_bytes());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut bytes = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(body);
        bytes
    }

    fn stco(offsets: &[u32]) -> Vec<u8> {
        let mut body = vec![0, 0, 0, 0];
        body.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
        for offset in offsets {
            body.extend_from_slice(&offset.to_be_bytes());
        }
        atom(b"stco", &body)
    }

    #[test]
    fn moves_moov_in_front_of_mdat() {
        let ftyp = atom(b"ftyp", b"M4B \0\0\0\0");
        let mdat = atom(b"mdat", b"AAAABBBB");
        let data_start = (ftyp.len() + 8) as u32;
        let offsets = [data_start, data_start + 4];
        let stbl = atom(b"stbl", &stco(&offsets));
        let trak = atom(b"trak", &atom(b"mdia", &atom(b"minf", &stbl)));
        let moov = atom(b"moov", &[atom(b"udta", b"tags"), trak].concat());
        let file = [ftyp.clone(), mdat.clone(), moov.clone()].concat();

        let dir = std::env::temp_dir().join(format!("folio-faststart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("in.mp4"), &file).unwrap();
        copy(
            &dir.join("in.mp4"),
            &dir.join("out.mp4"),
            |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        let out = std::fs::read(dir.join("out.mp4")).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(out.len(), file.len());
        assert_eq!(&out[..ftyp.len()], &ftyp[..]);
        assert_eq!(&out[ftyp.len() + 4..ftyp.len() + 8], b"moov");
        let mdat_start = ftyp.len() + moov.len();
        assert_eq!(&out[mdat_start..], &mdat[..]);
        let shifted: Vec<u8> = offsets
            .iter()
            .flat_map(|o| (o + moov.len() as u32).to_be_bytes())
            .collect();
        assert!(out.windows(shifted.len()).any(|w| w == shifted.as_slice()));
        let first = u32::from_be_bytes(shifted[..4].try_into().unwrap()) as usize;
        assert_eq!(&out[first..first + 4], b"AAAA");
    }

    #[test]
    fn refuses_offsets_that_overflow() {
        let mut moov = atom(
            b"moov",
            &atom(
                b"trak",
                &atom(
                    b"mdia",
                    &atom(b"minf", &atom(b"stbl", &stco(&[u32::MAX - 1]))),
                ),
            ),
        );
        assert!(shift_chunk_offsets(&mut moov, 10).is_err());
    }
}
