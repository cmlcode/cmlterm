use nix::{errno::Errno, unistd::ForkResult};
use std::{
    ffi::CStr,
    ops::Range,
    os::fd::{AsRawFd, OwnedFd},
};

use ansi::{AnsiParser, SelectGraphicRendition, TerminalOutput};

mod ansi;

/// Spawn a shell in a child process and return the file descriptor used for I/O
fn spawn_shell() -> OwnedFd {
    unsafe {
        let res = nix::pty::forkpty(None, None).unwrap();
        match res.fork_result {
            ForkResult::Parent { .. } => (),
            ForkResult::Child => {
                let shell_name = CStr::from_bytes_with_nul(b"bash\0")
                    .expect("Should always have null terminator");
                let args: &[&[u8]] = &[b"bash\0", b"--noprofile\0", b"--norc\0"];

                let args: Vec<&'static CStr> = args
                    .iter()
                    .map(|v| {
                        CStr::from_bytes_with_nul(v).expect("Should always have null terminator")
                    })
                    .collect::<Vec<_>>();

                // Temporary workaround to avoid rendering issues
                std::env::remove_var("PROMPT_COMMAND");
                std::env::set_var("PS1", "$ ");
                nix::unistd::execvp(shell_name, &args).unwrap();
                // Should never run
                std::process::exit(1);
            }
        }
        res.master
    }
}

fn update_cursor(incoming: &[u8], cursor: &mut CursorState) {
    for c in incoming {
        match c {
            b'\n' => {
                cursor.x = 0;
                cursor.y += 1;
            }
            _ => {
                cursor.x += 1;
            }
        }
    }
}

fn set_nonblock(fd: &OwnedFd) {
    let flags = nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL).unwrap();
    let mut flags =
        nix::fcntl::OFlag::from_bits(flags & nix::fcntl::OFlag::O_ACCMODE.bits()).unwrap();
    flags.set(nix::fcntl::OFlag::O_NONBLOCK, true);

    nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(flags)).unwrap();
}

fn cursor_to_buffer_position(cursor_pos: &CursorState, buf: &[u8]) -> usize {
    let line_start = buf
        .split(|b| *b == b'\n')
        .take(cursor_pos.y)
        .fold(0, |acc, item| acc + item.len() + 1);
    line_start + cursor_pos.x
}

/// Inserts data at position in buf, extending if necessary
fn insert_data_at_position(data: &[u8], pos: usize, buf: &mut Vec<u8>) {
    assert!(
        pos <= buf.len(),
        "assume pos is never more than 1 past the end of the buffer"
    );

    if pos >= buf.len() {
        assert_eq!(pos, buf.len());
        buf.extend_from_slice(data);
        return;
    }

    let amount_that_fits = buf.len() - pos;
    let (data_to_copy, data_to_push): (&[u8], &[u8]) = if amount_that_fits > data.len() {
        (&data, &[])
    } else {
        data.split_at(amount_that_fits)
    };

    buf[pos..pos + data_to_copy.len()].copy_from_slice(data_to_copy);
    buf.extend_from_slice(data_to_push);
}

fn delete_items_from_vec<T>(mut to_delete: Vec<usize>, vec: &mut Vec<T>) {
    to_delete.sort();
    for idx in to_delete.iter().rev() {
        vec.remove(*idx);
    }
}

struct ColorRangeAdjustment {
    should_delete: bool,
    to_insert: Option<ColorTag>,
}

fn range_fully_conatins(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start <= b.start && a.end >= b.end
}

fn range_starts_overlapping(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start > b.start && a.end > b.end
}

fn range_ends_overlapping(a: &Range<usize>, b: &Range<usize>) -> bool {
    range_starts_overlapping(b, a)
}

fn adjust_existing_color_range(
    existing_elem: &mut ColorTag,
    range: &Range<usize>,
) -> ColorRangeAdjustment {
    let mut ret = ColorRangeAdjustment {
        should_delete: false,
        to_insert: None,
    };

    let existing_range = existing_elem.start..existing_elem.end;
    if range_fully_conatins(range, &existing_range) {
        ret.should_delete = true;
    } else if range_fully_conatins(&existing_range, range) {
        if existing_elem.start == range.start {
            ret.should_delete = true;
        }

        if range.end != existing_elem.end {
            ret.to_insert = Some(ColorTag {
                start: range.end,
                end: existing_elem.end,
                color: existing_elem.color,
            });
        }

        existing_elem.end = range.start;
    } else if range_starts_overlapping(range, &existing_range) {
        existing_elem.end = range.start;
        if existing_elem.start == existing_elem.end {
            ret.should_delete = true;
        }
    } else if range_ends_overlapping(range, &existing_range) {
        existing_elem.start = range.end;
        if existing_elem.start == existing_elem.end {
            ret.should_delete = true;
        }
    } else {
        panic!(
            "Unhandled case {}-{}, {}-{}",
            existing_elem.start, existing_elem.end, range.start, range.end
        );
    }

    ret
}

