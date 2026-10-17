//! Kitty graphics protocol state (ADR-014):
//! <https://sw.kovidgoyal.net/kitty/graphics-protocol/>.
//!
//! [`GraphicsState`] handles one complete APC command at a time, at its
//! place in the output stream: [`GraphicsState::handle`] gets the cursor
//! there ([`Cursor`]) and returns the reply to send and how far to move the
//! cursor ([`Outcome`]). Images are decoded to RGBA (formats 24, 32 and
//! 100 = PNG, optionally zlib-compressed) and kept within byte budgets.
//! Placements are anchored to the grid line under the cursor with the
//! grid's scroll counters, as prompt markers are, so they scroll with the
//! text; [`GraphicsState::visible`] lists the ones in a viewport.
//!
//! Not implemented (answered with an error where a reply is due): shared
//! memory (`t=s`), animation (`a=f`, `a=a`, `a=c`), relative placements and
//! Unicode placeholders.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Arc;

use crate::graphics_filter::GraphicsEvent;

/// Largest decoded image (RGBA bytes).
pub const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
/// Total decoded bytes kept for all images.
pub const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;
/// Largest transfer (chunks, file or decompressed data) for one image.
pub const MAX_TRANSFER_BYTES: usize = 64 * 1024 * 1024;
/// Largest width or height (a texture must fit the GPU's 8192 limit).
pub const MAX_DIMENSION: u32 = 8192;
/// Most placements kept; the oldest goes first.
pub const MAX_PLACEMENTS: usize = 1024;
/// Images with no id from the application get ids from here up.
const INTERNAL_ID_BASE: u32 = 1 << 31;

/// A stored image.
#[derive(Debug, Clone)]
pub struct Image {
    pub id: u32,
    /// The application's image number (`I`), if it gave one.
    pub number: Option<u32>,
    pub width: u32,
    pub height: u32,
    /// RGBA, `width * height * 4` bytes, shared with snapshots.
    pub pixels: Arc<[u8]>,
    /// Distinguishes uploads under the same id (for texture caches).
    pub generation: u64,
}

/// Where a command arrived: the cursor and screen at that point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// The cursor's grid line (0 = top of the screen).
    pub line: i32,
    pub col: usize,
    /// The grid's `total_scrolled` and `total_pulled` counters.
    pub scrolled: u64,
    pub pulled: u64,
    pub columns: usize,
    pub lines: usize,
    /// The alternate screen is active.
    pub alt: bool,
    /// The cell size in pixels.
    pub cell_width: u32,
    pub cell_height: u32,
}

/// What handling a command produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The reply to send to the application, as bytes.
    pub reply: Option<Vec<u8>>,
    /// Move the cursor right by `.0` columns and down by `.1 - 1` rows.
    pub move_cursor: Option<(u32, u32)>,
}

/// An image placed on the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub image_id: u32,
    /// The application's placement id (`p`), 0 when none.
    pub placement_id: u32,
    line: i32,
    scrolled: u64,
    pulled: u64,
    pub col: usize,
    /// Pixel offset inside the first cell.
    pub offset: (u32, u32),
    /// Source rectangle in the image: x, y, width, height.
    pub source: [u32; 4],
    /// Drawn size in pixels at `cell` size; scales with the cell.
    pub size: (u32, u32),
    /// The cell size the placement was made at.
    pub cell: (u32, u32),
    pub columns: u32,
    pub rows: u32,
    pub z: i32,
    pub alt: bool,
    /// Placement order, for eviction and stable drawing.
    serial: u64,
}

impl Placement {
    /// The grid line of the placement's top row now.
    fn line_now(&self, scrolled: u64, pulled: u64) -> i64 {
        i64::from(self.line) - scrolled.saturating_sub(self.scrolled) as i64
            + pulled.saturating_sub(self.pulled) as i64
    }
}

/// A placement in a viewport, with what is needed to draw it.
#[derive(Debug, Clone)]
pub struct VisibleImage {
    pub image_id: u32,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u8]>,
    /// The viewport row of the placement's top (may be negative).
    pub row: i32,
    pub col: u32,
    pub offset: (u32, u32),
    pub source: [u32; 4],
    pub size: (u32, u32),
    pub cell: (u32, u32),
    pub z: i32,
}

/// A failure, as the protocol's error code and message.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Failure(&'static str, &'static str);

