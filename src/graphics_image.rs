//! Bounded inline image upload and storage (N33a, N34).
//!
//! [`ImageStore`] stores uploaded graphics images with strict size
//! validation, checked allocation arithmetic, and per-image and total byte
//! budgets. It advertises only the capabilities it supports (the `f=24`
//! RGB and `f=32` RGBA pixel formats, stored internally as RGBA).
//! [`GraphicsHandler`] processes the real kitty-graphics protocol
//! (`G` + `key=value` control data, per ADR-011) against the image and
//! placement stores and generates protocol replies.

use std::collections::HashMap;

use crate::graphics_filter::GraphicsEvent;

/// Maximum dimension (width or height) for a single image.
pub const MAX_DIMENSION: u32 = 8192;
/// Per-image byte budget (32 MiB).
pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
/// Total byte budget across all images (128 MiB).
pub const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;
/// Maximum number of stored image IDs.
pub const MAX_IMAGE_IDS: usize = 1024;

/// An upload error (protocol-appropriate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadError {
    /// The params string is malformed (wrong number of fields or invalid values).
    MalformedParams,
    /// The requested pixel format is not supported.
    UnsupportedFormat,
    /// The image dimensions exceed the maximum.
    DimensionTooLarge,
    /// The pixel data length does not match the expected size.
    DataLengthMismatch,
    /// The per-image byte budget would be exceeded.
    PerImageBudget,
    /// The total byte budget would be exceeded.
    TotalBudget,
    /// The maximum number of image IDs would be exceeded.
    TooManyIds,
}

/// A decoded image (RGBA, 4 bytes per pixel).
#[derive(Debug, Clone)]
pub struct Image {
    /// The image width in pixels.
    pub width: u32,
    /// The image height in pixels.
    pub height: u32,
    /// The RGBA pixel data (width * height * 4 bytes).
    pub pixels: Vec<u8>,
}

impl Image {
    /// The byte cost of this image (the pixel data length).
    pub fn byte_cost(&self) -> usize {
        self.pixels.len()
    }
}

/// Bytes per source pixel for each supported kitty pixel format.
///
/// The real protocol uses `f=24` (8-bit RGB) and `f=32` (8-bit RGBA, the
/// default); `f=100` (PNG) is a later slice. Returns `None` for unsupported
/// formats.
fn bytes_per_pixel(format: u32) -> Option<usize> {
    Some(match format {
        24 => 3, // 8-bit-per-channel RGB (f=24)
        32 => 4, // 8-bit-per-channel RGBA (f=32)
        _ => return None,
    })
}

