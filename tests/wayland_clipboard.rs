//! Native Wayland clipboard check (ADR-016), against a real compositor.
//!
//! Ignored by default: it needs a Wayland compositor with a keyboard seat
//! and `wl-copy`/`wl-paste`. `scripts/wayland-clipboard-check.sh` starts a
//! headless one and runs it. The test maps a window (one cleared wgpu
//! frame) so the compositor gives it keyboard focus, creates nora's
//! clipboard from the window's display handle, and checks:
//! 1. a selection owned by another client (`wl-copy`) is pasted;
//! 2. nora pastes its own copy (nora owns the selection);
//! 3. another client (`wl-paste`) reads nora's copy.
//!
//! A compositor accepts a new selection only with the serial of a recent
//! input event, as a real copy has (the button release ending a
//! selection, or the copy key). Checks 2 and 3 therefore wait for a key
//! press in the window and run only with `NORA_WAYLAND_COPY_CHECK=1`, in a
//! session where someone can press it; the headless run reports them as
//! skipped.

#![cfg(all(feature = "wayland", unix, not(target_os = "macos")))]

use std::process::{Command, Stdio};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use nora::clipboard::{ClipboardBackend, ClipboardKind, clipboard_kind, create_clipboard};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
use winit::platform::wayland::EventLoopBuilderExtWayland;
use winit::raw_window_handle::HasDisplayHandle;
use winit::window::{Window, WindowId};

/// How long the copy checks wait for a key press.
const KEY_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug)]
enum Outcome {
    Ok,
    Fail(String),
    Skipped(&'static str),
}

impl From<Result<(), String>> for Outcome {
    fn from(result: Result<(), String>) -> Self {
        result.map_or_else(Outcome::Fail, |()| Outcome::Ok)
    }
}

/// The checker's results: (check, outcome).
type Report = Vec<(&'static str, Outcome)>;

struct Probe {
    proxy: EventLoopProxy<Report>,
    copy_checks: bool,
    window: Option<Arc<Window>>,
    clipboard: Option<Arc<dyn ClipboardBackend>>,
    checker: Option<std::thread::JoinHandle<()>>,
    key_pressed: Option<mpsc::Sender<()>>,
    focused: bool,
    report: Option<Report>,
}

/// Present one cleared frame: a Wayland window is mapped (and can get
/// keyboard focus) only once its surface has a buffer.
fn present_one_frame(event_loop: &ActiveEventLoop, window: &Arc<Window>) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(
        Box::new(event_loop.owned_display_handle()),
    ));
    let surface = instance.create_surface(window.clone()).expect("surface");
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .expect("adapter");
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("device");
    let size = window.inner_size();
    let config = surface
        .get_default_config(&adapter, size.width.max(1), size.height.max(1))
        .expect("surface config");
    surface.configure(&device, &config);
    let (wgpu::CurrentSurfaceTexture::Success(frame)
    | wgpu::CurrentSurfaceTexture::Suboptimal(frame)) = surface.get_current_texture()
    else {
        panic!("no surface texture");
    };
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::default(),
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    }));
    queue.submit([encoder.finish()]);
    queue.present(frame);
}

fn expect_paste(clipboard: &dyn ClipboardBackend, want: &str) -> Result<(), String> {
    match clipboard.get_text() {
        Ok(text) if text == want => Ok(()),
        Ok(text) => Err(format!("pasted {text:?}, want {want:?}")),
        Err(err) => Err(err.to_string()),
    }
}

fn settle() {
    std::thread::sleep(Duration::from_millis(300));
}

fn paste_external(clipboard: &dyn ClipboardBackend) -> Outcome {
    let external = "nora-wayland-external";
    let copied = Command::new("timeout")
        .args(["3", "wl-copy", external])
        .status()
        .map_err(|err| err.to_string())
        .and_then(|status| {
            status
                .success()
                .then_some(())
                .ok_or(format!("wl-copy: {status}"))
        });
    settle();
    copied
        .and_then(|()| expect_paste(clipboard, external))
        .into()
}