const EBADF: Failure = Failure("EBADF", "Failed to read image data");

/// The protocol state of one terminal.
#[derive(Debug, Default)]
pub struct GraphicsState {
    images: BTreeMap<u32, Image>,
    total_bytes: usize,
    placements: Vec<Placement>,
    /// The first chunk of a chunked transfer, with the data so far.
    chunk: Option<GraphicsEvent>,
    chunk_overflow: bool,
    next_internal: u32,
    generation: u64,
    serial: u64,
    /// The cell size in pixels, for sizing placements.
    cell: (u32, u32),
    /// Bumped by every change that alters what is drawn.
    version: u64,
}

/// A command's control keys, with the protocol's defaults.
struct Keys<'a>(&'a GraphicsEvent);

impl Keys<'_> {
    fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key)
    }
    fn char(&self, key: &str, default: char) -> char {
        self.str(key)
            .and_then(|v| v.chars().next())
            .unwrap_or(default)
    }
    fn u32(&self, key: &str) -> u32 {
        self.0.get_u32(key).unwrap_or(0)
    }
    fn i32(&self, key: &str) -> i32 {
        self.0.get_i32(key).unwrap_or(0)
    }
}

impl GraphicsState {
    pub fn new() -> Self {
        Self {
            cell: (8, 16),
            ..Self::default()
        }
    }

    /// The cell size in pixels (the window's, updated on zoom).
    pub fn cell_size(&self) -> (u32, u32) {
        self.cell
    }

    pub fn set_cell_size(&mut self, width: u32, height: u32) {
        self.cell = (width.max(1), height.max(1));
    }

    /// A counter that changes whenever the drawn images may have changed.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Handle one APC graphics command arriving with the cursor at `at`.
    pub fn handle(&mut self, event: &GraphicsEvent, at: &Cursor) -> Outcome {
        if event.command != b'G' {
            return Outcome::default();
        }
        // Chunking: the first chunk carries the keys; later ones only `m`
        // (and perhaps `q`) and data. A delete aborts a transfer.
        let more = event.get("m") == Some("1");
        let keys = Keys(event);
        if keys.char("a", 't') == 'd' && self.chunk.is_some() && event.get("m").is_none() {
            self.chunk = None;
        }
        if let Some(chunk) = self.chunk.as_mut() {
            if chunk.data.len() + event.data.len() > MAX_TRANSFER_BYTES {
                self.chunk_overflow = true;
            } else if !self.chunk_overflow {
                chunk.data.extend_from_slice(&event.data);
            }
            if more {
                return Outcome::default();
            }
            let complete = self.chunk.take().expect("chunk in progress");
            let overflow = std::mem::take(&mut self.chunk_overflow);
            return self.complete(&complete, at, overflow);
        }
        if more {
            self.chunk = Some(event.clone());
            self.chunk_overflow = event.data.len() > MAX_TRANSFER_BYTES;
            return Outcome::default();
        }
        self.complete(event, at, false)
    }

    fn complete(&mut self, event: &GraphicsEvent, at: &Cursor, overflow: bool) -> Outcome {
        self.version += 1;
        let keys = Keys(event);
        let action = keys.char("a", 't');
        let id = keys.u32("i");
        let number = keys.u32("I");
        let placement_id = keys.u32("p");
        let quiet = keys.u32("q");
        let mut outcome = Outcome::default();
        let result: Result<u32, Failure> = if id != 0 && number != 0 {
            Err(Failure("EINVAL", "Image id and number are both given"))
        } else {
            match action {
                't' | 'T' | 'q' => {
                    let decoded = if overflow {
                        Err(Failure("EFBIG", "Image data too large"))
                    } else {
                        decode(&keys)
                    };
                    match (action, decoded) {
                        (_, Err(failure)) => Err(failure),
                        ('q', Ok(_)) => Ok(id),
                        (_, Ok((width, height, pixels))) => {
                            let stored = self.store(id, number, width, height, pixels);
                            if action == 'T' {
                                outcome.move_cursor = self.place(stored, placement_id, &keys, at);
                            }
                            Ok(stored)
                        }
                    }
                }
                'p' => {
                    let target = if number != 0 {
                        self.newest_with_number(number)
                    } else {
                        self.images.contains_key(&id).then_some(id)
                    };
                    match target {
                        Some(image) => {
                            outcome.move_cursor = self.place(image, placement_id, &keys, at);
                            Ok(image)
                        }
                        None => Err(Failure("ENOENT", "No such image")),
                    }
                }
                'd' => {
                    self.delete(&keys, at);
                    return Outcome::default();
                }
                _ => Err(Failure("EINVAL", "Unsupported action")),
            }
        };
        // Replies only for commands that name an image (by id or number).
        if id == 0 && number == 0 {
            return outcome;
        }
        let mut head = String::new();
        match &result {
            Ok(image) if number != 0 => head.push_str(&format!("i={image},I={number}")),
            _ if number != 0 => head.push_str(&format!("I={number}")),
            _ => head.push_str(&format!("i={id}")),
        }
        if placement_id != 0 && action != 't' && action != 'q' {
            head.push_str(&format!(",p={placement_id}"));
        }
        let message = match &result {
            Ok(_) if quiet >= 1 => return outcome,
            Ok(_) => "OK".to_string(),
            Err(_) if quiet >= 2 => return outcome,
            Err(Failure(code, text)) => format!("{code}:{text}"),
        };
        outcome.reply = Some(format!("\x1b_G{head};{message}\x1b\\").into_bytes());
        outcome
    }

