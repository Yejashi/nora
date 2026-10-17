//! Kitty graphics protocol state (ADR-014): commands, formats, replies,
//! placements and deletes, against
//! <https://sw.kovidgoyal.net/kitty/graphics-protocol/>.

use std::io::Write;

use nora::graphics_filter::GraphicsEvent;
use nora::graphics_image::{Cursor, GraphicsState, Outcome};

/// A command from its control keys (`a=T,i=5`) and decoded payload.
fn cmd(keys: &str, data: &[u8]) -> GraphicsEvent {
    GraphicsEvent {
        command: b'G',
        keys: keys
            .split(',')
            .filter(|kv| !kv.is_empty())
            .map(|kv| {
                let (k, v) = kv.split_once('=').expect("key=value");
                (k.to_string(), v.to_string())
            })
            .collect(),
        data: data.to_vec(),
    }
}

/// The cursor at (line, col) on an 80x24 screen with 10x20 cells.
fn at(line: i32, col: usize) -> Cursor {
    Cursor {
        line,
        col,
        scrolled: 0,
        pulled: 0,
        columns: 80,
        lines: 24,
        alt: false,
        cell_width: 10,
        cell_height: 20,
    }
}

fn reply(outcome: &Outcome) -> Option<String> {
    outcome
        .reply
        .as_ref()
        .map(|r| String::from_utf8(r.clone()).expect("ASCII reply"))
}

fn rgba(width: u32, height: u32) -> Vec<u8> {
    [255u8, 0, 0, 255].repeat((width * height) as usize)
}

#[test]
fn transmit_replies_only_when_an_image_is_named() {
    let mut g = GraphicsState::new();
    let out = g.handle(&cmd("a=t,f=32,s=2,v=1,i=5", &rgba(2, 1)), &at(0, 0));
    assert_eq!(reply(&out).as_deref(), Some("\x1b_Gi=5;OK\x1b\\"));
    assert_eq!(out.move_cursor, None, "transmit only does not display");
    let image = g.image(5).expect("stored");
    assert_eq!((image.width, image.height), (2, 1));
    let out = g.handle(&cmd("f=32,s=1,v=1", &rgba(1, 1)), &at(0, 0));
    assert_eq!(out.reply, None, "no id, no reply");
    assert_eq!(g.image_count(), 2);
}

#[test]
fn quiet_modes_and_errors() {
    let mut g = GraphicsState::new();
    let ok = g.handle(&cmd("a=t,s=1,v=1,i=1,q=1", &rgba(1, 1)), &at(0, 0));
    assert_eq!(ok.reply, None, "q=1 drops OK");
    let short = g.handle(&cmd("a=t,s=2,v=2,i=2,q=1", &rgba(1, 1)), &at(0, 0));
    assert!(
        reply(&short).unwrap().starts_with("\x1b_Gi=2;ENODATA:"),
        "q=1 keeps errors"
    );
    let silent = g.handle(&cmd("a=t,s=2,v=2,i=3,q=2", &rgba(1, 1)), &at(0, 0));
    assert_eq!(silent.reply, None, "q=2 drops errors");
    let both = g.handle(&cmd("a=t,s=1,v=1,i=4,I=7", &rgba(1, 1)), &at(0, 0));
    assert!(reply(&both).unwrap().contains(";EINVAL:"));
    let format = g.handle(&cmd("a=t,f=7,s=1,v=1,i=6", &rgba(1, 1)), &at(0, 0));
    assert!(reply(&format).unwrap().contains(";EINVAL:"));
    let shm = g.handle(
        &cmd("a=t,t=s,s=1,v=1,i=8", b"/nora-no-such-object"),
        &at(0, 0),
    );
    assert!(reply(&shm).unwrap().contains(";EBADF:"), "a missing object");
}

