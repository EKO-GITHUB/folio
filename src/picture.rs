use std::{
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// An encoded cover image, shareable between the UI and worker threads.
#[derive(Clone)]
pub struct Picture {
    pub bytes: Arc<[u8]>,
    /// Unique `bytes://` URI so egui caches each picture separately.
    pub uri: String,
    pub size: Option<(u32, u32)>,
}

impl Picture {
    pub fn new(bytes: Vec<u8>) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let size = image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .ok()
            .and_then(|reader| reader.into_dimensions().ok());
        let ext = if bytes.starts_with(b"\x89PNG") {
            "png"
        } else {
            "jpg"
        };
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            bytes: bytes.into(),
            uri: format!("bytes://picture-{id}.{ext}"),
            size,
        }
    }

    pub fn is_png(&self) -> bool {
        self.bytes.starts_with(b"\x89PNG")
    }

    pub fn extension(&self) -> &'static str {
        if self.is_png() { "png" } else { "jpg" }
    }

    pub fn describe_size(&self) -> String {
        self.size
            .map_or_else(|| "unknown size".to_owned(), |(w, h)| format!("{w}×{h}"))
    }
}