    /// Store an image, replacing one with the same id (and its
    /// placements), evicting others to stay within the budget.
    fn store(&mut self, id: u32, number: u32, width: u32, height: u32, pixels: Vec<u8>) -> u32 {
        let id = if id != 0 { id } else { self.internal_id() };
        if let Some(old) = self.images.remove(&id) {
            self.total_bytes -= old.pixels.len();
            self.placements.retain(|p| p.image_id != id);
        }
        let bytes = pixels.len();
        while self.total_bytes + bytes > MAX_TOTAL_BYTES {
            // Images without placements go first, then the oldest.
            let victim = self
                .images
                .values()
                .filter(|image| !self.placements.iter().any(|p| p.image_id == image.id))
                .min_by_key(|image| image.generation)
                .or_else(|| self.images.values().min_by_key(|image| image.generation))
                .map(|image| image.id);
            let Some(victim) = victim else { break };
            if let Some(old) = self.images.remove(&victim) {
                self.total_bytes -= old.pixels.len();
            }
            self.placements.retain(|p| p.image_id != victim);
        }
        self.generation += 1;
        self.total_bytes += bytes;
        self.images.insert(
            id,
            Image {
                id,
                number: (number != 0).then_some(number),
                width,
                height,
                pixels: pixels.into(),
                generation: self.generation,
            },
        );
        id
    }

    fn internal_id(&mut self) -> u32 {
        loop {
            self.next_internal = self.next_internal.wrapping_add(1);
            let id = INTERNAL_ID_BASE | (self.next_internal & !INTERNAL_ID_BASE);
            if id != 0 && !self.images.contains_key(&id) {
                return id;
            }
        }
    }

    fn newest_with_number(&self, number: u32) -> Option<u32> {
        self.images
            .values()
            .filter(|image| image.number == Some(number))
            .max_by_key(|image| image.generation)
            .map(|image| image.id)
    }

    /// Place image `id` at the cursor; returns the cursor movement unless
    /// `C=1`.
    fn place(
        &mut self,
        id: u32,
        placement_id: u32,
        keys: &Keys,
        at: &Cursor,
    ) -> Option<(u32, u32)> {
        let image = self.images.get(&id)?;
        let (cw, ch) = (at.cell_width.max(1), at.cell_height.max(1));
        let sx = keys.u32("x").min(image.width);
        let sy = keys.u32("y").min(image.height);
        let sw = match keys.u32("w") {
            0 => image.width - sx,
            w => w.min(image.width - sx),
        };
        let sh = match keys.u32("h") {
            0 => image.height - sy,
            h => h.min(image.height - sy),
        };
        let (c, r) = (keys.u32("c"), keys.u32("r"));
        let ratio = |a: u32, b: u32, n: u64| -> u32 {
            if b == 0 {
                0
            } else {
                (n * u64::from(a) / u64::from(b)) as u32
            }
        };
        let size = match (c, r) {
            (0, 0) => (sw, sh),
            (c, 0) => (c * cw, ratio(sh, sw, u64::from(c * cw))),
            (0, r) => (ratio(sw, sh, u64::from(r * ch)), r * ch),
            (c, r) => (c * cw, r * ch),
        };
        let offset = (keys.u32("X").min(cw - 1), keys.u32("Y").min(ch - 1));
        let columns = (offset.0 + size.0).div_ceil(cw).max(1);
        let rows = (offset.1 + size.1).div_ceil(ch).max(1);
        if placement_id != 0 {
            self.placements
                .retain(|p| !(p.image_id == id && p.placement_id == placement_id));
        }
        if self.placements.len() >= MAX_PLACEMENTS
            && let Some(oldest) = self
                .placements
                .iter()
                .enumerate()
                .min_by_key(|(_, p)| p.serial)
                .map(|(i, _)| i)
        {
            self.placements.remove(oldest);
        }
        self.serial += 1;
        self.placements.push(Placement {
            image_id: id,
            placement_id,
            line: at.line,
            scrolled: at.scrolled,
            pulled: at.pulled,
            col: at.col,
            offset,
            source: [sx, sy, sw, sh],
            size,
            cell: (cw, ch),
            columns,
            rows,
            z: keys.i32("z"),
            alt: at.alt,
            serial: self.serial,
        });
        (keys.u32("C") != 1).then_some((columns, rows))
    }