#[test]
fn rgb_png_and_zlib() {
    let mut g = GraphicsState::new();
    g.handle(&cmd("a=t,f=24,s=2,v=1,i=1", &[1, 2, 3, 4, 5, 6]), &at(0, 0));
    assert_eq!(&*g.image(1).unwrap().pixels, &[1, 2, 3, 255, 4, 5, 6, 255]);

    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, 2, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[9, 8, 7, 6, 5, 4]).unwrap();
    }
    let out = g.handle(&cmd("a=t,f=100,i=2", &png_bytes), &at(0, 0));
    assert_eq!(reply(&out).as_deref(), Some("\x1b_Gi=2;OK\x1b\\"));
    let image = g.image(2).unwrap();
    assert_eq!((image.width, image.height), (2, 1), "size from the PNG");
    assert_eq!(&*image.pixels, &[9, 8, 7, 255, 6, 5, 4, 255]);
    let bad = g.handle(&cmd("a=t,f=100,i=3", b"not a png"), &at(0, 0));
    assert!(reply(&bad).unwrap().contains(";EBADPNG:"));

    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&rgba(3, 2)).unwrap();
    let out = g.handle(&cmd("a=t,o=z,s=3,v=2,i=4", &z.finish().unwrap()), &at(0, 0));
    assert_eq!(reply(&out).as_deref(), Some("\x1b_Gi=4;OK\x1b\\"));
    assert_eq!(g.image(4).unwrap().pixels.len(), 3 * 2 * 4);
}

#[test]
fn chunked_transfer_replies_at_the_end() {
    let mut g = GraphicsState::new();
    let data = rgba(2, 2);
    let first = g.handle(&cmd("a=T,s=2,v=2,i=9,m=1", &data[..8]), &at(3, 4));
    assert_eq!(first, Outcome::default());
    let middle = g.handle(&cmd("m=1", &data[8..12]), &at(3, 4));
    assert_eq!(middle, Outcome::default());
    let last = g.handle(&cmd("m=0", &data[12..]), &at(3, 4));
    assert_eq!(reply(&last).as_deref(), Some("\x1b_Gi=9;OK\x1b\\"));
    assert_eq!(
        last.move_cursor,
        Some((1, 1)),
        "the first chunk's a=T displays"
    );
    assert_eq!(g.image(9).unwrap().pixels.len(), 16);
}

#[test]
fn display_size_and_cursor_movement() {
    let mut g = GraphicsState::new();
    // 25x45 px at 10x20 cells: 3 columns, 3 rows.
    let out = g.handle(&cmd("a=T,s=25,v=45,i=1", &rgba(25, 45)), &at(2, 5));
    assert_eq!(out.move_cursor, Some((3, 3)));
    let p = g.placements().last().unwrap();
    assert_eq!((p.col, p.columns, p.rows, p.size), (5, 3, 3, (25, 45)));
    // C=1 keeps the cursor; c only keeps the aspect ratio; c and r stretch.
    let still = g.handle(&cmd("a=p,i=1,C=1", &[]), &at(0, 0));
    assert_eq!(still.move_cursor, None);
    g.handle(&cmd("a=p,i=1,c=5", &[]), &at(0, 0));
    assert_eq!(g.placements().last().unwrap().size, (50, 90));
    g.handle(&cmd("a=p,i=1,c=4,r=2", &[]), &at(0, 0));
    assert_eq!(g.placements().last().unwrap().size, (40, 40));
    // A source rectangle and a pixel offset (clamped inside the cell).
    g.handle(&cmd("a=p,i=1,x=5,y=5,w=10,h=10,X=8,Y=30", &[]), &at(0, 0));
    let p = g.placements().last().unwrap();
    assert_eq!(
        (p.source, p.offset, p.columns, p.rows),
        ([5, 5, 10, 10], (8, 19), 2, 2)
    );
}

#[test]
fn put_by_id_or_number() {
    let mut g = GraphicsState::new();
    let missing = g.handle(&cmd("a=p,i=42", &[]), &at(0, 0));
    assert!(reply(&missing).unwrap().starts_with("\x1b_Gi=42;ENOENT:"));
    let first = g.handle(&cmd("a=t,s=1,v=1,I=3", &rgba(1, 1)), &at(0, 0));
    let first = reply(&first).unwrap();
    assert!(
        first.starts_with("\x1b_Gi=") && first.ends_with(",I=3;OK\x1b\\"),
        "{first:?}"
    );
    g.handle(&cmd("a=t,s=2,v=2,I=3,q=2", &rgba(2, 2)), &at(0, 0));
    let put = g.handle(&cmd("a=p,I=3,p=7", &[]), &at(0, 0));
    assert!(reply(&put).unwrap().ends_with(",I=3,p=7;OK\x1b\\"));
    let newest = g.placements().last().unwrap().image_id;
    assert_eq!(
        g.image(newest).unwrap().width,
        2,
        "the newest image with that number"
    );
}

