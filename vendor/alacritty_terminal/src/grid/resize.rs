//! Grid resize and reflow.

use std::cmp::{Ordering, max, min};
use std::mem;

use crate::index::{Boundary, Column, Line};
use crate::term::cell::{Flags, ResetDiscriminant};

use crate::grid::row::Row;
use crate::grid::{Dimensions, Grid, GridCell};

// nora: where a reflowing width change moved the grid's cells, so a
// consumer holding `(line, column)` references (prompt markers, a
// selection, image placements) can keep them on their text. A global
// counter cannot express it: joining or splitting rows moves each line by
// the number of rows joined or split around it.
/// How a reflowing resize moved the grid's points.
///
/// Recorded by [`Grid::resize`] when the width changed with reflow, and
/// taken with [`Grid::take_reflow_map`]. Its size is bounded by the
/// number of rows the reflow joined or split (rows that only moved with
/// their neighbours add nothing).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReflowMap {
    /// Breakpoints in buffer rows counted from the oldest row (0), sorted
    /// by `old`. A point `(row, col)` maps through the last breakpoint at
    /// or before it: on the breakpoint's own row the column moves by the
    /// same amount, a later row keeps its column and moves by the rows
    /// between them. Points before the first breakpoint keep their row.
    breaks: Vec<ReflowBreak>,
    /// History rows before and after (buffer row `n` is grid line
    /// `n - history`); `new_history` counts rows the history limit then
    /// dropped, so their points land above the topmost line.
    old_history: i64,
    new_history: i64,
    /// The grid size after the resize.
    lines: usize,
    columns: usize,
    /// The grid's `total_scrolled`/`total_pulled` counters after the
    /// resize (the reflow itself changes neither).
    total_scrolled: u64,
    total_pulled: u64,
}

// nora: one breakpoint of a [`ReflowMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReflowBreak {
    old: (usize, usize),
    new: (usize, usize),
}

// nora: see [`ReflowMap`].
impl ReflowMap {
    /// Where the buffer point `old` lands under the breakpoints so far.
    fn lookup(&self, old: (usize, usize)) -> (usize, usize) {
        let index = self.breaks.partition_point(|b| b.old <= old);
        match index.checked_sub(1).map(|i| self.breaks[i]) {
            None => old,
            Some(b) if b.old.0 == old.0 => (b.new.0, old.1 - b.old.1 + b.new.1),
            Some(b) => (b.new.0 + (old.0 - b.old.0), old.1),
        }
    }

    /// Record that the buffer point `old` (and the cells after it in its
    /// row) moved to `new`. Points are recorded in increasing order; one
    /// the breakpoints so far already imply is not stored.
    fn record(&mut self, old: (usize, usize), new: (usize, usize)) {
        match self.breaks.last_mut() {
            Some(last) if last.old == old => last.new = new,
            Some(last) => {
                debug_assert!(last.old < old);
                if self.lookup(old) != new {
                    self.breaks.push(ReflowBreak { old, new });
                }
            },
            None if old != new => self.breaks.push(ReflowBreak { old, new }),
            None => (),
        }
    }

    /// Whether every point keeps its grid position.
    fn is_identity(&self) -> bool {
        self.breaks.is_empty() && self.old_history == self.new_history
    }

    /// Where the grid point (`line`, `column`) before the reflow is after
    /// it. A point above the history stays above it, and a point the
    /// history limit dropped lands above the new topmost line; the result
    /// is otherwise clamped to the grid (the column to the last one, for a
    /// point in blanks the reflow dropped).
    pub fn map(&self, line: i32, column: usize) -> (i32, usize) {
        let row = i64::from(line) + self.old_history;
        let (row, column) = match usize::try_from(row) {
            Ok(row) => {
                let (row, column) = self.lookup((row, column));
                (row as i64, column)
            },
            Err(_) => (row, column),
        };
        let line = (row - self.new_history).min(self.lines as i64 - 1);
        let line = line.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        (line, column.min(self.columns.saturating_sub(1)))
    }

    /// The number of breakpoints stored (the map's size).
    pub fn len(&self) -> usize {
        self.breaks.len()
    }

    /// Whether no breakpoint is stored.
    pub fn is_empty(&self) -> bool {
        self.breaks.is_empty()
    }

    /// The grid's `total_scrolled` counter after the resize.
    pub fn total_scrolled(&self) -> u64 {
        self.total_scrolled
    }

    /// The grid's `total_pulled` counter after the resize.
    pub fn total_pulled(&self) -> u64 {
        self.total_pulled
    }
}