fn adjust_existing_color_ranges(existing: &mut Vec<ColorTag>, range: &Range<usize>) {
    let mut effected_infos = existing
        .iter_mut()
        .enumerate()
        .filter(|(_i, item)| ranges_overlap(item.start..item.end, range.clone()))
        .collect::<Vec<_>>();

    let mut to_delete = Vec::new();
    let mut to_push = Vec::new();
    for info in &mut effected_infos {
        let adjustment = adjust_existing_color_range(info.1, range);
        if adjustment.should_delete {
            to_delete.push(info.0);
        }
        if let Some(item) = adjustment.to_insert {
            to_push.push(item);
        }
    }

    delete_items_from_vec(to_delete, existing);
    existing.extend(to_push);
}

#[derive(Clone)]
pub struct CursorState {
    pub x: usize,
    pub y: usize,
    color: TerminalColor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalColor {
    Default,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
}

impl TerminalColor {
    fn from_sgr(sgr: SelectGraphicRendition) -> Option<TerminalColor> {
        let ret = match sgr {
            SelectGraphicRendition::Reset => TerminalColor::Default,
            SelectGraphicRendition::Black => TerminalColor::Black,
            SelectGraphicRendition::Red => TerminalColor::Red,
            SelectGraphicRendition::Green => TerminalColor::Green,
            SelectGraphicRendition::Yellow => TerminalColor::Yellow,
            SelectGraphicRendition::Blue => TerminalColor::Blue,
            SelectGraphicRendition::Magenta => TerminalColor::Magenta,
            SelectGraphicRendition::Cyan => TerminalColor::Cyan,
            SelectGraphicRendition::White => TerminalColor::White,
            _ => return None,
        };

        Some(ret)
    }
}

fn ranges_overlap(a: Range<usize>, b: Range<usize>) -> bool {
    if a.end <= b.start {
        return false;
    }

    if a.start >= b.end {
        return false;
    }

    true
}

#[derive(Debug)]
struct ColorTag {
    pub start: usize,
    pub end: usize,
    pub color: TerminalColor,
}

struct ColorTracker {
    color_info: Vec<ColorTag>,
}

impl ColorTracker {
    fn new() -> ColorTracker {
        ColorTracker {
            color_info: vec![ColorTag {
                start: 0,
                end: usize::MAX,
                color: TerminalColor::Default,
            }],
        }
    }

    fn push_range(&mut self, cursor_color: TerminalColor, range: Range<usize>) {
        adjust_existing_color_ranges(&mut self.color_info, &range);

        self.color_info.push(ColorTag {
            start: range.start,
            end: range.end,
            color: cursor_color,
        });

        // FIXME: Insertion sort
        // FIXME: Merge adjacent
        self.color_info.sort_by(|a, b| a.start.cmp(&b.start));
    }

    fn colors(&self) -> Vec<(Range<usize>, TerminalColor)> {
        let mut output = Vec::new();
        for i in 0..self.color_info.len() {
            // FIXME: Track actual buffer len maybe?
            let end = self
                .color_info
                .get(i + 1)
                .map(|x| x.start)
                .unwrap_or(usize::MAX);
            let item = &self.color_info[i];
            output.push((item.start..end, item.color))
        }
        output
    }
}

pub struct TerminalEmulator {
    output_buf: AnsiParser,
    buf: Vec<u8>,
    color_tracker: ColorTracker,
    cursor_pos: CursorState,
    fd: OwnedFd,
}

impl TerminalEmulator {
    pub fn new() -> TerminalEmulator {
        let fd = spawn_shell();
        set_nonblock(&fd);

        TerminalEmulator {
            output_buf: AnsiParser::new(),
            buf: Vec::new(),
            color_tracker: ColorTracker::new(),
            cursor_pos: CursorState {
                x: 0,
                y: 0,
                color: TerminalColor::Default,
            },
            fd,
        }
    }

    pub fn write(&mut self, mut to_write: &[u8]) {
        while !to_write.is_empty() {
            let written = nix::unistd::write(self.fd.as_raw_fd(), to_write).unwrap();
            to_write = &to_write[written..];
        }
    }