/// Checks 2 and 3; `key` reports a key press in the window, whose serial
/// lets the compositor accept nora's selection.
fn copy_checks(clipboard: &dyn ClipboardBackend, key: &mpsc::Receiver<()>, report: &mut Report) {
    println!("press a key in the \"nora clipboard check\" window");
    if key.recv_timeout(KEY_WAIT).is_err() {
        report.push((
            "paste nora's own copy",
            Outcome::Fail("no key press".into()),
        ));
        return;
    }
    let own = "nora-wayland-own-copy";
    let stored = clipboard.set_text(own).map_err(|err| err.to_string());
    settle();
    report.push((
        "paste nora's own copy",
        stored.and_then(|()| expect_paste(clipboard, own)).into(),
    ));

    let read = Command::new("timeout")
        .args(["3", "wl-paste", "--no-newline"])
        .stderr(Stdio::inherit())
        .output()
        .map_err(|err| err.to_string())
        .and_then(|out| {
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            (out.status.success() && text == own)
                .then_some(())
                .ok_or(format!("wl-paste {}: {text:?}", out.status))
        });
    report.push(("another client reads nora's copy", read.into()));
}

impl ApplicationHandler<Report> for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes().with_title("nora clipboard check"))
                .expect("window"),
        );
        let display = window.display_handle().expect("display handle").as_raw();
        assert_eq!(clipboard_kind(&display), ClipboardKind::Wayland);
        // SAFETY: the display belongs to the event loop, which the test
        // drops only after this backend and the checker thread (joined
        // before the loop exits) are gone.
        self.clipboard = unsafe { create_clipboard(display) };
        present_one_frame(event_loop, &window);
        self.window = Some(window);

        let clipboard = self.clipboard.clone().expect("Wayland clipboard");
        let proxy = self.proxy.clone();
        let copy = self.copy_checks;
        let (key_tx, key) = mpsc::channel();
        self.key_pressed = Some(key_tx);
        self.checker = Some(std::thread::spawn(move || {
            // The clipboard's own seat devices see the focus independently
            // of winit; give them a moment after the window maps.
            std::thread::sleep(Duration::from_secs(1));
            let mut report = vec![(
                "paste another client's selection",
                paste_external(clipboard.as_ref()),
            )];
            if copy {
                copy_checks(clipboard.as_ref(), &key, &mut report);
            } else {
                let why = "needs a key press (set NORA_WAYLAND_COPY_CHECK=1 in a session)";
                report.push(("paste nora's own copy", Outcome::Skipped(why)));
                report.push(("another client reads nora's copy", Outcome::Skipped(why)));
            }
            let _ = proxy.send_event(report);
        }));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::Focused(focused) => self.focused |= focused,
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(key) = &self.key_pressed {
                    let _ = key.send(());
                }
            }
            WindowEvent::CloseRequested => event_loop.exit(),
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, report: Report) {
        if let Some(checker) = self.checker.take() {
            checker.join().expect("checker thread");
        }
        self.report = Some(report);
        event_loop.exit();
    }
}

#[test]
#[ignore = "needs a Wayland compositor: scripts/wayland-clipboard-check.sh"]
fn native_wayland_clipboard_round_trips() {
    let mut event_loop = EventLoop::<Report>::with_user_event()
        .with_wayland()
        .with_any_thread(true)
        .build()
        .expect("Wayland event loop");
    let mut probe = Probe {
        proxy: event_loop.create_proxy(),
        copy_checks: std::env::var_os("NORA_WAYLAND_COPY_CHECK").is_some_and(|v| v == "1"),
        window: None,
        clipboard: None,
        checker: None,
        key_pressed: None,
        focused: false,
        report: None,
    };
    event_loop
        .run_app_on_demand(&mut probe)
        .expect("event loop");
    println!("window focused: {}", probe.focused);
    let report = probe.report.take().expect("checks ran");
    // Drop the backend while the event loop (and its display) still lives.
    drop(probe);
    let mut failed = 0;
    for (check, outcome) in &report {
        match outcome {
            Outcome::Ok => println!("ok   {check}"),
            Outcome::Skipped(why) => println!("skip {check}: {why}"),
            Outcome::Fail(err) => {
                failed += 1;
                println!("FAIL {check}: {err}");
            }
        }
    }
    drop(event_loop);
    assert_eq!(failed, 0, "{failed} Wayland clipboard check(s) failed");
}
