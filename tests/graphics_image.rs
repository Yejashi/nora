//! N33a: bounded inline image upload and storage tests.

use nora::graphics_image::{ImageStore, UploadError};

/// Build a valid RGBA upload params string for a `width` x `height` image.
fn params(flags: u32, id: u32, width: u32, height: u32, format: u32) -> String {
    format!(
        "{};{};{};{};0;{};ignored-data-field",
        flags, id, width, height, format
    )
}

/// Build RGBA pixel data of the given size (width * height * 4 bytes).
fn pixels(width: u32, height: u32) -> Vec<u8> {
    vec![0u8; (width * height * 4) as usize]
}

#[test]
fn upload_rgba_success() {
    let mut store = ImageStore::new();
    let data = pixels(2, 2);
    let id = store
        .upload(&params(0, 0, 2, 2, 1), &data)
        .expect("valid RGBA upload");
    assert_eq!(store.len(), 1);
    let img = store.query(id).expect("stored");
    assert_eq!((img.width, img.height), (2, 2));
    assert_eq!(img.pixels.len(), 16);
}

#[test]
fn upload_malformed_params() {
    let mut store = ImageStore::new();
    // Too few fields.
    assert_eq!(
        store.upload("0;0;2;2;0;1", &[]),
        Err(UploadError::MalformedParams)
    );
    // Non-numeric field.
    assert_eq!(
        store.upload("x;0;2;2;0;1;data", &[]),
        Err(UploadError::MalformedParams)
    );
    // Flags > 1.
    assert_eq!(
        store.upload("2;0;2;2;0;1;data", &[]),
        Err(UploadError::MalformedParams)
    );
}

#[test]
fn upload_unsupported_format() {
    let mut store = ImageStore::new();
    // Format 0 (RGB) is not the supported RGBA format (1).
    assert_eq!(
        store.upload(&params(0, 0, 2, 2, 0), &pixels(2, 2)),
        Err(UploadError::UnsupportedFormat)
    );
}

#[test]
fn upload_dimension_too_large() {
    let mut store = ImageStore::new();
    assert_eq!(
        store.upload(&params(0, 0, 8193, 2, 1), &pixels(2, 2)),
        Err(UploadError::DimensionTooLarge)
    );
    // Zero dimension.
    assert_eq!(
        store.upload(&params(0, 0, 0, 2, 1), &pixels(2, 2)),
        Err(UploadError::DimensionTooLarge)
    );
}

#[test]
fn upload_data_length_mismatch() {
    let mut store = ImageStore::new();
    // 2x2 RGBA needs 16 bytes; give 8.
    assert_eq!(
        store.upload(&params(0, 0, 2, 2, 1), &[0u8; 8]),
        Err(UploadError::DataLengthMismatch)
    );
}

#[test]
fn upload_per_image_budget() {
    let mut store = ImageStore::new();
    // 4096x4096 RGBA = 64 MiB > 32 MiB per-image budget.
    let data = pixels(4096, 4096);
    assert_eq!(
        store.upload(&params(0, 0, 4096, 4096, 1), &data),
        Err(UploadError::PerImageBudget)
    );
}

#[test]
fn upload_total_budget() {
    let mut store = ImageStore::new();
    // 2048x2048 RGBA = 16 MiB per image. 8 images = 128 MiB (== budget).
    let data = pixels(2048, 2048);
    for i in 0..8 {
        store
            .upload(&params(0, i, 2048, 2048, 1), &data)
            .expect("within budget");
    }
    assert_eq!(store.total_bytes(), 8 * 16 * 1024 * 1024);
    // 9th image would exceed the 128 MiB total budget.
    assert_eq!(
        store.upload(&params(0, 99, 2048, 2048, 1), &data),
        Err(UploadError::TotalBudget)
    );
}

#[test]
fn upload_too_many_ids() {
    let mut store = ImageStore::new();
    // Explicit IDs 0..1024 (1024 images) fit; the 1025th is rejected.
    let data = pixels(1, 1);
    for i in 0..1024u32 {
        store
            .upload(&params(0, i, 1, 1, 1), &data)
            .expect("within ID limit");
    }
    assert_eq!(
        store.upload(&params(0, 5000, 1, 1, 1), &data),
        Err(UploadError::TooManyIds)
    );
}

#[test]
fn upload_auto_assign_id() {
    let mut store = ImageStore::new();
    let data = pixels(1, 1);
    let id0 = store.upload(&params(0, 0, 1, 1, 1), &data).expect("auto 0");
    let id1 = store.upload(&params(0, 0, 1, 1, 1), &data).expect("auto 1");
    assert_ne!(id0, id1, "auto IDs must differ");
}

#[test]
fn upload_explicit_id_and_duplicate() {
    let mut store = ImageStore::new();
    let data = pixels(1, 1);
    let id = store
        .upload(&params(0, 42, 1, 1, 1), &data)
        .expect("explicit");
    assert_eq!(id, 42);
    // Duplicate explicit ID is rejected.
    assert_eq!(
        store.upload(&params(0, 42, 1, 1, 1), &data),
        Err(UploadError::MalformedParams)
    );
}

#[test]
fn query_and_delete_releases_bytes() {
    let mut store = ImageStore::new();
    let data = pixels(2, 2);
    let id = store.upload(&params(0, 0, 2, 2, 1), &data).expect("upload");
    assert_eq!(store.total_bytes(), 16);
    assert!(store.query(id).is_some());
    let removed = store.delete(id).expect("delete");
    assert_eq!(removed.pixels.len(), 16);
    assert!(store.query(id).is_none());
    assert_eq!(store.total_bytes(), 0);
    assert!(store.is_empty());
}

#[test]
fn capabilities_advertise_rgba_only() {
    let caps = nora::graphics_image::ImageStore::capabilities();
    assert_eq!(caps.supported_formats, &[1], "only RGBA (1) supported");
    assert_eq!(caps.max_dimension, 8192);
    assert_eq!(caps.max_image_bytes, 32 * 1024 * 1024);
    assert_eq!(caps.max_total_bytes, 128 * 1024 * 1024);
    assert_eq!(caps.max_image_ids, 1024);
}

#[test]
fn checked_allocation_no_overflow() {
    let mut store = ImageStore::new();
    // Max allowed dimensions: 8192x8192 RGBA = 256 MiB, exceeds per-image
    // budget and must fail safely (no allocation, no overflow).
    let data = pixels(8192, 8192);
    assert_eq!(
        store.upload(&params(0, 0, 8192, 8192, 1), &data),
        Err(UploadError::PerImageBudget)
    );
}