/// Convert pixel data from a supported kitty format to the internal RGBA
/// representation (4 bytes per pixel).
///
/// `data` must be exactly `width * height * bytes_per_pixel(format)` bytes;
/// a mismatch is a `DataLengthMismatch` error. An unsupported `format` is an
/// `UnsupportedFormat` error. The colorspace field is ignored (all source
/// values are used as-is; linear/scRGB conversion is a later concern).
pub fn convert_to_rgba(
    format: u32,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<Vec<u8>, UploadError> {
    let bpp = bytes_per_pixel(format).ok_or(UploadError::UnsupportedFormat)?;
    // A zero dimension is degenerate; reject it (consistent with upload_rgba).
    if width == 0 || height == 0 {
        return Err(UploadError::DimensionTooLarge);
    }
    let expected = (width as u64)
        .checked_mul(height as u64)
        .and_then(|v| v.checked_mul(bpp as u64))
        .and_then(|v| u32::try_from(v).ok())
        .ok_or(UploadError::DimensionTooLarge)?;
    if data.len() != expected as usize {
        return Err(UploadError::DataLengthMismatch);
    }
    let pixels = width as usize * height as usize;
    let mut out = Vec::with_capacity(pixels * 4);
    match format {
        32 => out.extend_from_slice(data),
        24 => {
            // RGB: append alpha = 255.
            for chunk in data.chunks_exact(3) {
                out.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
        }
        _ => return Err(UploadError::UnsupportedFormat),
    }
    Ok(out)
}

/// The capabilities the graphics implementation advertises.
///
/// Only what is actually supported is listed; unsupported capabilities are
/// not advertised as full protocol support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphicsCapabilities {
    /// Supported pixel format `f` values (`24` RGB, `32` RGBA; stored
    /// internally as RGBA).
    pub supported_formats: &'static [u32],
    /// Maximum dimension (width or height) for a single image.
    pub max_dimension: u32,
    /// Per-image byte budget.
    pub max_image_bytes: usize,
    /// Total byte budget across all images.
    pub max_total_bytes: usize,
    /// Maximum number of stored image IDs.
    pub max_image_ids: usize,
}

/// A bounded image cache (per-image and total budgets).
#[derive(Debug)]
pub struct ImageStore {
    images: HashMap<u32, Image>,
    total_bytes: usize,
    next_id: u32,
}

impl Default for ImageStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageStore {
    /// Create an empty store.
    ///
    /// Image IDs are auto-assigned starting at 1 (0 is reserved for the
    /// "clear all" delete sentinel).
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            total_bytes: 0,
            next_id: 1,
        }
    }

    /// Store an image from decoded RGBA pixel data.
    ///
    /// `rgba` is the pixel data (4 bytes per pixel, `width * height * 4`
    /// bytes) already converted to the internal RGBA representation (see
    /// [`convert_to_rgba`]). `image_id` is the requested image ID, or `None`
    /// (or `Some(0)`) to auto-assign.
    ///
    /// Returns the image ID on success, or an error on failure.
    pub fn upload_rgba(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
        image_id: Option<u32>,
    ) -> Result<u32, UploadError> {
        // Validate dimensions (non-zero, within max).
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(UploadError::DimensionTooLarge);
        }
        // Enforce the per-image byte budget (on the stored RGBA data).
        if rgba.len() > MAX_IMAGE_BYTES {
            return Err(UploadError::PerImageBudget);
        }
        // Enforce the total byte budget.
        if self.total_bytes + rgba.len() > MAX_TOTAL_BYTES {
            return Err(UploadError::TotalBudget);
        }
        // Determine the image ID (auto-assign if absent or 0).
        let image_id = match image_id {
            Some(0) | None => {
                if self.images.len() >= MAX_IMAGE_IDS {
                    return Err(UploadError::TooManyIds);
                }
                // IDs are 1-based (0 is the auto-assign / clear-all sentinel).
                let new_id = self.next_id;
                self.next_id = self.next_id.wrapping_add(1);
                new_id
            }
            Some(id) => {
                // Explicit ID: must not already be in use.
                if self.images.contains_key(&id) {
                    return Err(UploadError::MalformedParams);
                }
                if self.images.len() >= MAX_IMAGE_IDS {
                    return Err(UploadError::TooManyIds);
                }
                id
            }
        };
        // Store the image.
        let image = Image {
            width,
            height,
            pixels: rgba.to_vec(),
        };
        self.total_bytes += image.byte_cost();
        self.images.insert(image_id, image);
        Ok(image_id)
    }

    /// The number of stored images.
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// Query an image by ID.
    pub fn query(&self, id: u32) -> Option<&Image> {
        self.images.get(&id)
    }

    /// Delete an image by ID, returning it if present.
    pub fn delete(&mut self, id: u32) -> Option<Image> {
        let image = self.images.remove(&id)?;
        self.total_bytes -= image.byte_cost();
        Some(image)
    }

    /// Remove all images, returning the number removed.
    pub fn clear(&mut self) -> usize {
        let n = self.images.len();
        self.total_bytes = 0;
        self.images.clear();
        n
    }

    /// The total byte cost of all images.
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// The capabilities this implementation advertises (the supported pixel
    /// formats: `f=24` RGB and `f=32` RGBA; `f=100` PNG is a later slice).
    pub fn capabilities() -> GraphicsCapabilities {
        GraphicsCapabilities {
            supported_formats: &[24, 32],
            max_dimension: MAX_DIMENSION,
            max_image_bytes: MAX_IMAGE_BYTES,
            max_total_bytes: MAX_TOTAL_BYTES,
            max_image_ids: MAX_IMAGE_IDS,
        }
    }
}

/// An error from placing an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementError {
    /// The placement count would exceed the maximum.
    TooManyPlacements,
    /// The placement dimensions are invalid (zero).
    InvalidDimensions,
}

/// A placement: where an image is drawn on the screen (cell coordinates).
///
/// Placements are maintained separately from image IDs: a placement
/// references an image, but deleting a placement does not delete the
/// image, and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// The placement ID (unique among placements).
    pub id: u32,
    /// The image ID this placement references.
    pub image_id: u32,
    /// The x position in columns (0-based, may be negative if offscreen).
    pub x: i32,
    /// The y position in rows (0-based, may be negative if offscreen).
    pub y: i32,
    /// The width in columns.
    pub columns: u32,
    /// The height in rows.
    pub rows: u32,
}

/// The maximum number of placements.
pub const MAX_PLACEMENTS: usize = 1024;

/// A bounded store of placements (separate from image IDs).
#[derive(Debug)]
pub struct PlacementStore {
    placements: HashMap<u32, Placement>,
    next_id: u32,
}

impl Default for PlacementStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PlacementStore {
    /// Create an empty store.
    ///
    /// Placement IDs are auto-assigned starting at 1 (0 is reserved for the
    /// "clear all" delete sentinel).
    pub fn new() -> Self {
        Self {
            placements: HashMap::new(),
            next_id: 1,
        }
    }