#[test]
fn query_does_not_store() {
    let mut g = GraphicsState::new();
    // The support-detection query.
    let out = g.handle(&cmd("i=31,s=1,v=1,a=q,t=d,f=24", &[0, 0, 0]), &at(0, 0));
    assert_eq!(reply(&out).as_deref(), Some("\x1b_Gi=31;OK\x1b\\"));
    assert_eq!(g.image_count(), 0);
    assert_eq!(out.move_cursor, None);
}

#[test]
fn placements_scroll_with_the_text() {
    let mut g = GraphicsState::new();
    g.handle(&cmd("a=T,s=10,v=40,i=1", &rgba(10, 40)), &at(5, 0));
    let rows = |g: &mut GraphicsState, scrolled, offset| -> Vec<i32> {
        g.visible(scrolled, 0, offset, 24, 100, false)
            .iter()
            .map(|v| v.row)
            .collect()
    };
    assert_eq!(rows(&mut g, 0, 0), vec![5]);
    assert_eq!(rows(&mut g, 3, 0), vec![2], "three lines scrolled up");
    assert_eq!(
        rows(&mut g, 3, 2),
        vec![4],
        "viewport two lines into the history"
    );
    assert_eq!(
        rows(&mut g, 6, 0),
        vec![-1],
        "partly above the screen, still drawn"
    );
    assert_eq!(
        rows(&mut g, 7, 0),
        Vec::<i32>::new(),
        "in the history, off screen"
    );
    assert_eq!(rows(&mut g, 200, 0), Vec::<i32>::new());
    assert_eq!(g.placements().count(), 0, "dropped once out of the history");
    assert!(g.visible(0, 0, 0, 24, 100, true).is_empty(), "other screen");
}

#[test]
fn deletes() {
    let mut g = GraphicsState::new();
    g.handle(&cmd("a=T,s=10,v=20,i=1", &rgba(10, 20)), &at(0, 0));
    g.handle(&cmd("a=T,s=10,v=20,i=2,z=-1", &rgba(10, 20)), &at(1, 3));
    g.handle(&cmd("a=T,s=10,v=20,i=3,p=4", &rgba(10, 20)), &at(2, 6));
    let ids = |g: &GraphicsState| -> Vec<u32> { g.placements().map(|p| p.image_id).collect() };
    let out = g.handle(&cmd("a=d,d=i,i=1", &[]), &at(9, 9));
    assert_eq!(out, Outcome::default(), "deletes do not reply");
    assert_eq!(ids(&g), vec![2, 3]);
    assert!(g.image(1).is_some(), "lowercase keeps the image");
    g.handle(&cmd("a=d,d=z,z=-1", &[]), &at(9, 9));
    assert_eq!(ids(&g), vec![3]);
    g.handle(&cmd("a=d,d=p,x=7,y=3", &[]), &at(9, 9));
    assert!(ids(&g).is_empty(), "the placement covering cell (7,3)");
    g.handle(&cmd("a=p,i=2", &[]), &at(0, 0));
    g.handle(&cmd("a=d,d=I,i=2", &[]), &at(9, 9));
    assert!(g.image(2).is_none(), "uppercase frees the image");
    g.handle(&cmd("a=p,i=1", &[]), &at(0, 0));
    g.handle(&cmd("a=d", &[]), &at(9, 9));
    assert!(ids(&g).is_empty(), "d=a: all on the screen");
    assert!(g.image(1).is_some());
}

