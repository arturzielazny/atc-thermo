//! Kitty graphics protocol: images handed over in shared memory (Linux) or a temporary
//! file (macOS and others without `/dev/shm`) and shown with Unicode placeholders, so
//! they occupy ordinary buffer cells.
//!
//! Only a short object name or path passes through the terminal, instead of megabytes
//! of base64. That needs the terminal on the same machine, which is why [`usable`]
//! refuses SSH. Terminals without the Kitty protocol (Terminal.app, iTerm2) get the
//! braille chart instead.

use std::fs;
use std::io;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui_image::picker::{Picker, ProtocolType};

/// Where `shm_open` objects live on Linux; `/dev/shm/x` is the object `/x`.
const SHM_DIR: &str = "/dev/shm";
const SHM_PREFIX: &str = "thermo-chart-";
/// Kitty and Ghostty read (and delete) a `t=t` file only if it sits in a temporary
/// directory and its name contains `tty-graphics-protocol`.
const TEMP_PREFIX: &str = "thermo-chart-tty-graphics-protocol-";
/// Without `/proc` to check whether its writer still runs, a file is an orphan once
/// it is far older than any terminal takes to read it.
const ORPHANED_AFTER: Duration = Duration::from_secs(60);
/// The terminal unlinks an object once it has read it; one still there after this long
/// was never going to be read.
const SHM_ABANDONED_AFTER: Duration = Duration::from_secs(10);

/// Cell character the terminal replaces with a piece of an image.
const PLACEHOLDER: char = '\u{10EEEE}';

/// Whether images can be passed this way: the terminal speaks the Kitty protocol and
/// shares our `/dev/shm`. Inside tmux the placeholders would need passthrough wrapping.
pub fn usable(picker: &Picker) -> bool {
    let remote = ["SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some());
    picker.protocol_type() == ProtocolType::Kitty && !picker.tmux_detected() && !remote
}

/// Straight (not premultiplied) RGBA pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// How images reach the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Medium {
    /// POSIX shared memory (`t=s`), named relative to `/dev/shm`.
    SharedMemory,
    /// A temporary file (`t=t`), named by its absolute path.
    TempFile,
}

impl Medium {
    /// Shared memory where `shm_open` objects are files we can write (Linux), else a
    /// temporary file: macOS has `shm_open` but no file system view of it.
    fn detect() -> Self {
        if Path::new(SHM_DIR).is_dir() {
            Self::SharedMemory
        } else {
            Self::TempFile
        }
    }

    fn dir(self) -> PathBuf {
        match self {
            Self::SharedMemory => PathBuf::from(SHM_DIR),
            Self::TempFile => std::env::temp_dir(),
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::SharedMemory => SHM_PREFIX,
            Self::TempFile => TEMP_PREFIX,
        }
    }

    fn code(self) -> char {
        match self {
            Self::SharedMemory => 's',
            Self::TempFile => 't',
        }
    }
}

/// An image written to shared memory or a temporary file, waiting for the terminal to
/// pick it up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shared {
    medium: Medium,
    /// Shared memory object name, or the file's path.
    name: String,
    width: u32,
    height: u32,
    len: usize,
}

impl Shared {
    /// Escape sequences that load the image as `id` and stretch it over `cols` × `rows`
    /// placeholder cells.
    ///
    /// Any earlier image with that id is deleted first: retransmitting alone swaps the
    /// pixels but keeps the old placement's size (seen in Ghostty after a resize).
    pub fn transmit(&self, id: u32, cols: u16, rows: u16) -> String {
        format!(
            "{}\x1b_Ga=T,U=1,q=2,i={id},f=32,t={},s={},v={},S={},c={cols},r={rows};{}\x1b\\",
            delete(id),
            self.medium.code(),
            self.width,
            self.height,
            self.len,
            base64(self.name.as_bytes()),
        )
    }
}

/// Escape sequence that frees image `id` in the terminal.
pub fn delete(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
}