    /// Place an image at (x, y) with the given size (in cells).
    ///
    /// The caller must ensure the image exists (the placement store does
    /// not validate image IDs — images and placements are maintained
    /// separately).
    ///
    /// Returns the placement ID on success.
    pub fn place(
        &mut self,
        image_id: u32,
        x: i32,
        y: i32,
        columns: u32,
        rows: u32,
    ) -> Result<u32, PlacementError> {
        if columns == 0 || rows == 0 {
            return Err(PlacementError::InvalidDimensions);
        }
        if self.placements.len() >= MAX_PLACEMENTS {
            return Err(PlacementError::TooManyPlacements);
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.placements.insert(
            id,
            Placement {
                id,
                image_id,
                x,
                y,
                columns,
                rows,
            },
        );
        Ok(id)
    }

    /// Delete a placement by ID, returning it if present.
    pub fn delete(&mut self, id: u32) -> Option<Placement> {
        self.placements.remove(&id)
    }

    /// Scroll all placements up by `n` rows (positive = up).
    ///
    /// Placements entirely above the viewport (y + rows <= 0) are deleted.
    /// Placements partially visible are kept (clipped when drawn).
    pub fn scroll(&mut self, n: i32) {
        let mut to_remove = Vec::new();
        for p in self.placements.values_mut() {
            p.y -= n;
            if p.y + p.rows as i32 <= 0 {
                to_remove.push(p.id);
            }
        }
        for id in to_remove {
            self.placements.remove(&id);
        }
    }

    /// Clear all placements (alternate screen policy).
    ///
    /// Switching to the alternate screen clears all placements. Switching
    /// back does not restore them (documented policy).
    pub fn clear(&mut self) {
        self.placements.clear();
    }

    /// Get a placement by ID.
    pub fn get(&self, id: u32) -> Option<&Placement> {
        self.placements.get(&id)
    }

    /// All placements (for drawing).
    pub fn all(&self) -> impl Iterator<Item = &Placement> {
        self.placements.values()
    }

    /// The number of placements.
    pub fn len(&self) -> usize {
        self.placements.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }
}

/// A clipped rectangle: the visible portion of a placement (in cells).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClippedRect {
    /// The x position in columns (0-based).
    pub x: i32,
    /// The y position in rows (0-based).
    pub y: i32,
    /// The width in columns.
    pub columns: u32,
    /// The height in rows.
    pub rows: u32,
}

/// Clip a placement to the viewport (in cells).
///
/// Returns the visible portion, or `None` if the placement is entirely
/// offscreen.
pub fn clip(
    x: i32,
    y: i32,
    columns: u32,
    rows: u32,
    viewport_cols: i32,
    viewport_rows: i32,
) -> Option<ClippedRect> {
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + columns as i32).min(viewport_cols);
    let y1 = (y + rows as i32).min(viewport_rows);
    if x1 <= x0 || y1 <= y0 {
        return None; // Entirely offscreen.
    }
    Some(ClippedRect {
        x: x0,
        y: y0,
        columns: (x1 - x0) as u32,
        rows: (y1 - y0) as u32,
    })
}

/// A protocol handler that processes [`GraphicsEvent`]s against the image
/// and placement stores and generates protocol replies.
///
/// The handler speaks the real kitty-graphics protocol (per ADR-011): a `G`
/// command character followed by comma-separated `key=value` control data.
/// The action is selected by the `a` key (`T` transmit [default], `p`
/// placement, `q` query, `d` delete); `q=1` suppresses replies.
///
/// The reply format (documented subset) is an APC reusing the `G` command
/// character: `ESC _ G <keys> ST`, where `<keys>` carries the image ID
/// (`i`), the placement ID (`p`, for placement replies), the image
/// dimensions (`s`/`v`, for query replies), and an `OK`/`ERR` status.
pub struct GraphicsHandler {
    /// The uploaded images.
    pub images: ImageStore,
    /// The placements (separate from image IDs).
    pub placements: PlacementStore,
}

impl Default for GraphicsHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphicsHandler {
    /// Create a handler with empty stores.
    pub fn new() -> Self {
        Self {
            images: ImageStore::new(),
            placements: PlacementStore::new(),
        }
    }

    /// Process a graphics event, returning the reply string (if a reply is
    /// due and not suppressed) or `None`.
    pub fn process(&mut self, event: &GraphicsEvent) -> Option<String> {
        // The real protocol uses command 'G' with an 'a' action key.
        if event.command != b'G' {
            return None;
        }
        // Quiet mode: q=1 suppresses replies.
        let quiet = event.get("q").is_some_and(|v| v == "1");
        // The action key defaults to transmit (a=T) when absent.
        let action = event.get("a").unwrap_or("T");
        match action {
            "T" => self.process_transmit(event, quiet),
            "p" => self.process_place(event, quiet),
            "q" => self.process_query(event, quiet),
            "d" => self.process_delete(event, quiet),
            _ => None, // Unsupported action: not advertised, no reply.
        }
    }

