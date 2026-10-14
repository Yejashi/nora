//! Bounded inline image upload and storage (N33a).
//!
//! [`ImageStore`] decodes and stores uploaded graphics images with strict
//! size validation, checked allocation arithmetic, and per-image and total
//! byte budgets. It advertises only the capabilities it supports (one pixel
//! format: RGBA).

use std::collections::HashMap;

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