/// Fills `area` with placeholders for image `id`, which must be below 2²⁴.
///
/// The id travels in the foreground color, and every cell names its row and column.
/// A cell without them would take its place from the cell to its left, and a popup
/// drawn over that cell would leave the rest of the row showing the image's first row.
pub fn place(buf: &mut Buffer, area: Rect, id: u32) {
    let [_, r, g, b] = id.to_be_bytes();
    let rows = area.height.min(diacritic_count());
    for row in 0..rows {
        for col in 0..area.width {
            let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) else {
                continue;
            };
            let mut symbol = String::from(PLACEHOLDER);
            // Columns past the table are counted on from the cell to the left.
            if let Some(&column) = DIACRITICS.get(usize::from(col)) {
                symbol.push(DIACRITICS[usize::from(row)]);
                symbol.push(column);
            }
            cell.set_symbol(&symbol)
                .set_fg(Color::Rgb(r, g, b))
                .set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
        }
    }
}

fn diacritic_count() -> u16 {
    u16::try_from(DIACRITICS.len()).expect("table is short")
}

/// Writes images to `/dev/shm` or the temporary directory under names unique to this
/// process.
pub struct SharedMemory {
    medium: Medium,
    next: u64,
    written: Vec<(PathBuf, Instant)>,
}

impl SharedMemory {
    /// Also removes objects left behind by earlier runs that have since exited.
    pub fn new() -> Self {
        let medium = Medium::detect();
        remove_orphans(medium);
        Self {
            medium,
            next: 0,
            written: Vec::new(),
        }
    }

    /// # Errors
    ///
    /// Fails if the directory is missing or full.
    pub fn write(&mut self, image: &Rgba) -> io::Result<Shared> {
        self.remove_abandoned();
        let name = format!(
            "{}{}-{}",
            self.medium.prefix(),
            std::process::id(),
            self.next
        );
        self.next += 1;
        let path = self.medium.dir().join(&name);
        fs::write(&path, &image.data)?;
        let name = match self.medium {
            Medium::SharedMemory => format!("/{name}"),
            Medium::TempFile => path.to_string_lossy().into_owned(),
        };
        self.written.push((path, Instant::now()));
        Ok(Shared {
            medium: self.medium,
            name,
            width: image.width,
            height: image.height,
            len: image.data.len(),
        })
    }

    fn remove_abandoned(&mut self) {
        self.written.retain(|(path, at)| {
            if at.elapsed() < SHM_ABANDONED_AFTER {
                return true;
            }
            // Usually already unlinked by the terminal.
            let _ = fs::remove_file(path);
            false
        });
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        for (path, _) in &self.written {
            let _ = fs::remove_file(path);
        }
    }
}

/// Removes objects whose process no longer runs, e.g. after a crash. Without `/proc`
/// (macOS), removes those older than [`ORPHANED_AFTER`] instead.
fn remove_orphans(medium: Medium) {
    let Ok(entries) = fs::read_dir(medium.dir()) else {
        return;
    };
    let proc = Path::new("/proc/self").exists();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(medium.prefix()))
            .and_then(|rest| rest.split('-').next())
        else {
            continue;
        };
        let orphaned = if proc {
            !PathBuf::from("/proc").join(pid).exists()
        } else {
            entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|at| SystemTime::now().duration_since(at).ok())
                .is_some_and(|age| age > ORPHANED_AFTER)
        };
        if orphaned {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 0x3f) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Row and column numbers, from