    pub fn read(&mut self) {
        let mut buf = vec![0u8; 4096];
        let mut ret = Ok(0);
        while ret.is_ok() {
            ret = nix::unistd::read(self.fd.as_raw_fd(), &mut buf);
            let Ok(read_size) = ret else {
                break;
            };

            let incoming = &buf[0..read_size];
            let parsed = self.output_buf.push(incoming);
            for segment in parsed {
                match segment {
                    TerminalOutput::Data(data) => {
                        let output_start = cursor_to_buffer_position(&self.cursor_pos, &self.buf);
                        insert_data_at_position(&data, output_start, &mut self.buf);
                        self.color_tracker.push_range(
                            self.cursor_pos.color,
                            output_start..output_start + data.len(),
                        );
                        update_cursor(&data, &mut self.cursor_pos);
                    }
                    TerminalOutput::SetCursorPos { x, y } => {
                        if let Some(x) = x {
                            self.cursor_pos.x = x - 1;
                        }
                        if let Some(y) = y {
                            self.cursor_pos.y = y - 1;
                        }
                    }
                    TerminalOutput::ClearForwards => {
                        let buf_pos = cursor_to_buffer_position(&self.cursor_pos, &self.buf);
                        self.color_tracker
                            .push_range(self.cursor_pos.color, buf_pos..usize::MAX);
                        self.buf = self.buf[..buf_pos].to_vec();
                    }
                    TerminalOutput::ClearBackwards => {
                        // FIXME: Write a test to check expected behavior here, might expect
                        // existing content to stay in the same position
                        // FIXME: Track color
                        let buf_pos = cursor_to_buffer_position(&self.cursor_pos, &self.buf);
                        self.buf = self.buf[buf_pos..].to_vec();
                    }
                    TerminalOutput::ClearAll => {
                        self.color_tracker
                            .push_range(self.cursor_pos.color, 0..usize::MAX);
                        self.buf.clear();
                    }
                    TerminalOutput::Sgr(sgr) => {
                        if let Some(color) = TerminalColor::from_sgr(sgr) {
                            self.cursor_pos.color = color;
                        } else {
                            println!("Unhandled sgr: {:?}", sgr);
                        }
                    }
                    TerminalOutput::Invalid => {}
                }
            }
        }

        if let Err(e) = ret {
            if e != Errno::EAGAIN {
                println!("Failed to read: {e}");
            }
        }
    }

    pub fn data(&self) -> &[u8] {
        &self.buf
    }

    pub fn colored_data(&self) -> Vec<(Range<usize>, TerminalColor)> {
        self.color_tracker.colors()
    }

    pub fn cursor_pos(&self) -> CursorState {
        self.cursor_pos.clone()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_cursor_data_insert() {
        let mut buf = Vec::new();
        insert_data_at_position(b"asdf", 0, &mut buf);
        assert_eq!(buf, b"asdf");

        insert_data_at_position(b"123", 0, &mut buf);
        assert_eq!(buf, b"123f");

        insert_data_at_position(b"xyzw", 4, &mut buf);
        assert_eq!(buf, b"123fxyzw");

        insert_data_at_position(b"asdf", 2, &mut buf);
        assert_eq!(buf, b"12asdfzw");
    }

    #[test]
    fn basic_color_tracker_test() {
        let mut color_tracker = ColorTracker::new();

        color_tracker.push_range(TerminalColor::Yellow, 3..10);
        let colors = color_tracker.colors();
        assert_eq!(
            colors,
            &[
                (0..3, TerminalColor::Default),
                (3..10, TerminalColor::Yellow),
                (10..usize::MAX, TerminalColor::Default),
            ]
        );

        color_tracker.push_range(TerminalColor::Blue, 5..7);
        let colors = color_tracker.colors();
        assert_eq!(
            colors,
            &[
                (0..3, TerminalColor::Default),
                (3..5, TerminalColor::Yellow),
                (5..7, TerminalColor::Blue),
                (7..10, TerminalColor::Yellow),
                (10..usize::MAX, TerminalColor::Default),
            ]
        );

        color_tracker.push_range(TerminalColor::Green, 7..9);
        let colors = color_tracker.colors();
        assert_eq!(
            colors,
            &[
                (0..3, TerminalColor::Default),
                (3..5, TerminalColor::Yellow),
                (5..7, TerminalColor::Blue),
                (7..9, TerminalColor::Green),
                (9..10, TerminalColor::Yellow),
                (10..usize::MAX, TerminalColor::Default),
            ]
        );

        color_tracker.push_range(TerminalColor::Red, 6..11);
        let colors = color_tracker.colors();
        assert_eq!(
            colors,
            &[
                (0..3, TerminalColor::Default),
                (3..5, TerminalColor::Yellow),
                (5..6, TerminalColor::Blue),
                (6..11, TerminalColor::Red),
                (11..usize::MAX, TerminalColor::Default),
            ]
        );
    }

    #[test]
    fn test_range_overlap() {
        assert!(ranges_overlap(5..10, 7..9));
        assert!(ranges_overlap(5..10, 8..12));
        assert!(ranges_overlap(5..10, 3..6));
        assert!(ranges_overlap(5..10, 2..12));
        assert!(!ranges_overlap(5..10, 10..12));
        assert!(!ranges_overlap(5..10, 0..5));
    }
}