    /// Build a reply APC (`ESC _ G <keys> ST`), or `None` when quiet.
    fn reply(&self, quiet: bool, keys: &str) -> Option<String> {
        if quiet {
            None
        } else {
            Some(format!("ESC _ G {keys} ST"))
        }
    }

    /// Transmit (store) an image: `f` (format, default 32), `s`/`v`
    /// (dimensions), `i` (image ID, optional). Replies only when `i` is
    /// present (the real protocol) and not quiet.
    fn process_transmit(&mut self, event: &GraphicsEvent, quiet: bool) -> Option<String> {
        let format = event.get_u32("f").unwrap_or(32);
        let width = event.get_u32("s").unwrap_or(0);
        let height = event.get_u32("v").unwrap_or(0);
        let image_id = event.get_u32("i");
        // Convert the payload to RGBA (validates format + data length).
        let rgba = match convert_to_rgba(format, width, height, &event.data) {
            Ok(rgba) => rgba,
            Err(_) => {
                let id = image_id.unwrap_or(0);
                return self.reply(quiet, &format!("i={id};ERR"));
            }
        };
        let result = self.images.upload_rgba(width, height, &rgba, image_id);
        match result {
            Ok(id) => {
                // Reply only when an image ID was requested (real protocol).
                if image_id.is_some() {
                    self.reply(quiet, &format!("i={id};OK"))
                } else {
                    None
                }
            }
            Err(_) => {
                let id = image_id.unwrap_or(0);
                self.reply(quiet, &format!("i={id};ERR"))
            }
        }
    }

    /// Place an image: `i` (image ID), `X`/`Y` (cell offsets), `c`/`r`
    /// (destination columns/rows). Placement IDs are auto-assigned (1-based);
    /// the `p` key is not yet supported in this subset.
    fn process_place(&mut self, event: &GraphicsEvent, quiet: bool) -> Option<String> {
        let image_id = event.get_u32("i").unwrap_or(0);
        let x = event.get_i32("X").unwrap_or(0);
        let y = event.get_i32("Y").unwrap_or(0);
        let columns = event.get_u32("c").unwrap_or(0);
        let rows = event.get_u32("r").unwrap_or(0);
        // The image must exist.
        if self.images.query(image_id).is_none() {
            return self.reply(quiet, &format!("i={image_id};ERR"));
        }
        let result = self.placements.place(image_id, x, y, columns, rows);
        match result {
            Ok(id) => self.reply(quiet, &format!("i={image_id},p={id};OK")),
            Err(_) => self.reply(quiet, &format!("i={image_id};ERR")),
        }
    }

    /// Query an image: `i` (image ID). Replies with the dimensions when
    /// found, `ERR` when not.
    fn process_query(&mut self, event: &GraphicsEvent, quiet: bool) -> Option<String> {
        let id = event.get_u32("i").unwrap_or(0);
        match self.images.query(id) {
            Some(image) => self.reply(
                quiet,
                &format!("i={id},s={},v={};OK", image.width, image.height),
            ),
            None => self.reply(quiet, &format!("i={id};ERR")),
        }
    }

    /// Delete: `a=d` with a `d` variant.
    /// - no `d` key: clear all visible placements.
    /// - `d=i` (+ `i`): delete image(s); no `i`/`i=0` deletes all images.
    /// - `d=p` (+ `p`): delete the placement with that ID.
    ///
    /// Other variants are not supported in this subset.
    fn process_delete(&mut self, event: &GraphicsEvent, quiet: bool) -> Option<String> {
        match event.get("d") {
            None => {
                // Clear all visible placements.
                let had = !self.placements.is_empty();
                self.placements.clear();
                self.reply(quiet, if had { "i=0;OK" } else { "i=0;ERR" })
            }
            Some("i") => {
                match event.get_u32("i") {
                    Some(0) | None => {
                        // Delete all images.
                        let had = !self.images.is_empty();
                        self.images.clear();
                        self.reply(quiet, if had { "i=0;OK" } else { "i=0;ERR" })
                    }
                    Some(id) => {
                        let found = self.images.delete(id).is_some();
                        self.reply(
                            quiet,
                            &format!("i={id};{}", if found { "OK" } else { "ERR" }),
                        )
                    }
                }
            }
            Some("p") => {
                let id = event.get_u32("p").unwrap_or(0);
                let found = self.placements.delete(id).is_some();
                self.reply(
                    quiet,
                    &format!("p={id};{}", if found { "OK" } else { "ERR" }),
                )
            }
            _ => self.reply(quiet, "ERR"),
        }
    }
}