impl<T: GridCell + Default + PartialEq> Grid<T> {
    /// Resize the grid's width and/or height.
    pub fn resize<D>(&mut self, reflow: bool, lines: usize, columns: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        // Use empty template cell for resetting cells due to resize.
        let template = mem::take(&mut self.cursor.template);

        // nora: a map left from an earlier resize no longer applies.
        self.reflow_map = None;

        match self.lines.cmp(&lines) {
            Ordering::Less => self.grow_lines(lines),
            Ordering::Greater => self.shrink_lines(lines),
            Ordering::Equal => (),
        }

        match self.columns.cmp(&columns) {
            Ordering::Less => self.grow_columns(reflow, columns),
            Ordering::Greater => self.shrink_columns(reflow, columns),
            Ordering::Equal => (),
        }

        // nora: the counters the map's grid lines go with.
        if let Some(map) = self.reflow_map.as_mut() {
            map.total_scrolled = self.total_scrolled;
            map.total_pulled = self.total_pulled;
        }

        // Restore template cell.
        self.cursor.template = template;
    }

    /// Add lines to the visible area.
    ///
    /// Alacritty keeps the cursor at the bottom of the terminal as long as there
    /// is scrollback available. Once scrollback is exhausted, new lines are
    /// simply added to the bottom of the screen.
    fn grow_lines<D>(&mut self, target: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        let lines_added = target - self.lines;

        // Need to resize before updating buffer.
        self.raw.grow_visible_lines(target);
        self.lines = target;

        let history_size = self.history_size();
        let from_history = min(history_size, lines_added);

        // Move existing lines up for every line that couldn't be pulled from history.
        if from_history != lines_added {
            let delta = lines_added - from_history;
            self.scroll_up(&(Line(0)..Line(target as i32)), delta);

            // nora: this scroll only undoes part of the shift that growing the
            // visible area applied (the net shift is `from_history`, counted
            // in `total_pulled` below); no line went into history, so it must
            // not count as scrolled or every stable line reference would read
            // `delta` lines too high.
            self.total_scrolled -= delta as u64;
        }

        // Move cursor down for every line pulled from history.
        self.saved_cursor.point.line += from_history;
        self.cursor.point.line += from_history;

        // Account for the lines pulled from history: they shift every
        // existing line down without a full-screen scroll, so stable line
        // references (e.g. OSC 133 prompt markers) must add this back.
        self.total_pulled += from_history as u64;

        self.display_offset = self.display_offset.saturating_sub(lines_added);
        self.decrease_scroll_limit(lines_added);
    }

    /// Remove lines from the visible area.
    ///
    /// The behavior in Terminal.app and iTerm.app is to keep the cursor at the
    /// bottom of the screen. This is achieved by pushing history "out the top"
    /// of the terminal window.
    ///
    /// Alacritty takes the same approach.
    fn shrink_lines<D>(&mut self, target: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        // Scroll up to keep content inside the window.
        let required_scrolling = (self.cursor.point.line.0 as usize + 1).saturating_sub(target);
        if required_scrolling > 0 {
            self.scroll_up(&(Line(0)..Line(self.lines as i32)), required_scrolling);

            // Clamp cursors to the new viewport size.
            self.cursor.point.line = min(self.cursor.point.line, Line(target as i32 - 1));
        }

        // Clamp saved cursor, since only primary cursor is scrolled into viewport.
        self.saved_cursor.point.line = min(self.saved_cursor.point.line, Line(target as i32 - 1));

        self.raw.rotate((self.lines - target) as isize);
        self.raw.shrink_visible_lines(target);
        self.lines = target;
    }