    /// `a=d`: delete placements (lowercase) or placements and their now
    /// unused images (uppercase). Cell coordinates are 1-based.
    fn delete(&mut self, keys: &Keys, at: &Cursor) {
        let what = keys.char("d", 'a');
        let free = what.is_ascii_uppercase();
        let (x, y) = (keys.u32("x"), keys.u32("y"));
        let z = keys.i32("z");
        let id = keys.u32("i");
        let number = keys.u32("I");
        let pid = keys.u32("p");
        let screen_lines = at.lines as i64;
        let covers_col = |p: &Placement, col: u32| {
            col >= 1 && (p.col as u32) < col && col <= p.col as u32 + p.columns
        };
        let covers_row = |p: &Placement, row: u32| {
            let top = p.line_now(at.scrolled, at.pulled);
            let row = i64::from(row) - 1;
            row >= top && row < top + i64::from(p.rows)
        };
        let target_image = match what.to_ascii_lowercase() {
            'n' => self.newest_with_number(number),
            _ => Some(id),
        };
        let doomed = |p: &Placement| -> bool {
            if p.alt != at.alt {
                return false;
            }
            match what.to_ascii_lowercase() {
                'a' => {
                    let top = p.line_now(at.scrolled, at.pulled);
                    top + i64::from(p.rows) > 0 && top < screen_lines
                }
                'i' | 'n' => {
                    Some(p.image_id) == target_image && (pid == 0 || p.placement_id == pid)
                }
                'c' => covers_col(p, at.col as u32 + 1) && covers_row(p, (at.line + 1) as u32),
                'p' => covers_col(p, x) && covers_row(p, y),
                'q' => covers_col(p, x) && covers_row(p, y) && p.z == z,
                'x' => covers_col(p, x),
                'y' => covers_row(p, y),
                'z' => p.z == z,
                'r' => p.image_id >= x && p.image_id <= y,
                _ => false,
            }
        };
        let removed: Vec<u32> = self
            .placements
            .iter()
            .filter(|p| doomed(p))
            .map(|p| p.image_id)
            .collect();
        self.placements.retain(|p| !doomed(p));
        if free {
            let mut candidates = removed;
            if matches!(what, 'I' | 'N')
                && let Some(image) = target_image
            {
                candidates.push(image);
            }
            if what == 'R' {
                candidates.extend(self.images.range(x..=y.max(x)).map(|(id, _)| *id));
            }
            for image in candidates {
                if !self.placements.iter().any(|p| p.image_id == image)
                    && let Some(old) = self.images.remove(&image)
                {
                    self.total_bytes -= old.pixels.len();
                }
            }
        }
    }

    /// `CSI 2 J`: delete the placements on the current screen.
    pub fn clear_screen(&mut self, at: &Cursor) {
        self.version += 1;
        let lines = at.lines as i64;
        self.placements.retain(|p| {
            let top = p.line_now(at.scrolled, at.pulled);
            p.alt != at.alt || !(top + i64::from(p.rows) > 0 && top < lines)
        });
    }

