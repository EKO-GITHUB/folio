# Folio

Folio is a Windows desktop program that turns a folder of audiobook files into a single M4B file with chapters, cover art and tags, and saves it into an audiobook library.

It reads the audio files in a folder along with an optional NFO file and cover image. It looks the book up on Audible, lets you review the details and chapters, encodes everything to AAC using all CPU threads, and copies the result into your library.

## Requirements

- Windows 10 or 11.
- ffmpeg and ffprobe. Folio uses them from `PATH` or from the folder that contains `folio.exe`. If neither has them, Folio downloads a build to `%LOCALAPPDATA%\folio\ffmpeg` on first start.
- An internet connection for Audible lookups. Without one you can still enter everything by hand.

## Building

Install Rust, then run:

```
cargo ship
```

This is an alias for `cargo build --release`. The program is `target\release\folio.exe`. It only needs the DLLs that come with Windows, so you can copy it anywhere.

The window icon is embedded with the `winresource` crate. That needs `windres` for the GNU toolchain or `rc.exe` for the MSVC toolchain.

## Using it

1. Start `folio.exe` and open a book folder in any of these ways:
   - Click **Open folder** or press Ctrl+O.
   - Drop the folder onto the window.
   - Drop the folder onto `folio.exe`.
2. Folio reads the files and looks the book up on Audible.
   - If an ASIN is found in the file tags, the NFO or the folder name, the book is fetched directly.
   - Otherwise Folio searches by title and author. Open the **Audible** tab to change the search or pick a different result.
   - Results that are the same length as your files are marked as such.
3. Check the **Details** tab.
   - Authors and narrators are comma separated.
   - The first author decides the library folder.
   - If Audible lists the book in several series, you can switch series there.
4. Check the **Chapters** tab. Chapters come from, in order of preference:
   - Audible's chapter timings, when the total length matches your files within 5 seconds.
   - Audible's chapter titles placed at file boundaries, when the number of files equals the number of chapters.
   - One chapter per file.

   Start times use the format `h:mm:ss.mmm`. Rows can be edited, inserted and removed.
5. Choose the AAC bitrate. The default is about three quarters of the source MP3 bitrate.
6. Click **Build m4b** or press Ctrl+Enter.

The **Source** tab shows the files that were found, their format and the NFO text.

## Output

Each book is written to:

```
<Library>\<Author>\[<Series>\]<Year> - <Title>[ <Series> <Number>]\
    <Title> (<Year>)[ <Series> <Number>].m4b
    <Title> (<Year>)[ <Series> <Number>] - cover.jpg
    desc.txt
    reader.txt
```

The parts in brackets are left out for books that are not part of a series. Characters that Windows does not allow in file names are removed.

The M4B contains:
- AAC audio.
- Chapters, both as a Nero chapter list and as a QuickTime chapter track.
- The cover.
- Standard iTunes tags: title, album, artist, album artist, composer (the narrators), year, genre, description, copyright, grouping and sort order.
- Freeform tags: `ASIN`, `SERIES`, `SERIES-PART`, `SUBTITLE`, `RELEASETIME`, `PUBLISHER`, `LANGUAGE`, `FORMAT`, `EXPLICIT` and `WWWAUDIOFILE`.

The book is built in the temp folder and only copied into the library once it is complete. Existing files with the same names are replaced only if you tick **Replace**. The source folder is never changed.

## Settings

Open them with the gear icon in the top right corner.

- **Library folder**: where books are saved. The default is `Music\Audiobooks` in your user folder, which is created on the first build.
- **Audible marketplaces**: which Audible stores to search. The default is US and DE.
- **Encoding**: the number of parallel encoders (default: one per CPU thread), and whether they run at low priority (default: on, so other programs stay responsive).
- **Appearance**: follow Windows, light or dark.

Settings and recently opened folders are stored in `%APPDATA%\Folio\data\app.ron`.

## How encoding works

1. Every file is decoded once to measure its exact length in samples.
2. The book is split into one segment per encoder, each cut at a quiet point.
3. Each segment is encoded by its own ffmpeg process. Each process also encodes a little audio before and after its segment, and only the AAC frames that belong to the segment are kept. This makes the joins seamless.
4. The frames are written into the M4B by Folio itself rather than by ffmpeg, which is much faster for long books.
5. The file is copied into the library with its index moved to the front, so players can start without reading the end of the file.

## Development

```
cargo test
```

This runs the unit tests.

```
cargo test preview -- --ignored
```

This renders the main screens to `target\ui-preview\*.png` without opening a window.

```
set FOLIO_SAMPLE=C:\path\to\a\folder\of\mp3s
cargo test --release joins_are_seamless -- --ignored --nocapture
```

This encodes the first six files of the folder with one encoder and again with 32 in parallel, then compares both results against the source audio at every join.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Folio by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

Folio downloads ffmpeg from [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) when it can't find it. ffmpeg is a separate program under its own license and is not part of this repository.