    /// Grow number of columns in each row, reflowing if necessary.
    fn grow_columns(&mut self, reflow: bool, columns: usize) {
        // Check if a row needs to be wrapped.
        let should_reflow = |row: &Row<T>| -> bool {
            let len = Column(row.len());
            reflow && len.0 > 0 && len < columns && row[len - 1].flags().contains(Flags::WRAPLINE)
        };

        self.columns = columns;

        let mut reversed: Vec<Row<T>> = Vec::with_capacity(self.raw.len());
        let mut cursor_line_delta = 0;

        // Remove the linewrap special case, by moving the cursor outside of the grid.
        if self.cursor.input_needs_wrap && reflow {
            self.cursor.input_needs_wrap = false;
            self.cursor.point.column += 1;
        }

        let mut rows = self.raw.take_all();

        // nora: where each old row's cells go (buffer rows from the oldest).
        let old_len = rows.len();
        let mut map = reflow.then(ReflowMap::default);

        for (i, mut row) in rows.drain(..).enumerate().rev() {
            // nora: this row's buffer index from the oldest, and the index
            // of the next new row.
            let old_row = old_len - 1 - i;
            let new_row = reversed.len();

            // Check if reflowing should be performed.
            let last_row = match reversed.last_mut() {
                Some(last_row) if should_reflow(last_row) => last_row,
                _ => {
                    if let Some(map) = map.as_mut() {
                        map.record((old_row, 0), (new_row, 0));
                    }
                    reversed.push(row);
                    continue;
                },
            };

            // Remove wrap flag before appending additional cells.
            if let Some(cell) = last_row.last_mut() {
                cell.flags_mut().remove(Flags::WRAPLINE);
            }

            // Remove leading spacers when reflowing wide char to the previous line.
            let mut last_len = last_row.len();
            if last_len >= 1
                && last_row[Column(last_len - 1)].flags().contains(Flags::LEADING_WIDE_CHAR_SPACER)
            {
                last_row.shrink(last_len - 1);
                last_len -= 1;
            }

            // Don't try to pull more cells from the next line than available.
            let mut num_wrapped = columns - last_len;
            let len = min(row.len(), num_wrapped);

            // nora: the row's first `moved` cells join the previous new row
            // after its `last_len` cells; the rest start the next one.
            let mut moved = len;

            // Insert leading spacer when there's not enough room for reflowing wide char.
            let mut cells = if row[Column(len - 1)].flags().contains(Flags::WIDE_CHAR) {
                num_wrapped -= 1;
                moved = len - 1;

                let mut cells = row.front_split_off(len - 1);

                let mut spacer = T::default();
                spacer.flags_mut().insert(Flags::LEADING_WIDE_CHAR_SPACER);
                cells.push(spacer);

                cells
            } else {
                row.front_split_off(len)
            };

            // Add removed cells to previous row and reflow content.
            last_row.append(&mut cells);

            // nora: see `moved`.
            if let Some(map) = map.as_mut() {
                map.record((old_row, 0), (new_row - 1, last_len));
            }

            let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;

            if i == cursor_buffer_line && reflow {
                // Resize cursor's line and reflow the cursor if necessary.
                let mut target = self.cursor.point.sub(self, Boundary::Cursor, num_wrapped);

                // Clamp to the last column, if no content was reflown with the cursor.
                if target.column.0 == 0 && row.is_clear() {
                    self.cursor.input_needs_wrap = true;
                    target = target.sub(self, Boundary::Cursor, 1);
                }
                self.cursor.point.column = target.column;

                // Get required cursor line changes. Since `num_wrapped` is smaller than `columns`
                // this will always be either `0` or `1`.
                let line_delta = self.cursor.point.line - target.line;

                if line_delta != 0 && row.is_clear() {
                    continue;
                }

                cursor_line_delta += line_delta.0 as usize;
            } else if row.is_clear() {
                if i < self.display_offset {
                    // Since we removed a line, rotate down the viewport.
                    self.display_offset = self.display_offset.saturating_sub(1);
                }

                // Rotate cursor down if content below them was pulled from history.
                if i < cursor_buffer_line {
                    self.cursor.point.line += 1;
                }

                // Don't push line into the new buffer.
                continue;
            }

            if let Some(cell) = last_row.last_mut() {
                // Set wrap flag if next line still has cells.
                cell.flags_mut().insert(Flags::WRAPLINE);
            }

            // nora: see `moved`.
            if let Some(map) = map.as_mut() {
                map.record((old_row, moved), (new_row, 0));
            }

            reversed.push(row);
        }

        // Make sure we have at least the viewport filled.
        if reversed.len() < self.lines {
            let delta = (self.lines - reversed.len()) as i32;
            self.cursor.point.line = max(self.cursor.point.line - delta, Line(0));
            reversed.resize_with(self.lines, || Row::new(columns));
        }

        // Pull content down to put cursor in correct position, or move cursor up if there's no
        // more lines to delete below the cursor.
        if cursor_line_delta != 0 {
            let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;
            let available = min(cursor_buffer_line, reversed.len() - self.lines);
            let overflow = cursor_line_delta.saturating_sub(available);
            reversed.truncate(reversed.len() + overflow - cursor_line_delta);
            self.cursor.point.line = max(self.cursor.point.line - overflow, Line(0));
        }

        // Reverse iterator and fill all rows that are still too short.
        let mut new_raw = Vec::with_capacity(reversed.len());
        for mut row in reversed.drain(..).rev() {
            if row.len() < columns {
                row.grow(columns);
            }
            new_raw.push(row);
        }

        // nora: the rows above the bottom keep their buffer index; the
        // rows removed below the cursor clamp to the bottom line.
        if let Some(mut map) = map {
            map.old_history = old_len as i64 - self.lines as i64;
            map.new_history = new_raw.len() as i64 - self.lines as i64;
            map.lines = self.lines;
            map.columns = columns;
            if !map.is_identity() {
                self.reflow_map = Some(map);
            }
        }

        self.raw.replace_inner(new_raw);

        // Clamp display offset in case lines above it got merged.
        self.display_offset = min(self.display_offset, self.history_size());
    }