    /// `CSI 3 J`: delete the placements in the scrollback.
    pub fn clear_scrollback(&mut self, at: &Cursor) {
        self.version += 1;
        self.placements
            .retain(|p| p.alt || p.line_now(at.scrolled, at.pulled) + i64::from(p.rows) > 0);
    }

    /// `ESC c`: delete every placement and image.
    pub fn reset(&mut self) {
        self.version += 1;
        self.placements.clear();
        self.images.clear();
        self.total_bytes = 0;
        self.chunk = None;
    }

    /// Drop the alternate screen's placements (it was left).
    pub fn leave_alternate_screen(&mut self) {
        let before = self.placements.len();
        self.placements.retain(|p| !p.alt);
        if self.placements.len() != before {
            self.version += 1;
        }
    }

    /// The placements visible in a viewport `display_offset` lines into
    /// the history, lowest z first; drops those scrolled out of a history
    /// of `history` lines.
    pub fn visible(
        &mut self,
        scrolled: u64,
        pulled: u64,
        display_offset: usize,
        lines: usize,
        history: usize,
        alt: bool,
    ) -> Vec<VisibleImage> {
        let oldest = -(history as i64);
        self.placements
            .retain(|p| p.line_now(scrolled, pulled) + i64::from(p.rows) > oldest);
        let mut shown: Vec<(&Placement, i64)> = self
            .placements
            .iter()
            .filter(|p| p.alt == alt)
            .map(|p| (p, p.line_now(scrolled, pulled) + display_offset as i64))
            .filter(|(p, row)| *row < lines as i64 && row + i64::from(p.rows) > 0)
            .collect();
        shown.sort_by_key(|(p, _)| (p.z, p.image_id, p.serial));
        shown
            .into_iter()
            .filter_map(|(p, row)| {
                let image = self.images.get(&p.image_id)?;
                Some(VisibleImage {
                    image_id: image.id,
                    generation: image.generation,
                    width: image.width,
                    height: image.height,
                    pixels: Arc::clone(&image.pixels),
                    row: row as i32,
                    col: p.col as u32,
                    offset: p.offset,
                    source: p.source,
                    size: p.size,
                    cell: p.cell,
                    z: p.z,
                })
            })
            .collect()
    }

    /// The stored image `id`.
    pub fn image(&self, id: u32) -> Option<&Image> {
        self.images.get(&id)
    }

    /// Number of stored images.
    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// Decoded bytes held for all images.
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// The placements, oldest first.
    pub fn placements(&self) -> impl Iterator<Item = &Placement> {
        self.placements.iter()
    }
}

/// Read, decompress and decode a transmission to RGBA.
fn decode(keys: &Keys) -> Result<(u32, u32, Vec<u8>), Failure> {
    let medium = keys.char("t", 'd');
    let file_medium = matches!(medium, 'f' | 't');
    let raw = match medium {
        'd' => keys.0.data.clone(),
        'f' | 't' => read_file(keys, medium == 't')?,
        's' => return Err(Failure("EINVAL", "Shared memory is not supported")),
        _ => return Err(Failure("EINVAL", "Unknown transmission medium")),
    };
    let decoded = decode_bytes(keys, raw);
    // A file's contents must not be probed through the error: every
    // failure with a file medium is the same EBADF.
    if file_medium {
        decoded.map_err(|_| EBADF)
    } else {
        decoded
    }
}

fn decode_bytes(keys: &Keys, raw: Vec<u8>) -> Result<(u32, u32, Vec<u8>), Failure> {
    let data = if keys.str("o") == Some("z") {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(raw.as_slice())
            .take(MAX_TRANSFER_BYTES as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|_| Failure("EINVAL", "Bad zlib data"))?;
        if out.len() > MAX_TRANSFER_BYTES {
            return Err(Failure("EFBIG", "Image data too large"));
        }
        out
    } else {
        raw
    };
    match keys.u32("f") {
        0 | 32 => raw_pixels(keys, data, 4),
        24 => raw_pixels(keys, data, 3),
        100 => png_pixels(&data),
        _ => Err(Failure("EINVAL", "Unknown format")),
    }
}

fn check_size(width: u32, height: u32) -> Result<(), Failure> {
    if width == 0 || height == 0 {
        return Err(Failure("EINVAL", "Zero width or height"));
    }
    if width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || width as usize * height as usize * 4 > MAX_IMAGE_BYTES
    {
        return Err(Failure("EFBIG", "Image too large"));
    }
    Ok(())
}

