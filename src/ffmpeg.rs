use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{ChildStderr, Command},
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

const DOWNLOAD_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";

#[derive(Clone, Debug)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

/// Looks for ffmpeg on PATH, next to folio.exe, then in the folder `download` installs to.
pub fn locate() -> Option<Tools> {
    let path_dirs = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    path_dirs
        .into_iter()
        .chain(exe_dir)
        .chain(download_dir())
        .find_map(|dir| tools_in(&dir))
}

fn tools_in(dir: &Path) -> Option<Tools> {
    let tools = Tools {
        ffmpeg: dir.join("ffmpeg.exe"),
        ffprobe: dir.join("ffprobe.exe"),
    };
    (tools.ffmpeg.is_file() && tools.ffprobe.is_file()).then_some(tools)
}

fn download_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("folio").join("ffmpeg"))
}

/// Downloads a static ffmpeg build into `%LOCALAPPDATA%\folio\ffmpeg`.
pub fn download(progress: impl Fn(f32)) -> Result<Tools> {
    let dir = download_dir().context("Could not find the local app data folder")?;
    fs::create_dir_all(&dir)?;
    let zip_path = dir.join("ffmpeg.zip");

    let client = reqwest::blocking::Client::builder().timeout(None).build()?;
    let mut response = client.get(DOWNLOAD_URL).send()?.error_for_status()?;
    let total = response.content_length();
    let mut file = File::create(&zip_path)?;
    let mut buffer = vec![0; 1 << 16];
    let mut done = 0u64;
    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        done += read as u64;
        if let Some(total) = total {
            progress(done as f32 / total as f32);
        }
    }
    drop(file);

    let mut archive = zip::ZipArchive::new(File::open(&zip_path)?)?;
    for exe in ["ffmpeg.exe", "ffprobe.exe"] {
        let entry = archive
            .file_names()
            .find(|name| name.ends_with(&format!("/bin/{exe}")))
            .map(str::to_owned)
            .with_context(|| format!("{exe} is missing from the ffmpeg download"))?;
        io::copy(
            &mut archive.by_name(&entry)?,
            &mut File::create(dir.join(exe))?,
        )?;
    }
    drop(archive);
    fs::remove_file(&zip_path).ok();
    tools_in(&dir).context("ffmpeg could not be installed")
}

/// A command that doesn't flash a console window when started from the GUI.
/// `low_priority` keeps long encodes from slowing down whatever else is running.
pub fn command(program: &Path, low_priority: bool) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        let priority = if low_priority {
            BELOW_NORMAL_PRIORITY_CLASS
        } else {
            0
        };
        command.creation_flags(CREATE_NO_WINDOW | priority);
    }
    #[cfg(not(windows))]
    let _ = low_priority;
    command
}

/// Collects a child's stderr on another thread so a chatty process can't block on a full pipe.
pub fn drain_stderr(stderr: Option<ChildStderr>) -> JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut stderr) = stderr {
            stderr.read_to_string(&mut text).ok();
        }
        text
    })
}

#[derive(Clone, Debug, Default)]
pub struct Probe {
    pub duration: f64,
    pub codec: String,
    /// Bits per second.
    pub bit_rate: u32,
    pub channels: u32,
    pub sample_rate: u32,
    pub has_picture: bool,
    /// Container tags with lowercase keys.
    pub tags: HashMap<String, String>,
}

pub fn probe(tools: &Tools, path: &Path) -> Result<Probe> {
    let output = command(&tools.ffprobe, false)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()?;
    ensure!(
        output.status.success(),
        "ffprobe could not read {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let json: ProbeJson = serde_json::from_slice(&output.stdout)?;
    let audio = json
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("audio"))
        .with_context(|| format!("{} has no audio", path.display()))?;
    let number = |value: &Option<String>| value.as_deref().and_then(|v| v.parse::<f64>().ok());
    Ok(Probe {
        duration: number(&json.format.duration).unwrap_or(0.0),
        codec: audio.codec_name.clone().unwrap_or_default(),
        bit_rate: number(&audio.bit_rate)
            .or(number(&json.format.bit_rate))
            .unwrap_or(0.0) as u32,
        channels: audio.channels.unwrap_or(2),
        sample_rate: number(&audio.sample_rate).unwrap_or(44100.0) as u32,
        has_picture: json
            .streams
            .iter()
            .any(|s| s.codec_type.as_deref() == Some("video")),
        tags: json
            .format
            .tags
            .into_iter()
            .map(|(k, v)| (k.to_lowercase(), v))
            .collect(),
    })
}

#[derive(Deserialize)]
struct ProbeJson {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: ProbeFormat,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
    bit_rate: Option<String>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
    bit_rate: Option<String>,
    #[serde(default)]
    tags: HashMap<String, String>,
}

/// Returns the cover art embedded in an audio file.
pub fn extract_picture(tools: &Tools, path: &Path) -> Result<Vec<u8>> {
    let output = command(&tools.ffmpeg, false)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-c", "copy", "-f", "image2pipe", "-"])
        .output()?;
    ensure!(
        output.status.success() && !output.stdout.is_empty(),
        "No embedded picture"
    );
    Ok(output.stdout)
}