    /// Shrink number of columns in each row, reflowing if necessary.
    fn shrink_columns(&mut self, reflow: bool, columns: usize) {
        self.columns = columns;

        // Remove the linewrap special case, by moving the cursor outside of the grid.
        if self.cursor.input_needs_wrap && reflow {
            self.cursor.input_needs_wrap = false;
            self.cursor.point.column += 1;
        }

        let mut new_raw = Vec::with_capacity(self.raw.len());
        let mut buffered: Option<Vec<T>> = None;

        let mut rows = self.raw.take_all();

        // nora: where each old row's cells go (buffer rows from the oldest).
        // `segments` are the old sources of the row being split (`row`
        // after the cells buffered from the row above): `(position, old
        // row, old column)` from position 0; `carried` are those of the
        // buffered cells.
        let old_len = rows.len();
        let mut map = reflow.then(ReflowMap::default);
        let mut segments: Vec<(usize, usize, usize)> = Vec::new();
        let mut carried: Vec<(usize, usize, usize)> = Vec::new();

        for (i, mut row) in rows.drain(..).enumerate().rev() {
            // nora: see `segments`; `start` is the position of `row`'s first
            // cell while it is split into new rows.
            let old_row = old_len - 1 - i;
            let buffered_len = buffered.as_ref().map_or(0, Vec::len);
            segments.clear();
            segments.extend(carried.drain(..).filter(|s| s.0 < buffered_len));
            segments.push((buffered_len, old_row, 0));
            let mut start = 0;

            // Append lines left over from the previous row.
            if let Some(buffered) = buffered.take() {
                // Add a column for every cell added before the cursor, if it goes beyond the new
                // width it is then later reflown.
                let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;
                if i == cursor_buffer_line {
                    self.cursor.point.column += buffered.len();
                }

                row.append_front(buffered);
            }

            loop {
                // Remove all cells which require reflowing.
                let mut wrapped = match row.shrink(columns) {
                    Some(wrapped) if reflow => wrapped,
                    _ => {
                        let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;
                        if reflow && i == cursor_buffer_line && self.cursor.point.column > columns {
                            // If there are empty cells before the cursor, we assume it is explicit
                            // whitespace and need to wrap it like normal content.
                            Vec::new()
                        } else {
                            // nora: the rest of the row stays together.
                            if let Some(map) = map.as_mut() {
                                record_chunk(map, &segments, start, None, new_raw.len());
                            }

                            // Since it fits, just push the existing line without any reflow.
                            new_raw.push(row);
                            break;
                        }
                    },
                };

                // nora: the number of cells of `row` this new row keeps.
                let mut kept = columns;

                // Insert spacer if a wide char would be wrapped into the last column.
                if row.len() >= columns
                    && row[Column(columns - 1)].flags().contains(Flags::WIDE_CHAR)
                {
                    let mut spacer = T::default();
                    spacer.flags_mut().insert(Flags::LEADING_WIDE_CHAR_SPACER);

                    let wide_char = mem::replace(&mut row[Column(columns - 1)], spacer);
                    wrapped.insert(0, wide_char);
                    kept = columns - 1;
                }

                // Remove wide char spacer before shrinking.
                let len = wrapped.len();
                if len > 0 && wrapped[len - 1].flags().contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                    if len == 1 {
                        // nora: only a spacer was left over.
                        if let Some(map) = map.as_mut() {
                            record_chunk(map, &segments, start, None, new_raw.len());
                        }

                        row[Column(columns - 1)].flags_mut().insert(Flags::WRAPLINE);
                        new_raw.push(row);
                        break;
                    } else {
                        // Remove the leading spacer from the end of the wrapped row.
                        wrapped[len - 2].flags_mut().insert(Flags::WRAPLINE);
                        wrapped.truncate(len - 1);
                    }
                }

                // nora: the cells after `kept` follow in `wrapped`.
                if let Some(map) = map.as_mut() {
                    record_chunk(map, &segments, start, Some(kept), new_raw.len());
                }

                new_raw.push(row);

                // Set line as wrapped if cells got removed.
                if let Some(cell) = new_raw.last_mut().and_then(|r| r.last_mut()) {
                    cell.flags_mut().insert(Flags::WRAPLINE);
                }

                if wrapped
                    .last()
                    .map(|c| c.flags().contains(Flags::WRAPLINE) && i >= 1)
                    .unwrap_or(false)
                    && wrapped.len() < columns
                {
                    // Make sure previous wrap flag doesn't linger around.
                    if let Some(cell) = wrapped.last_mut() {
                        cell.flags_mut().remove(Flags::WRAPLINE);
                    }

                    // nora: the buffered cells' sources move to the next row.
                    carry_segments(&segments, start + kept, &mut carried);

                    // Add removed cells to start of next row.
                    buffered = Some(wrapped);
                    break;
                } else {
                    // Reflow cursor if a line below it is deleted.
                    let cursor_buffer_line = self.lines - self.cursor.point.line.0 as usize - 1;
                    if (i == cursor_buffer_line && self.cursor.point.column < columns)
                        || i < cursor_buffer_line
                    {
                        self.cursor.point.line = max(self.cursor.point.line - 1, Line(0));
                    }

                    // Reflow the cursor if it is on this line beyond the width.
                    if i == cursor_buffer_line && self.cursor.point.column >= columns {
                        // Since only a single new line is created, we subtract only `columns`
                        // from the cursor instead of reflowing it completely.
                        self.cursor.point.column -= columns;
                    }

                    // Make sure new row is at least as long as new width.
                    let occ = wrapped.len();
                    if occ < columns {
                        wrapped.resize_with(columns, T::default);
                    }
                    row = Row::from_vec(wrapped, occ);
                    start += kept;

                    if i < self.display_offset {
                        // Since we added a new line, rotate up the viewport.
                        self.display_offset += 1;
                    }
                }
            }
        }

