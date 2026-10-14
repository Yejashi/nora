//! Bounded inline image upload and storage (N33a).
//!
//! [`ImageStore`] decodes and stores uploaded graphics images with strict
//! size validation, checked allocation arithmetic, and per-image and total
//! byte budgets. It advertises only the capabilities it supports (one pixel
//! format: RGBA).

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
/// Bytes per pixel for the supported RGBA format.
const BYTES_PER_PIXEL: usize = 4;

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

/// The capabilities the graphics implementation advertises.
///
/// Only what is actually supported is listed; unsupported capabilities are
/// not advertised as full protocol support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphicsCapabilities {
    /// Supported pixel formats (RGBA only).
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
#[derive(Debug, Default)]
pub struct ImageStore {
    images: HashMap<u32, Image>,
    total_bytes: usize,
    next_id: u32,
}

impl ImageStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Upload an image from a decoded payload.
    ///
    /// `params` is the raw params string (after the command char), with the
    /// form `<flags>;<id>;<width>;<height>;<colorspace>;<format>;<data>`.
    /// `data` is the base64-decoded pixel data (from the N32a filter).
    ///
    /// Returns the image ID on success, or an error on failure.
    pub fn upload(&mut self, params: &str, data: &[u8]) -> Result<u32, UploadError> {
        // Split params on `;` into fields.
        let fields: Vec<&str> = params.split(';').collect();
        // Need exactly 7 fields: flags, id, width, height, colorspace, format, data.
        if fields.len() != 7 {
            return Err(UploadError::MalformedParams);
        }
        // Parse the metadata fields.
        let flags: u32 = fields[0]
            .parse()
            .map_err(|_| UploadError::MalformedParams)?;
        let id: u32 = fields[1]
            .parse()
            .map_err(|_| UploadError::MalformedParams)?;
        let width: u32 = fields[2]
            .parse()
            .map_err(|_| UploadError::MalformedParams)?;
        let height: u32 = fields[3]
            .parse()
            .map_err(|_| UploadError::MalformedParams)?;
        let _colorspace: u32 = fields[4]
            .parse()
            .map_err(|_| UploadError::MalformedParams)?;
        let format: u32 = fields[5]
            .parse()
            .map_err(|_| UploadError::MalformedParams)?;
        // Validate flags (0 = no reply, 1 = reply).
        if flags > 1 {
            return Err(UploadError::MalformedParams);
        }
        // Validate dimensions (non-zero, within max).
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(UploadError::DimensionTooLarge);
        }
        // Validate the pixel format (only RGBA = 1 is supported).
        if format != 1 {
            return Err(UploadError::UnsupportedFormat);
        }
        // Checked allocation arithmetic: width * height * BYTES_PER_PIXEL.
        let expected_bytes = (width as u64)
            .checked_mul(height as u64)
            .and_then(|v| v.checked_mul(BYTES_PER_PIXEL as u64))
            .and_then(|v| u32::try_from(v).ok())
            .ok_or(UploadError::DimensionTooLarge)?;
        // Validate the data length matches the expected size.
        if data.len() != expected_bytes as usize {
            return Err(UploadError::DataLengthMismatch);
        }
        // Enforce the per-image byte budget.
        if data.len() > MAX_IMAGE_BYTES {
            return Err(UploadError::PerImageBudget);
        }
        // Enforce the total byte budget.
        if self.total_bytes + data.len() > MAX_TOTAL_BYTES {
            return Err(UploadError::TotalBudget);
        }
        // Determine the image ID (auto-assign if 0).
        let image_id = if id == 0 {
            if self.images.len() >= MAX_IMAGE_IDS {
                return Err(UploadError::TooManyIds);
            }
            let new_id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1);
            new_id
        } else {
            // Explicit ID: must not already be in use.
            if self.images.contains_key(&id) {
                return Err(UploadError::MalformedParams);
            }
            if self.images.len() >= MAX_IMAGE_IDS {
                return Err(UploadError::TooManyIds);
            }
            id
        };
        // Store the image.
        let image = Image {
            width,
            height,
            pixels: data.to_vec(),
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

    /// The total byte cost of all images.
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// The capabilities this implementation advertises (RGBA only).
    pub fn capabilities() -> GraphicsCapabilities {
        GraphicsCapabilities {
            supported_formats: &[1],
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

/// Protocol reply status codes (documented subset of the kitty graphics
/// protocol).
pub mod status {
    /// The command succeeded.
    pub const SUCCESS: u8 = 0;
    /// Too many images / IDs (total budget or ID cap reached).
    pub const TOO_MANY: u8 = 1;
    /// Invalid parameter (malformed params, unsupported format, bad
    /// dimensions, unknown image).
    pub const INVALID_PARAM: u8 = 2;
    /// Corrupted image data (length mismatch).
    pub const CORRUPTED: u8 = 3;
    /// Memory allocation failure (per-image budget exceeded).
    pub const ALLOC_FAILURE: u8 = 4;
    /// Internal error.
    pub const INTERNAL: u8 = 5;
    /// Query: the image was not found.
    pub const NOT_FOUND: u8 = 1;
}

/// A protocol handler that processes [`GraphicsEvent`]s against the image
/// and placement stores and generates protocol replies.
///
/// The reply format (documented subset) is an APC reusing the command
/// character: `ESC _ <cmd>;<status>;<id> ST`. Query replies additionally
/// carry the image dimensions. Replies are produced only when the reply
/// flag (bit 0 of the flags field) is set.
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

    /// Process a graphics event, returning the reply string (if the reply
    /// flag is set and a reply is due) or `None`.
    pub fn process(&mut self, event: &GraphicsEvent) -> Option<String> {
        let fields: Vec<&str> = event.params.split(';').collect();
        let reply = fields
            .first()
            .and_then(|f| f.parse::<u32>().ok())
            .is_some_and(|f| f & 1 == 1);
        match event.command {
            b'e' => self.process_upload(&event.params, &event.data, reply),
            b't' => self.process_place(&fields, reply),
            b'q' => self.process_query(&fields, reply),
            b'c' => self.process_delete_image(&fields, reply),
            b'd' => self.process_delete_placement(&fields, reply),
            _ => None, // Unsupported command: not advertised, no reply.
        }
    }

    fn process_upload(&mut self, params: &str, data: &[u8], reply: bool) -> Option<String> {
        // params = <flags>;<id>;<w>;<h>;<colorspace>;<format>;<data>
        let result = self.images.upload(params, data);
        let status = match result {
            Ok(_) => status::SUCCESS,
            Err(UploadError::MalformedParams)
            | Err(UploadError::UnsupportedFormat)
            | Err(UploadError::DimensionTooLarge) => status::INVALID_PARAM,
            Err(UploadError::DataLengthMismatch) => status::CORRUPTED,
            Err(UploadError::PerImageBudget) => status::ALLOC_FAILURE,
            Err(UploadError::TotalBudget) | Err(UploadError::TooManyIds) => status::TOO_MANY,
        };
        if reply {
            match result {
                Ok(id) => Some(format!("ESC _ e;{status};{id} ST")),
                Err(_) => Some(format!("ESC _ e;{status};0 ST")),
            }
        } else {
            None
        }
    }

    fn process_place(&mut self, fields: &[&str], reply: bool) -> Option<String> {
        // params = <flags>;<image_id>;<x>;<y>;<columns>;<rows>
        if fields.len() != 6 {
            return reply.then(|| format!("ESC _ t;{};0 ST", status::INVALID_PARAM));
        }
        let image_id: u32 = fields[1].parse().unwrap_or(u32::MAX);
        let x: i32 = fields[2].parse().unwrap_or(0);
        let y: i32 = fields[3].parse().unwrap_or(0);
        let columns: u32 = fields[4].parse().unwrap_or(0);
        let rows: u32 = fields[5].parse().unwrap_or(0);
        // The caller ensures the image exists; a missing image is an
        // invalid parameter.
        if self.images.query(image_id).is_none() {
            return reply.then(|| format!("ESC _ t;{};0 ST", status::INVALID_PARAM));
        }
        let result = self.placements.place(image_id, x, y, columns, rows);
        let (status, id) = match result {
            Ok(id) => (status::SUCCESS, id),
            Err(PlacementError::InvalidDimensions) | Err(PlacementError::TooManyPlacements) => {
                (status::INVALID_PARAM, 0)
            }
        };
        reply.then(|| format!("ESC _ t;{status};{id} ST"))
    }

    fn process_query(&mut self, fields: &[&str], reply: bool) -> Option<String> {
        // params = <flags>;<id>
        if fields.len() != 2 {
            return reply.then(|| format!("ESC _ q;{};0 ST", status::INVALID_PARAM));
        }
        let id: u32 = fields[1].parse().unwrap_or(u32::MAX);
        match self.images.query(id) {
            Some(image) => reply.then(|| {
                format!(
                    "ESC _ q;{};{};{};{};1;1 ST",
                    status::SUCCESS,
                    id,
                    image.width,
                    image.height
                )
            }),
            None => reply.then(|| format!("ESC _ q;{};{id} ST", status::NOT_FOUND)),
        }
    }

    fn process_delete_image(&mut self, fields: &[&str], reply: bool) -> Option<String> {
        // params = <flags>;<id> (id=0 clears all)
        if fields.len() != 2 {
            return reply.then(|| format!("ESC _ c;{};0 ST", status::INVALID_PARAM));
        }
        let id: u32 = fields[1].parse().unwrap_or(u32::MAX);
        let found = if id == 0 {
            !self.images.is_empty()
        } else {
            self.images.delete(id).is_some()
        };
        let status = if found {
            status::SUCCESS
        } else {
            status::NOT_FOUND
        };
        reply.then(|| format!("ESC _ c;{status};{id} ST"))
    }

    fn process_delete_placement(&mut self, fields: &[&str], reply: bool) -> Option<String> {
        // params = <flags>;<id>
        if fields.len() != 2 {
            return reply.then(|| format!("ESC _ d;{};0 ST", status::INVALID_PARAM));
        }
        let id: u32 = fields[1].parse().unwrap_or(u32::MAX);
        let found = self.placements.delete(id).is_some();
        let status = if found {
            status::SUCCESS
        } else {
            status::NOT_FOUND
        };
        reply.then(|| format!("ESC _ d;{status};{id} ST"))
    }
}
