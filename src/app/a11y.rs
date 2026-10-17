//! Screen readers (N37, ADR-015): the active pane's text as an AT-SPI2
//! tree through AccessKit's Unix adapter.
//!
//! After each frame the window builds a [`TextModel`] from the active
//! pane's snapshot: one line per visible row (trailing blanks trimmed,
//! `\n` between rows), with each character's byte length, position and
//! width, and the caret at the cursor (or the selection). While an
//! assistive technology has activated the adapter, a changed model is sent
//! as a tree: a window, a terminal and one text run per line. The adapter
//! derives the AT-SPI2 events (text changed, caret moved) from successive
//! trees. With no assistive technology the cost is a flag check.

use alacritty_terminal::term::cell::Flags;
use nora::engine::Snapshot;

/// The accessible text of one pane, in window pixels.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextModel {
    lines: Vec<Line>,
    /// The pane's top-left corner and cell size.
    origin: (f32, f32),
    cell: (f32, f32),
    columns: usize,
    /// Anchor and focus as (line, character index); equal for a caret.
    selection: Option<((usize, usize), (usize, usize))>,
    title: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Line {
    text: String,
    /// UTF-8 bytes of each character (a cell's cluster).
    lengths: Vec<u8>,
    /// Each character's left edge and width, from the line's start.
    positions: Vec<f32>,
    widths: Vec<f32>,
    /// The grid column each character starts at.
    columns: Vec<usize>,
}

impl Line {
    /// Characters before the newline.
    fn visible_len(&self) -> usize {
        self.columns.len()
    }

    /// The character index of grid column `col` (the end when past it).
    fn index_of(&self, col: usize) -> usize {
        self.columns
            .iter()
            .position(|&c| c >= col)
            .unwrap_or(self.visible_len())
    }
}

impl TextModel {
    /// The model of `snap`, drawn with its top-left cell at `origin` and
    /// cells of `cell` pixels.
    pub fn of(snap: &Snapshot, origin: (f32, f32), cell: (f32, f32), title: &str) -> Self {
        let (cw, _) = cell;
        let mut lines = Vec::with_capacity(snap.rows);
        for row in 0..snap.rows {
            let mut line = Line::default();
            for cell in snap.cells.iter().skip(row * snap.cols).take(snap.cols) {
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let mut cluster = String::new();
                if cell.ch == nora::graphics_image::PLACEHOLDER {
                    cluster.push(' ');
                } else {
                    cluster.push(cell.ch);
                    for &mark in &cell.zerowidth {
                        if cluster.len() + mark.len_utf8() > usize::from(u8::MAX) {
                            break;
                        }
                        cluster.push(mark);
                    }
                }
                let span = if cell.flags.contains(Flags::WIDE_CHAR) {
                    2.0
                } else {
                    1.0
                };
                line.text.push_str(&cluster);
                line.lengths.push(cluster.len() as u8);
                line.positions.push(cell.col as f32 * cw);
                line.widths.push(span * cw);
                line.columns.push(cell.col);
            }
            // Trailing blanks are not text, except before the cursor on its
            // row: a typed space is reported and the caret stays on it.
            let keep_before = match snap.cursor {
                Some((cursor_row, cursor_col)) if cursor_row == row => cursor_col,
                _ => 0,
            };
            while line.text.ends_with(' ')
                && line.lengths.last() == Some(&1)
                && line.columns.last().is_some_and(|&col| col >= keep_before)
            {
                line.text.pop();
                line.lengths.pop();
                line.positions.pop();
                line.widths.pop();
                line.columns.pop();
            }
            lines.push(line);
        }
        // Rows are joined by a newline at the end of each but the last.
        let last = lines.len().saturating_sub(1);
        for line in lines.iter_mut().take(last) {
            let end = line
                .positions
                .last()
                .zip(line.widths.last())
                .map_or(0.0, |(p, w)| p + w);
            line.text.push('\n');
            line.lengths.push(1);
            line.positions.push(end);
            line.widths.push(0.0);
        }
        let position = |lines: &[Line], row: usize, col: usize| {
            let row = row.min(lines.len().saturating_sub(1));
            (row, lines.get(row).map_or(0, |l| l.index_of(col)))
        };
        let selected: Vec<usize> = snap
            .selected
            .iter()
            .enumerate()
            .filter(|(_, s)| **s)
            .map(|(i, _)| i)
            .collect();
        let selection = match (selected.first(), selected.last()) {
            (Some(&first), Some(&last)) if snap.cols > 0 => Some((
                position(&lines, first / snap.cols, first % snap.cols),
                position(&lines, last / snap.cols, last % snap.cols + 1),
            )),
            _ => snap.cursor.map(|(row, col)| {
                let at = position(&lines, row, col);
                (at, at)
            }),
        };
        Self {
            lines,
            origin,
            cell,
            columns: snap.cols,
            selection,
            title: title.to_string(),
        }
    }