fn raw_pixels(keys: &Keys, data: Vec<u8>, channels: usize) -> Result<(u32, u32, Vec<u8>), Failure> {
    let (width, height) = (keys.u32("s"), keys.u32("v"));
    check_size(width, height)?;
    let count = width as usize * height as usize;
    if data.len() < count * channels {
        return Err(Failure("ENODATA", "Insufficient image data"));
    }
    let pixels = if channels == 4 {
        let mut data = data;
        data.truncate(count * 4);
        data
    } else {
        let mut out = Vec::with_capacity(count * 4);
        for rgb in data[..count * 3].as_chunks::<3>().0 {
            out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        out
    };
    Ok((width, height, pixels))
}

fn png_pixels(data: &[u8]) -> Result<(u32, u32, Vec<u8>), Failure> {
    let bad = |_| Failure("EBADPNG", "Failed to decode PNG");
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    decoder.set_limits(png::Limits {
        bytes: MAX_IMAGE_BYTES,
    });
    let mut reader = decoder.read_info().map_err(bad)?;
    let (width, height) = reader.info().size();
    check_size(width, height)?;
    let size = reader
        .output_buffer_size()
        .ok_or(Failure("EFBIG", "Image too large"))?;
    let mut buffer = vec![0; size];
    let info = reader.next_frame(&mut buffer).map_err(bad)?;
    let bytes = &buffer[..info.buffer_size()];
    let count = width as usize * height as usize;
    let mut out = Vec::with_capacity(count * 4);
    match info.color_type {
        png::ColorType::Rgba => out.extend_from_slice(bytes),
        png::ColorType::Rgb => {
            for p in bytes.as_chunks::<3>().0 {
                out.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for p in bytes.as_chunks::<2>().0 {
                out.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
        }
        png::ColorType::Grayscale => {
            for &g in bytes {
                out.extend_from_slice(&[g, g, g, 255]);
            }
        }
        png::ColorType::Indexed => return Err(Failure("EBADPNG", "Failed to decode PNG")),
    }
    if out.len() != count * 4 {
        return Err(Failure("EBADPNG", "Failed to decode PNG"));
    }
    Ok((width, height, out))
}

/// Read a file named by the payload (`t=f`, or `t=t` which is removed
/// afterwards when it is a temporary file of this protocol). Regular files
/// only; `/proc`, `/sys` and `/dev` are refused. Every failure is EBADF.
fn read_file(keys: &Keys, temporary: bool) -> Result<Vec<u8>, Failure> {
    let path = std::str::from_utf8(&keys.0.data).map_err(|_| EBADF)?;
    let path = std::fs::canonicalize(path).map_err(|_| EBADF)?;
    if ["/proc", "/sys", "/dev"]
        .iter()
        .any(|p| path.starts_with(p))
    {
        return Err(EBADF);
    }
    let file = std::fs::File::open(&path).map_err(|_| EBADF)?;
    let meta = file.metadata().map_err(|_| EBADF)?;
    if !meta.is_file() {
        return Err(EBADF);
    }
    let offset = u64::from(keys.u32("O"));
    let size = match keys.u32("S") {
        0 => meta.len().saturating_sub(offset),
        s => u64::from(s),
    };
    if size > MAX_TRANSFER_BYTES as u64 {
        return Err(EBADF);
    }
    let mut data = Vec::with_capacity(size as usize);
    let mut reader = std::io::BufReader::new(file);
    std::io::copy(&mut (&mut reader).take(offset), &mut std::io::sink()).map_err(|_| EBADF)?;
    reader
        .take(size)
        .read_to_end(&mut data)
        .map_err(|_| EBADF)?;
    if temporary && is_protocol_temp_file(&path) {
        let _ = std::fs::remove_file(&path);
    }
    Ok(data)
}

/// A `t=t` file is deleted only inside a temporary directory and only if
/// its path says it belongs to this protocol.
fn is_protocol_temp_file(path: &std::path::Path) -> bool {
    let mut dirs = vec![
        std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from("/dev/shm"),
    ];
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        dirs.push(std::fs::canonicalize(&tmp).unwrap_or_else(|_| tmp.into()));
    }
    dirs.iter().any(|dir| path.starts_with(dir))
        && path.to_string_lossy().contains("tty-graphics-protocol")
}