/// A flood of tiny images is bounded by count and by a per-image charge;
/// placed images are evicted last, and puts by number find the newest.
#[test]
fn image_count_and_overhead_are_bounded() {
    use nora::graphics_image::{IMAGE_OVERHEAD, MAX_IMAGES, MAX_TOTAL_BYTES};
    let mut g = GraphicsState::new();
    g.handle(&cmd("a=T,s=1,v=1,i=1", &rgba(1, 1)), &at(0, 0));
    for _ in 0..MAX_IMAGES * 3 {
        g.handle(&cmd("a=t,s=1,v=1", &rgba(1, 1)), &at(0, 0));
        assert!(g.image_count() <= MAX_IMAGES);
    }
    assert_eq!(g.image_count(), MAX_IMAGES);
    assert_eq!(g.total_bytes(), MAX_IMAGES * (4 + IMAGE_OVERHEAD));
    assert!(g.total_bytes() <= MAX_TOTAL_BYTES);
    assert!(g.image(1).is_some(), "the placed image outlives newer ones");
    // Two images with one number: a put by number shows the newer one.
    let id_of = |out: &Outcome| -> u32 {
        let r = reply(out).expect("reply");
        r[r.find("i=").unwrap() + 2..r.find(",I=").unwrap()]
            .parse()
            .unwrap()
    };
    let older = id_of(&g.handle(&cmd("a=t,s=1,v=1,I=9", &rgba(1, 1)), &at(0, 0)));
    let newer = id_of(&g.handle(&cmd("a=t,s=1,v=1,I=9", &rgba(1, 1)), &at(0, 0)));
    assert_ne!(older, newer);
    g.handle(&cmd("a=p,I=9", &[]), &at(0, 0));
    assert_eq!(g.placements().last().map(|p| p.image_id), Some(newer));
    g.handle(&cmd("a=d,d=N,I=9", &[]), &at(0, 0));
    assert!(g.image(newer).is_none());
    g.handle(&cmd("a=p,I=9", &[]), &at(0, 0));
    assert_eq!(g.placements().last().map(|p| p.image_id), Some(older));
    g.reset();
    assert_eq!((g.image_count(), g.total_bytes()), (0, 0));
}

/// `d=R` frees the images with ids in `x..=y`; `y < x` is an empty range.
#[test]
fn an_empty_id_range_frees_nothing() {
    let mut g = GraphicsState::new();
    for id in 3..=6 {
        g.handle(&cmd(&format!("a=t,s=1,v=1,i={id}"), &rgba(1, 1)), &at(0, 0));
    }
    g.handle(&cmd("a=d,d=R,x=5,y=3", &[]), &at(0, 0));
    assert_eq!(g.image_count(), 4, "y < x names no image");
    g.handle(&cmd("a=d,d=R,x=4,y=5", &[]), &at(0, 0));
    assert!(g.image(4).is_none() && g.image(5).is_none());
    assert!(g.image(3).is_some() && g.image(6).is_some());
}

#[test]
fn replacing_an_image_drops_its_placements() {
    let mut g = GraphicsState::new();
    g.handle(&cmd("a=T,s=1,v=1,i=1", &rgba(1, 1)), &at(0, 0));
    let generation = g.image(1).unwrap().generation;
    g.handle(&cmd("a=t,s=2,v=2,i=1", &rgba(2, 2)), &at(0, 0));
    assert_eq!(g.placements().count(), 0);
    assert!(g.image(1).unwrap().generation > generation);
}

#[test]
fn clears() {
    let mut g = GraphicsState::new();
    g.handle(&cmd("a=T,s=1,v=1,i=1", &rgba(1, 1)), &at(0, 0));
    let mut scrolled = at(0, 0);
    scrolled.scrolled = 30;
    g.handle(&cmd("a=T,s=1,v=1,i=1,p=2", &[]), &at(0, 0));
    g.handle(&cmd("a=p,i=1", &[]), &scrolled);
    // Two placements in the scrollback (lines -30), one on the screen.
    g.clear_screen(&scrolled);
    assert_eq!(g.placements().count(), 1, "only the screen's");
    g.clear_scrollback(&scrolled);
    assert_eq!(g.placements().count(), 0);
    g.reset();
    assert_eq!((g.image_count(), g.total_bytes()), (0, 0));
}