    /// The text of all lines (tests and traces).
    #[cfg(test)]
    pub fn text(&self) -> String {
        self.lines.iter().map(|l| l.text.as_str()).collect()
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use accesskit::{
        ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, Node, NodeId, Rect,
        Role, TextPosition, TextSelection, TreeId, TreeInfo, TreeUpdate,
    };
    use winit::event_loop::EventLoopProxy;

    use super::TextModel;
    use nora::session::pump::AppEvent;

    const WINDOW: NodeId = NodeId(0);
    const TERMINAL: NodeId = NodeId(1);
    fn line_id(index: usize) -> NodeId {
        NodeId(2 + index as u64)
    }

    struct Activation {
        active: Arc<AtomicBool>,
        requested: Arc<AtomicBool>,
        proxy: EventLoopProxy<AppEvent>,
    }

    impl ActivationHandler for Activation {
        fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
            // Built on the window thread, which has the text: it sends the
            // full tree when woken.
            self.active.store(true, Ordering::Release);
            self.requested.store(true, Ordering::Release);
            let _ = self.proxy.send_event(AppEvent::Accessibility);
            None
        }
    }

    struct Deactivation(Arc<AtomicBool>);

    impl DeactivationHandler for Deactivation {
        fn deactivate_accessibility(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }

    /// Actions (focus, set selection) are not taken: the text follows the
    /// terminal, not the screen reader.
    struct NoActions;

    impl ActionHandler for NoActions {
        fn do_action(&mut self, _request: ActionRequest) {}
    }

    pub struct Accessibility {
        adapter: accesskit_unix::Adapter,
        active: Arc<AtomicBool>,
        requested: Arc<AtomicBool>,
        last: Option<TextModel>,
    }

    impl Accessibility {
        pub fn new(proxy: EventLoopProxy<AppEvent>) -> Self {
            let active = Arc::new(AtomicBool::new(false));
            let requested = Arc::new(AtomicBool::new(false));
            let adapter = accesskit_unix::Adapter::new(
                Activation {
                    active: Arc::clone(&active),
                    requested: Arc::clone(&requested),
                    proxy,
                },
                NoActions,
                Deactivation(Arc::clone(&active)),
            );
            Self {
                adapter,
                active,
                requested,
                last: None,
            }
        }

        /// Whether an assistive technology is listening (build models only
        /// then).
        pub fn is_active(&self) -> bool {
            self.active.load(Ordering::Acquire)
        }

        /// Send `model` if it changed, or the full tree if one was asked for.
        pub fn update(&mut self, model: TextModel) {
            let requested = self.requested.swap(false, Ordering::AcqRel);
            if !requested && self.last.as_ref() == Some(&model) {
                return;
            }
            self.adapter.update_if_active(|| tree(&model));
            self.last = Some(model);
        }

        /// The adapter asked for the tree: send the last model again.
        pub fn resend(&mut self) {
            if self.requested.swap(false, Ordering::AcqRel) {
                let model = self.last.clone().unwrap_or_default();
                self.adapter.update_if_active(|| tree(&model));
            }
        }

        pub fn set_focused(&mut self, focused: bool) {
            self.adapter.update_window_focus_state(focused);
        }

        /// The window's outer and inner rectangles on the screen.
        pub fn set_bounds(&mut self, outer: [f64; 4], inner: [f64; 4]) {
            let rect = |r: [f64; 4]| Rect {
                x0: r[0],
                y0: r[1],
                x1: r[0] + r[2],
                y1: r[1] + r[3],
            };
            self.adapter
                .set_root_window_bounds(rect(outer), rect(inner));
        }
    }