/// <https://sw.kovidgoyal.net/kitty/_downloads/1792bad15b12979994cd6ecc54c967a6/rowcolumn-diacritics.txt>.
#[rustfmt::skip]
static DIACRITICS: [char; 297] = [
    '\u{305}', '\u{30D}', '\u{30E}', '\u{310}', '\u{312}', '\u{33D}', '\u{33E}', '\u{33F}',
    '\u{346}', '\u{34A}', '\u{34B}', '\u{34C}', '\u{350}', '\u{351}', '\u{352}', '\u{357}',
    '\u{35B}', '\u{363}', '\u{364}', '\u{365}', '\u{366}', '\u{367}', '\u{368}', '\u{369}',
    '\u{36A}', '\u{36B}', '\u{36C}', '\u{36D}', '\u{36E}', '\u{36F}', '\u{483}', '\u{484}',
    '\u{485}', '\u{486}', '\u{487}', '\u{592}', '\u{593}', '\u{594}', '\u{595}', '\u{597}',
    '\u{598}', '\u{599}', '\u{59C}', '\u{59D}', '\u{59E}', '\u{59F}', '\u{5A0}', '\u{5A1}',
    '\u{5A8}', '\u{5A9}', '\u{5AB}', '\u{5AC}', '\u{5AF}', '\u{5C4}', '\u{610}', '\u{611}',
    '\u{612}', '\u{613}', '\u{614}', '\u{615}', '\u{616}', '\u{617}', '\u{657}', '\u{658}',
    '\u{659}', '\u{65A}', '\u{65B}', '\u{65D}', '\u{65E}', '\u{6D6}', '\u{6D7}', '\u{6D8}',
    '\u{6D9}', '\u{6DA}', '\u{6DB}', '\u{6DC}', '\u{6DF}', '\u{6E0}', '\u{6E1}', '\u{6E2}',
    '\u{6E4}', '\u{6E7}', '\u{6E8}', '\u{6EB}', '\u{6EC}', '\u{730}', '\u{732}', '\u{733}',
    '\u{735}', '\u{736}', '\u{73A}', '\u{73D}', '\u{73F}', '\u{740}', '\u{741}', '\u{743}',
    '\u{745}', '\u{747}', '\u{749}', '\u{74A}', '\u{7EB}', '\u{7EC}', '\u{7ED}', '\u{7EE}',
    '\u{7EF}', '\u{7F0}', '\u{7F1}', '\u{7F3}', '\u{816}', '\u{817}', '\u{818}', '\u{819}',
    '\u{81B}', '\u{81C}', '\u{81D}', '\u{81E}', '\u{81F}', '\u{820}', '\u{821}', '\u{822}',
    '\u{823}', '\u{825}', '\u{826}', '\u{827}', '\u{829}', '\u{82A}', '\u{82B}', '\u{82C}',
    '\u{82D}', '\u{951}', '\u{953}', '\u{954}', '\u{F82}', '\u{F83}', '\u{F86}', '\u{F87}',
    '\u{135D}', '\u{135E}', '\u{135F}', '\u{17DD}', '\u{193A}', '\u{1A17}', '\u{1A75}', '\u{1A76}',
    '\u{1A77}', '\u{1A78}', '\u{1A79}', '\u{1A7A}', '\u{1A7B}', '\u{1A7C}', '\u{1B6B}', '\u{1B6D}',
    '\u{1B6E}', '\u{1B6F}', '\u{1B70}', '\u{1B71}', '\u{1B72}', '\u{1B73}', '\u{1CD0}', '\u{1CD1}',
    '\u{1CD2}', '\u{1CDA}', '\u{1CDB}', '\u{1CE0}', '\u{1DC0}', '\u{1DC1}', '\u{1DC3}', '\u{1DC4}',
    '\u{1DC5}', '\u{1DC6}', '\u{1DC7}', '\u{1DC8}', '\u{1DC9}', '\u{1DCB}', '\u{1DCC}', '\u{1DD1}',
    '\u{1DD2}', '\u{1DD3}', '\u{1DD4}', '\u{1DD5}', '\u{1DD6}', '\u{1DD7}', '\u{1DD8}', '\u{1DD9}',
    '\u{1DDA}', '\u{1DDB}', '\u{1DDC}', '\u{1DDD}', '\u{1DDE}', '\u{1DDF}', '\u{1DE0}', '\u{1DE1}',
    '\u{1DE2}', '\u{1DE3}', '\u{1DE4}', '\u{1DE5}', '\u{1DE6}', '\u{1DFE}', '\u{20D0}', '\u{20D1}',
    '\u{20D4}', '\u{20D5}', '\u{20D6}', '\u{20D7}', '\u{20DB}', '\u{20DC}', '\u{20E1}', '\u{20E7}',
    '\u{20E9}', '\u{20F0}', '\u{2CEF}', '\u{2CF0}', '\u{2CF1}', '\u{2DE0}', '\u{2DE1}', '\u{2DE2}',
    '\u{2DE3}', '\u{2DE4}', '\u{2DE5}', '\u{2DE6}', '\u{2DE7}', '\u{2DE8}', '\u{2DE9}', '\u{2DEA}',
    '\u{2DEB}', '\u{2DEC}', '\u{2DED}', '\u{2DEE}', '\u{2DEF}', '\u{2DF0}', '\u{2DF1}', '\u{2DF2}',
    '\u{2DF3}', '\u{2DF4}', '\u{2DF5}', '\u{2DF6}', '\u{2DF7}', '\u{2DF8}', '\u{2DF9}', '\u{2DFA}',
    '\u{2DFB}', '\u{2DFC}', '\u{2DFD}', '\u{2DFE}', '\u{2DFF}', '\u{A66F}', '\u{A67C}', '\u{A67D}',
    '\u{A6F0}', '\u{A6F1}', '\u{A8E0}', '\u{A8E1}', '\u{A8E2}', '\u{A8E3}', '\u{A8E4}', '\u{A8E5}',
    '\u{A8E6}', '\u{A8E7}', '\u{A8E8}', '\u{A8E9}', '\u{A8EA}', '\u{A8EB}', '\u{A8EC}', '\u{A8ED}',
    '\u{A8EE}', '\u{A8EF}', '\u{A8F0}', '\u{A8F1}', '\u{AAB0}', '\u{AAB2}', '\u{AAB3}', '\u{AAB7}',
    '\u{AAB8}', '\u{AABE}', '\u{AABF}', '\u{AAC1}', '\u{FE20}', '\u{FE21}', '\u{FE22}', '\u{FE23}',
    '\u{FE24}', '\u{FE25}', '\u{FE26}', '\u{10A0F}', '\u{10A38}', '\u{1D185}', '\u{1D186}', '\u{1D187}',
    '\u{1D188}', '\u{1D189}', '\u{1D1AA}', '\u{1D1AB}', '\u{1D1AC}', '\u{1D1AD}', '\u{1D242}', '\u{1D243}',
    '\u{1D244}',
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("/thermo-chart-1-0", "L3RoZXJtby1jaGFydC0xLTA="),
        ] {
            assert_eq!(base64(input.as_bytes()), want);
        }
    }

    #[test]
    fn placeholders_carry_id_and_row() {
        let area = Rect::new(0, 0, 3, 2);
        let mut buf = Buffer::empty(area);
        place(&mut buf, area, 0x12_3456);
        let cell = &buf[(0, 1)];
        assert_eq!(cell.symbol(), "\u{10EEEE}\u{30D}\u{305}");
        assert_eq!(cell.fg, Color::Rgb(0x12, 0x34, 0x56));
        assert_eq!(buf[(2, 1)].symbol(), "\u{10EEEE}\u{30D}\u{30E}");
    }

    #[test]
    fn cells_right_of_a_popup_keep_their_place() {
        let area = Rect::new(0, 0, 400, 2);
        let mut buf = Buffer::empty(area);
        place(&mut buf, area, 1);
        // A popup covering columns 0..10 leaves column 10 to stand on its own.
        assert_eq!(buf[(10, 1)].symbol(), "\u{10EEEE}\u{30D}\u{34B}");
        assert_eq!(buf[(399, 1)].symbol(), "\u{10EEEE}");
    }

    #[test]
    fn transmit_names_object_and_size() {
        let shared = Shared {
            medium: Medium::SharedMemory,
            name: "/thermo-chart-1-0".into(),
            width: 28,
            height: 64,
            len: 28 * 64 * 4,
        };
        assert_eq!(
            shared.transmit(7, 2, 2),
            "\x1b_Ga=d,d=I,i=7,q=2\x1b\\\
             \x1b_Ga=T,U=1,q=2,i=7,f=32,t=s,s=28,v=64,S=7168,c=2,r=2;L3RoZXJtby1jaGFydC0xLTA=\x1b\\"
        );
    }

    #[test]
    fn temp_files_use_t_t_and_their_path() {
        let shared = Shared {
            medium: Medium::TempFile,
            name: "/tmp/thermo-chart-tty-graphics-protocol-1-0".into(),
            width: 1,
            height: 1,
            len: 4,
        };
        let sequence = shared.transmit(7, 1, 1);
        assert!(sequence.contains(",t=t,"));
        assert!(sequence.ends_with(&format!(
            ";{}\x1b\\",
            base64(b"/tmp/thermo-chart-tty-graphics-protocol-1-0")
        )));
        assert!(TEMP_PREFIX.contains("tty-graphics-protocol"));
    }

    #[test]
    fn temp_file_round_trip() {
        let mut memory = SharedMemory {
            medium: Medium::TempFile,
            next: 0,
            written: Vec::new(),
        };
        let image = Rgba {
            width: 1,
            height: 1,
            data: vec![1, 2, 3, 4],
        };
        let shared = memory.write(&image).expect("temp dir is writable");
        assert!(
            shared
                .name
                .starts_with(&*std::env::temp_dir().to_string_lossy())
        );
        assert_eq!(fs::read(&shared.name).expect("written"), image.data);
        drop(memory);
        assert!(!Path::new(&shared.name).exists());
    }
}
