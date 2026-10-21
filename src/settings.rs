use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::audible::Region;

const MAX_RECENT: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "Follow Windows",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub library_root: String,
    /// Marketplaces searched, in `Region::ALL` order.
    pub regions: Vec<Region>,
    /// Parallel encoders; 0 means one per logical CPU.
    pub workers: usize,
    /// Runs ffmpeg below normal priority so games and other apps stay smooth.
    pub low_priority: bool,
    pub appearance: Appearance,
    /// Recently opened book folders, newest first.
    pub recent: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            library_root: default_library(),
            regions: vec![Region::Us, Region::De],
            workers: 0,
            low_priority: true,
            appearance: Appearance::System,
            recent: Vec::new(),
        }
    }
}

impl Settings {
    pub fn worker_count(&self) -> usize {
        if self.workers == 0 {
            available_cpus()
        } else {
            self.workers
        }
    }

    pub fn remember(&mut self, dir: PathBuf) {
        self.recent.retain(|recent| *recent != dir);
        self.recent.insert(0, dir);
        self.recent.truncate(MAX_RECENT);
    }
}

pub fn available_cpus() -> usize {
    std::thread::available_parallelism().map_or(4, |count| count.get())
}

/// `Music\Audiobooks` in the user's folder.
fn default_library() -> String {
    dirs::audio_dir()
        .or_else(dirs::home_dir)
        .map(|dir| dir.join("Audiobooks").display().to_string())
        .unwrap_or_default()
}