    /// The whole tree for `model`.
    pub(super) fn tree(model: &TextModel) -> TreeUpdate {
        let (ox, oy) = (f64::from(model.origin.0), f64::from(model.origin.1));
        let (cw, ch) = (f64::from(model.cell.0), f64::from(model.cell.1));
        let width = model.columns as f64 * cw;
        let mut nodes = Vec::with_capacity(model.lines.len() + 2);
        let mut window = Node::new(Role::Window);
        window.set_label("nora");
        window.set_children(vec![TERMINAL]);
        nodes.push((WINDOW, window));
        let mut terminal = Node::new(Role::Terminal);
        terminal.set_label(if model.title.is_empty() {
            "Terminal".to_string()
        } else {
            model.title.clone()
        });
        terminal.set_children((0..model.lines.len()).map(line_id).collect::<Vec<_>>());
        terminal.set_bounds(Rect {
            x0: ox,
            y0: oy,
            x1: ox + width,
            y1: oy + model.lines.len() as f64 * ch,
        });
        if let Some(((al, ac), (fl, fc))) = model.selection {
            terminal.set_text_selection(TextSelection {
                anchor: TextPosition {
                    node: line_id(al),
                    character_index: ac,
                },
                focus: TextPosition {
                    node: line_id(fl),
                    character_index: fc,
                },
            });
        }
        nodes.push((TERMINAL, terminal));
        for (index, line) in model.lines.iter().enumerate() {
            let mut run = Node::new(Role::TextRun);
            let y0 = oy + index as f64 * ch;
            run.set_bounds(Rect {
                x0: ox,
                y0,
                x1: ox + width,
                y1: y0 + ch,
            });
            run.set_value(line.text.clone());
            run.set_character_lengths(line.lengths.clone());
            run.set_character_positions(line.positions.clone());
            run.set_character_widths(line.widths.clone());
            nodes.push((line_id(index), run));
        }
        let mut info = TreeInfo::new(WINDOW);
        info.toolkit_name = Some("nora".to_string());
        info.toolkit_version = Some(env!("CARGO_PKG_VERSION").to_string());
        TreeUpdate {
            nodes,
            tree: Some(info),
            tree_id: TreeId::ROOT,
            focus: TERMINAL,
        }
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
mod platform {
    use super::TextModel;
    use nora::session::pump::AppEvent;
    use winit::event_loop::EventLoopProxy;

    /// No accessibility provider on this platform yet.
    pub struct Accessibility;

    impl Accessibility {
        pub fn new(_proxy: EventLoopProxy<AppEvent>) -> Self {
            Self
        }
        pub fn is_active(&self) -> bool {
            false
        }
        pub fn update(&mut self, _model: TextModel) {}
        pub fn resend(&mut self) {}
        pub fn set_focused(&mut self, _focused: bool) {}
        pub fn set_bounds(&mut self, _outer: [f64; 4], _inner: [f64; 4]) {}
    }
}

pub use platform::Accessibility;

#[cfg(test)]
mod tests {
    use super::*;
    use nora::engine::Engine;

    fn model(bytes: &[u8]) -> TextModel {
        let mut engine = Engine::new(3, 10);
        engine.feed(bytes);
        TextModel::of(&engine.snapshot(), (0.0, 0.0), (8.0, 16.0), "")
    }

    #[test]
    fn rows_become_lines_without_trailing_blanks() {
        let m = model(b"ab  \r\n\r\n  c");
        assert_eq!(m.text(), "ab\n\n  c");
        assert_eq!(m.lines[0].lengths, vec![1, 1, 1]);
        assert_eq!(m.lines[2].positions, vec![0.0, 8.0, 16.0]);
        assert_eq!(m.selection, Some(((2, 3), (2, 3))), "caret after the c");
    }

    #[test]
    fn wide_and_combining_characters_are_one_character() {
        let m = model("日x e\u{301}".as_bytes());
        assert_eq!(m.lines[0].text, "日x e\u{301}\n");
        assert_eq!(m.lines[0].lengths, vec![3, 1, 1, 3, 1]);
        assert_eq!(m.lines[0].positions[..4], [0.0, 16.0, 24.0, 32.0]);
        assert_eq!(
            m.lines[0].widths[0], 16.0,
            "a wide character spans two cells"
        );
    }

    #[test]
    fn the_caret_follows_the_cursor_and_past_the_text_ends_the_line() {
        let m = model(b"abc\x1b[1;2H");
        assert_eq!(m.selection, Some(((0, 1), (0, 1))));
        let m = model(b"ab\x1b[1;8H");
        assert_eq!(
            m.selection,
            Some(((0, 7), (0, 7))),
            "blanks before the cursor are kept, so the caret is on it"
        );
        assert_eq!(m.lines[0].text, "ab     \n");
        // Past the cursor, and on other rows, trailing blanks are trimmed.
        let m = model(b"ab  \r\nls \x1b[2;4H");
        assert_eq!(m.text(), "ab\nls \n");
        assert_eq!(m.selection, Some(((1, 3), (1, 3))), "caret after the space");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn the_tree_is_a_window_a_terminal_and_its_lines() {
        let m = model(b"hi\r\nthere");
        let update = platform::tree(&m);
        assert_eq!(update.nodes.len(), 2 + 3);
        let (_, terminal) = &update.nodes[1];
        assert_eq!(terminal.role(), accesskit::Role::Terminal);
        assert_eq!(terminal.children().len(), 3);
        let (_, first) = &update.nodes[2];
        assert_eq!(first.role(), accesskit::Role::TextRun);
        assert_eq!(first.value(), Some("hi\n"));
    }
}