#[test]
fn file_media() {
    let dir = std::env::temp_dir().join(format!("nora-graphics-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("image.rgba");
    std::fs::write(&file, rgba(2, 2)).unwrap();
    let mut g = GraphicsState::new();
    let path = file.to_string_lossy().into_owned();
    let out = g.handle(&cmd("a=t,t=f,s=2,v=2,i=1", path.as_bytes()), &at(0, 0));
    assert_eq!(reply(&out).as_deref(), Some("\x1b_Gi=1;OK\x1b\\"));
    assert!(file.exists(), "t=f keeps the file");
    for bad in ["/proc/self/status", "/nonexistent/x", "/etc"] {
        let out = g.handle(&cmd("a=t,t=f,s=1,v=1,i=2", bad.as_bytes()), &at(0, 0));
        assert_eq!(
            reply(&out).as_deref(),
            Some("\x1b_Gi=2;EBADF:Failed to read image data\x1b\\"),
            "{bad}"
        );
    }
    // A decode failure on a readable file is the same EBADF.
    std::fs::write(&file, b"short").unwrap();
    let out = g.handle(&cmd("a=t,t=f,s=2,v=2,i=3", path.as_bytes()), &at(0, 0));
    assert!(reply(&out).unwrap().contains(";EBADF:"));
    // /dev/shm is a temporary directory clients use (kitten icat does).
    if std::path::Path::new("/dev/shm").is_dir() {
        let shm = format!("/dev/shm/nora-tty-graphics-protocol-{}", std::process::id());
        std::fs::write(&shm, rgba(1, 1)).unwrap();
        let out = g.handle(&cmd("a=q,t=t,s=1,v=1,i=9", shm.as_bytes()), &at(0, 0));
        assert_eq!(reply(&out).as_deref(), Some("\x1b_Gi=9;OK\x1b\\"));
        assert!(
            !std::path::Path::new(&shm).exists(),
            "removed after reading"
        );
    }
    // t=t removes only a protocol temp file.
    let temp = std::env::temp_dir().join(format!("tty-graphics-protocol-{}", std::process::id()));
    std::fs::write(&temp, rgba(1, 1)).unwrap();
    let canonical = std::fs::canonicalize(&temp).unwrap();
    let in_tmp = ["/tmp", "/dev/shm"]
        .iter()
        .any(|d| canonical.starts_with(d))
        || std::env::var_os("TMPDIR").is_some();
    g.handle(
        &cmd("a=t,t=t,s=1,v=1,i=4", temp.to_string_lossy().as_bytes()),
        &at(0, 0),
    );
    assert!(g.image(4).is_some());
    assert_eq!(!temp.exists(), in_tmp, "deleted when in a temp directory");
    std::fs::write(&file, rgba(2, 2)).unwrap();
    g.handle(&cmd("a=t,t=t,s=2,v=2,i=5", path.as_bytes()), &at(0, 0));
    assert!(file.exists(), "not a protocol temp file: kept");
    let _ = std::fs::remove_file(&temp);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `t=s`: a POSIX shared-memory object (`/dev/shm` on Linux) is read and
/// unlinked once it decoded; one that is not the image is left alone.
#[test]
fn shared_memory_media() {
    if !std::path::Path::new("/dev/shm").is_dir() {
        return;
    }
    let mut g = GraphicsState::new();
    let name = format!("/nora-graphics-shm-{}", std::process::id());
    let file = format!("/dev/shm{name}");
    std::fs::write(&file, rgba(2, 1)).unwrap();
    let out = g.handle(&cmd("a=t,t=s,s=2,v=1,i=1,S=8", name.as_bytes()), &at(0, 0));
    assert_eq!(reply(&out).as_deref(), Some("\x1b_Gi=1;OK\x1b\\"));
    assert!(
        !std::path::Path::new(&file).exists(),
        "unlinked after reading"
    );
    std::fs::write(&file, b"not an image").unwrap();
    let out = g.handle(&cmd("a=t,t=s,f=100,i=2", name.as_bytes()), &at(0, 0));
    assert!(reply(&out).unwrap().contains(";EBADF:"));
    assert!(
        std::path::Path::new(&file).exists(),
        "kept when it is not the image"
    );
    let out = g.handle(&cmd("a=t,t=s,s=1,v=1,i=3", b"/a/b"), &at(0, 0));
    assert!(
        reply(&out).unwrap().contains(";EBADF:"),
        "a name with a slash"
    );
    std::fs::remove_file(&file).unwrap();
}

/// Make a FIFO at `path` (nobody opens it for writing).
fn mkfifo(path: &std::path::Path) {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `name` is a valid NUL-terminated path for the call.
    let rc = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo {}", path.display());
}

/// Handle `command` on another thread; its reply, or `None` if it did not
/// finish within five seconds (it blocked).
fn reply_within_timeout(command: GraphicsEvent) -> Option<Option<String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut g = GraphicsState::new();
        let out = g.handle(&command, &at(0, 0));
        let _ = tx.send(reply(&out));
    });
    rx.recv_timeout(std::time::Duration::from_secs(5)).ok()
}