        // nora: rows the history limit drops below land above the topmost
        // line (`new_history` counts them).
        if let Some(mut map) = map {
            map.old_history = old_len as i64 - self.lines as i64;
            map.new_history = new_raw.len() as i64 - self.lines as i64;
            map.lines = self.lines;
            map.columns = columns;
            if !map.is_identity() {
                self.reflow_map = Some(map);
            }
        }

        // Reverse iterator and use it as the new grid storage.
        let mut reversed: Vec<Row<T>> = new_raw.drain(..).rev().collect();
        reversed.truncate(self.max_scroll_limit + self.lines);
        self.raw.replace_inner(reversed);

        // Clamp display offset in case some lines went off.
        self.display_offset = min(self.display_offset, self.history_size());

        // Reflow the primary cursor, or clamp it if reflow is disabled.
        if !reflow {
            self.cursor.point.column = min(self.cursor.point.column, Column(columns - 1));
        } else if self.cursor.point.column == columns
            && !self[self.cursor.point.line][Column(columns - 1)].flags().contains(Flags::WRAPLINE)
        {
            self.cursor.input_needs_wrap = true;
            self.cursor.point.column -= 1;
        } else {
            self.cursor.point = self.cursor.point.grid_clamp(self, Boundary::Cursor);
        }

        // Clamp the saved cursor to the grid.
        self.saved_cursor.point.column = min(self.saved_cursor.point.column, Column(columns - 1));
    }
}

// nora: record in `map` that the cells of a row being split, from position
// `start` (up to `start + kept`, or all of them), now start the new row
// `new_row`. `segments` are the row's old sources, see `shrink_columns`.
fn record_chunk(
    map: &mut ReflowMap,
    segments: &[(usize, usize, usize)],
    start: usize,
    kept: Option<usize>,
    new_row: usize,
) {
    let end = kept.map_or(usize::MAX, |kept| start.saturating_add(kept));
    let first = segments.partition_point(|s| s.0 <= start).saturating_sub(1);
    for (k, &(pos, row, col)) in segments.iter().enumerate().skip(first) {
        if k == first {
            map.record((row, col + start.saturating_sub(pos)), (new_row, 0));
        } else if pos < end {
            map.record((row, col), (new_row, pos - start));
        }
    }
}

// nora: the sources of a split row's cells from position `from` on, from
// position 0 (the cells buffered into the next row).
fn carry_segments(
    segments: &[(usize, usize, usize)],
    from: usize,
    out: &mut Vec<(usize, usize, usize)>,
) {
    out.clear();
    let first = segments.partition_point(|s| s.0 <= from).saturating_sub(1);
    for (k, &(pos, row, col)) in segments.iter().enumerate().skip(first) {
        if k == first {
            out.push((0, row, col + from.saturating_sub(pos)));
        } else {
            out.push((pos - from, row, col));
        }
    }
}