/// A FIFO with no writer is refused at once: opening it for reading would
/// block the PTY reader thread (holding the terminal lock) forever.
#[test]
fn a_fifo_is_refused_without_blocking() {
    let dir = std::env::temp_dir().join(format!("nora-graphics-fifo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fifo = dir.join("pipe");
    let _ = std::fs::remove_file(&fifo);
    mkfifo(&fifo);
    let path = fifo.to_string_lossy().into_owned();
    let ebadf = Some("\x1b_Gi=1;EBADF:Failed to read image data\x1b\\".to_string());
    for medium in ["f", "t"] {
        let command = cmd(&format!("a=t,t={medium},s=1,v=1,i=1"), path.as_bytes());
        assert_eq!(
            reply_within_timeout(command),
            Some(ebadf.clone()),
            "t={medium}"
        );
    }
    assert!(fifo.exists(), "not a protocol temp file: kept");
    let _ = std::fs::remove_dir_all(&dir);
    if std::path::Path::new("/dev/shm").is_dir() {
        let name = format!("/nora-graphics-fifo-{}", std::process::id());
        let shm = std::path::PathBuf::from(format!("/dev/shm{name}"));
        let _ = std::fs::remove_file(&shm);
        mkfifo(&shm);
        let command = cmd("a=t,t=s,s=1,v=1,i=1", name.as_bytes());
        assert_eq!(reply_within_timeout(command), Some(ebadf), "t=s");
        let _ = std::fs::remove_file(&shm);
    }
}

/// Client sizes (`c`, `r`) are clamped, so no size arithmetic overflows
/// (a debug build used to panic on the PTY thread).
#[test]
fn huge_cell_counts_are_clamped() {
    use nora::graphics_image::MAX_PLACEMENT_CELLS as MAX;
    let mut g = GraphicsState::new();
    g.handle(&cmd("a=t,f=32,s=1,v=1,i=1", &rgba(1, 1)), &at(0, 0));
    let out = g.handle(&cmd("a=p,i=1,c=4294967295,r=4294967295", &[]), &at(0, 0));
    assert_eq!(out.move_cursor, Some((MAX, MAX)));
    let p = g.placements().last().unwrap();
    assert_eq!(
        (p.columns, p.rows, p.size),
        (MAX, MAX, (MAX * 10, MAX * 20))
    );
    // Only `r`: the width follows the aspect ratio, also clamped.
    let out = g.handle(&cmd("a=p,i=1,r=4294967295,X=9,Y=19", &[]), &at(0, 0));
    assert_eq!(out.move_cursor, Some((MAX, MAX)));
    // A tall image scaled by `c` only.
    g.handle(&cmd("a=t,f=32,s=1,v=8192,i=2", &rgba(1, 8192)), &at(0, 0));
    let out = g.handle(&cmd("a=p,i=2,c=4294967295", &[]), &at(0, 0));
    assert_eq!(out.move_cursor, Some((MAX, MAX)));
    assert_eq!(g.placements().last().unwrap().size, (MAX * 10, MAX * 20));
}

/// Each pane has its own state, but the renderer keys textures by (id,
/// generation): two states storing the same id (or their first internal
/// id) never share a generation.
#[test]
fn generations_are_unique_across_states() {
    let (mut a, mut b) = (GraphicsState::new(), GraphicsState::new());
    let mut seen = std::collections::HashSet::new();
    for g in [&mut a, &mut b] {
        g.handle(&cmd("a=t,f=32,s=1,v=1,i=1", &rgba(1, 1)), &at(0, 0));
        g.handle(&cmd("a=T,f=32,s=1,v=1", &rgba(1, 1)), &at(0, 0));
        let internal = g.placements().last().unwrap().image_id;
        for id in [1, internal] {
            assert!(seen.insert(g.image(id).unwrap().generation), "id {id}");
        }
    }
    let keys: std::collections::HashSet<_> = [&a, &b]
        .iter()
        .flat_map(|g| {
            g.placements()
                .map(|p| (p.image_id, g.image(p.image_id).unwrap().generation))
        })
        .collect();
    assert_eq!(keys.len(), 2, "the internal images' texture keys differ");
}

/// An image stored without an id or number gets an internal id the client
/// never learns; a client using that id (they are 2^31 and up) reaches its
/// own image, never the internal one.
#[test]
fn clients_never_address_internal_images() {
    let mut g = GraphicsState::new();
    g.handle(&cmd("a=T,f=32,s=1,v=1", &rgba(1, 1)), &at(0, 0));
    let hidden = g.placements().last().unwrap().image_id;
    assert!(hidden >= 1 << 31 && g.image(hidden).unwrap().internal);
    let keys = format!("a=p,i={hidden}");
    let out = g.handle(&cmd(&keys, &[]), &at(1, 0));
    assert_eq!(
        reply(&out),
        Some(format!("\x1b_Gi={hidden};ENOENT:No such image\x1b\\"))
    );
    for delete in [
        format!("a=d,d=I,i={hidden}"),
        format!("a=d,d=R,x={hidden},y={hidden}"),
        "a=d,d=R,x=1,y=4294967295".to_string(),
    ] {
        g.handle(&cmd(&delete, &[]), &at(0, 0));
        assert_eq!(g.placements().count(), 1, "{delete}");
        assert!(g.image(hidden).is_some(), "{delete}");
    }
    // Transmitting with that id moves the internal image (and its
    // placement) to a fresh id instead of replacing it.
    let keys = format!("a=t,f=32,s=2,v=1,i={hidden}");
    let out = g.handle(&cmd(&keys, &rgba(2, 1)), &at(0, 0));
    assert_eq!(reply(&out), Some(format!("\x1b_Gi={hidden};OK\x1b\\")));
    let client = g.image(hidden).unwrap();
    assert_eq!((client.width, client.internal), (2, false));
    assert_eq!(g.image_count(), 2);
    let moved = g.placements().last().unwrap().image_id;
    assert_ne!(moved, hidden);
    let image = g.image(moved).unwrap();
    assert_eq!((image.id, image.width, image.internal), (moved, 1, true));
    // A position delete still removes it (it is on the screen).
    g.handle(&cmd("a=d,d=A", &[]), &at(0, 0));
    assert!(g.image(moved).is_none());
    // A numbered image's id is told to the client: it is addressable.
    let out = g.handle(&cmd("a=t,f=32,s=1,v=1,I=3", &rgba(1, 1)), &at(0, 0));
    let text = reply(&out).unwrap();
    let id: u32 = text["\x1b_Gi=".len()..text.find(',').unwrap()]
        .parse()
        .unwrap();
    let keys = format!("a=p,i={id}");
    assert_eq!(
        reply(&g.handle(&cmd(&keys, &[]), &at(0, 0))),
        Some(format!("\x1b_Gi={id};OK\x1b\\"))
    );
}

/// File media with an offset (`O`) and size (`S`): the image is read from
/// the middle of the file (the reader seeks there).
#[test]
fn file_offset_and_size() {
    let dir = std::env::temp_dir().join(format!("nora-graphics-offset-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("packed");
    let mut bytes = b"junk-junk".to_vec();
    bytes.extend_from_slice(&rgba(2, 1));
    bytes.extend_from_slice(b"trailing");
    std::fs::write(&file, &bytes).unwrap();
    let mut g = GraphicsState::new();
    let keys = format!("a=t,t=f,s=2,v=1,i=1,O=9,S={}", rgba(2, 1).len());
    let out = g.handle(&cmd(&keys, file.to_string_lossy().as_bytes()), &at(0, 0));
    assert_eq!(reply(&out).as_deref(), Some("\x1b_Gi=1;OK\x1b\\"));
    assert_eq!(&*g.image(1).unwrap().pixels, rgba(2, 1).as_slice());
    let _ = std::fs::remove_dir_all(&dir);
}
