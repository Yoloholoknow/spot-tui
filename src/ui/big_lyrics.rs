//! Lyrics drawn as an image, so they can be larger than the terminal's own font.
//!
//! A terminal app cannot change its font size, and the Kitty text-sizing
//! protocol is not implemented by Ghostty, so the visible part of the sheet is
//! rasterised with a system font and shown through the same graphics protocol
//! the covers use. The image is rebuilt only when the text, the current line or
//! the size changes, never for the per-character sweep, which an image cannot
//! afford to redraw at frame rate. Anything missing (no graphics protocol, no
//! usable font, a tiny pane) makes `render_big_lyrics` return `false` and the
//! caller draws the ordinary text instead.

use super::*;
use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// Latin first, then CJK and Hangul fallbacks, tried per character. Faces load
/// lazily: the Korean and Arial Unicode files are tens of MB and most sheets
/// never need them.
const SYSTEM_FONTS: &[(&str, u32)] = &[
    // macOS
    ("/System/Library/Fonts/SFNS.ttf", 0),
    ("/System/Library/Fonts/ヒラギノ角ゴシック W4.ttc", 0),
    ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
    ("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0),
    ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0),
    // Linux
    ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0),
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
    ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
    (
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        0,
    ),
    // Windows
    ("C:\\Windows\\Fonts\\segoeui.ttf", 0),
    ("C:\\Windows\\Fonts\\YuGothR.ttc", 0),
    ("C:\\Windows\\Fonts\\msyh.ttc", 0),
    ("C:\\Windows\\Fonts\\malgun.ttf", 0),
];

/// Line pitch within one lyric, and the gap after it, as multiples of the font size.
const LINE_PITCH: f32 = 1.2;
const SPACER: f32 = 0.55;
/// How far above the current line the compact view starts, in font sizes: about
/// one already-sung line, as `top_anchored_offset` keeps in the text view.
const COMPACT_LEAD: f32 = LINE_PITCH + SPACER;

struct Face {
    path: PathBuf,
    index: u32,
    /// `None` until first needed; `Some(None)` if the file would not load.
    font: Option<Option<FontVec>>,
}

pub struct FontSet {
    faces: Vec<Face>,
    /// Which face covers a character, remembered because layout asks per glyph.
    coverage: HashMap<char, Option<usize>>,
}

impl FontSet {
    /// The configured font first (if any), then whichever system fonts exist.
    /// `None` when no candidate file is present at all.
    pub fn discover(custom: Option<&str>) -> Option<Self> {
        let mut faces: Vec<Face> = custom
            .map(|p| (PathBuf::from(p), 0))
            .into_iter()
            .chain(SYSTEM_FONTS.iter().map(|(p, i)| (PathBuf::from(p), *i)))
            .filter(|(p, _)| p.is_file())
            .map(|(path, index)| Face {
                path,
                index,
                font: None,
            })
            .collect();
        if faces.is_empty() {
            return None;
        }
        // The primary face has to be real: it sets the baseline of every row.
        while let Some(first) = faces.first_mut() {
            if load_face(first).is_some() {
                return Some(Self {
                    faces,
                    coverage: HashMap::new(),
                });
            }
            faces.remove(0);
        }
        None
    }

    fn face_for(&mut self, c: char) -> Option<usize> {
        if let Some(found) = self.coverage.get(&c) {
            return *found;
        }
        let mut found = None;
        for i in 0..self.faces.len() {
            if let Some(font) = load_face(&mut self.faces[i])
                && font.glyph_id(c).0 != 0
            {
                found = Some(i);
                break;
            }
        }
        self.coverage.insert(c, found);
        found
    }

    fn font(&self, index: usize) -> &FontVec {
        self.faces[index]
            .font
            .as_ref()
            .and_then(Option::as_ref)
            .expect("face_for only returns loaded faces")
    }

    fn advance(&mut self, c: char, px: f32) -> f32 {
        match self.face_for(c) {
            Some(i) => {
                let font = self.font(i);
                font.as_scaled(PxScale::from(px))
                    .h_advance(font.glyph_id(c))
            }
            // A character no face has draws nothing; give it a narrow gap.
            None => px * 0.3,
        }
    }
}

fn load_face(face: &mut Face) -> Option<&FontVec> {
    face.font
        .get_or_insert_with(|| {
            let data = std::fs::read(&face.path).ok()?;
            FontVec::try_from_vec_and_index(data, face.index).ok()
        })
        .as_ref()
}

/// Wide scripts break between any two characters, not only at spaces.
fn is_wide(c: char) -> bool {
    matches!(c as u32, 0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

/// One wrapped row of a lyric: its text, measured width, and the index of its
/// first character in the whole lyric (so per-character colours line up).
struct Row {
    text: String,
    width: f32,
    start: usize,
}

/// Greedy wrap of one lyric into rows no wider than `max_w`. Breaks after
/// spaces and wide characters, and mid-word only when one word alone is too long.
fn wrap_rows(fonts: &mut FontSet, text: &str, px: f32, max_w: f32) -> Vec<Row> {
    let chars: Vec<char> = text.chars().collect();
    let widths: Vec<f32> = chars.iter().map(|&c| fonts.advance(c, px)).collect();
    let max_w = max_w.max(1.0);

    let mut rows = Vec::new();
    let mut push_row = |from: usize, to: usize| {
        let range = &chars[from..to];
        let end = range.iter().rposition(|c| *c != ' ').map_or(0, |i| i + 1);
        rows.push(Row {
            text: range[..end].iter().collect(),
            width: widths[from..from + end].iter().sum(),
            start: from,
        });
    };

    let (mut start, mut width, mut last_break) = (0usize, 0f32, None::<usize>);
    for i in 0..chars.len() {
        if width + widths[i] > max_w && i > start && chars[i] != ' ' {
            let brk = last_break.filter(|&b| b > start).unwrap_or(i);
            push_row(start, brk);
            start = brk;
            width = widths[start..i].iter().sum();
            last_break = None;
        }
        width += widths[i];
        if chars[i] == ' ' || is_wide(chars[i]) {
            last_break = Some(i + 1);
        }
    }
    if start < chars.len() {
        push_row(start, chars.len());
    }
    rows
}

pub struct StyledLine {
    pub text: String,
    pub rgb: [u8; 3],
    /// A colour per character, only for a line drawn in more than one colour
    /// (the word sweep). Those lines go in their own small image.
    pub chars: Option<Vec<[u8; 3]>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// Fullscreen: the current line sits mid-viewport.
    Centered,
    /// Now Playing: the current line sits near the top, one sung line above it.
    Top,
}

struct Block {
    top: f32,
    height: f32,
    rgb: [u8; 3],
    chars: Option<Vec<[u8; 3]>>,
    rows: Vec<Row>,
}

struct Sheet {
    blocks: Vec<Block>,
    total: f32,
}

fn layout(fonts: &mut FontSet, lines: &[StyledLine], px: f32, max_w: f32) -> Sheet {
    let mut blocks = Vec::with_capacity(lines.len());
    let mut top = 0f32;
    for line in lines {
        let rows = if line.text.is_empty() {
            Vec::new()
        } else {
            wrap_rows(fonts, &line.text, px, max_w)
        };
        let height = if rows.is_empty() {
            px * SPACER
        } else {
            rows.len() as f32 * px * LINE_PITCH
        };
        blocks.push(Block {
            top,
            height,
            rgb: line.rgb,
            chars: line.chars.clone(),
            rows,
        });
        top += height;
    }
    Sheet { blocks, total: top }
}

/// Pixels of the sheet scrolled off the top. May be negative: like the text
/// view's padding, it lets the first and last lines reach the anchor.
fn scroll_offset(
    sheet: &Sheet,
    current: Option<usize>,
    anchor: Anchor,
    view_h: f32,
    px: f32,
) -> f32 {
    let Some(block) = current.and_then(|i| sheet.blocks.get(i)) else {
        // A status message or unsynced text: centred if it fits, else from the top.
        return match anchor {
            Anchor::Centered if sheet.total < view_h => -(view_h - sheet.total) / 2.0,
            _ => 0.0,
        };
    };
    match anchor {
        Anchor::Centered => block.top + block.height / 2.0 - view_h / 2.0,
        Anchor::Top => {
            let max = (sheet.total - view_h).max(0.0);
            (block.top - px * COMPACT_LEAD).clamp(0.0, max)
        }
    }
}

/// The whole cell rows, as pixel rows `[y0, y1)`, covered by the current line when
/// it is drawn per character. That strip is left blank in the sheet image and
/// supplied by its own image instead, so the sweep only ever re-sends a strip.
fn sweep_band(
    sheet: &Sheet,
    current: Option<usize>,
    offset: f32,
    cell_h: u32,
    view_h: u32,
) -> Option<(u32, u32)> {
    let block = current.and_then(|i| sheet.blocks.get(i))?;
    block.chars.as_ref()?;
    let cell = cell_h.max(1) as f32;
    let top = block.top - offset;
    let y0 = ((top / cell).floor().max(0.0) * cell) as u32;
    let y1 = (((top + block.height) / cell).ceil().max(0.0) * cell) as u32;
    let y1 = y1.min(view_h);
    (y1 > y0).then_some((y0, y1))
}

/// Where each character of the swept `block` sits, mirroring how `rasterize`
/// advances along a row.
fn char_spans(
    fonts: &mut FontSet,
    block: &Block,
    px: f32,
    width: u32,
    centered_text: bool,
) -> Vec<Option<CharSpan>> {
    let total = block.chars.as_ref().map_or(0, Vec::len);
    let mut spans = vec![None; total];
    for (r, row) in block.rows.iter().enumerate() {
        let mut x = if centered_text {
            ((width as f32 - row.width) / 2.0).max(0.0)
        } else {
            0.0
        };
        for (k, c) in row.text.chars().enumerate() {
            let advance = match fonts.face_for(c) {
                Some(face) => {
                    let font = fonts.font(face);
                    font.as_scaled(PxScale::from(px))
                        .h_advance(font.glyph_id(c))
                }
                None => px * 0.3,
            };
            if let Some(slot) = spans.get_mut(row.start + k) {
                *slot = Some(CharSpan {
                    row: r,
                    x0: x,
                    x1: x + advance,
                });
            }
            x += advance;
        }
    }
    spans
}

/// For each cell column of one cell row (its vertical middle `y_mid`, in view
/// pixels) of the swept line: is the text there sung? Columns with no text, and
/// rows outside the line, report `false`; both copies of the strip look the same
/// there.
fn sung_columns(
    block: &Block,
    spans: &[Option<CharSpan>],
    sung: &[bool],
    (offset, px): (f32, f32),
    y_mid: f32,
    (cell_w, cols): (u32, u16),
) -> Vec<bool> {
    let rel = y_mid - (block.top - offset);
    let row = (rel >= 0.0).then(|| (rel / (px * LINE_PITCH)) as usize);
    (0..cols)
        .map(|col| {
            let x = col as f32 * cell_w as f32 + cell_w as f32 / 2.0;
            row.and_then(|row| {
                spans
                    .iter()
                    .position(|s| s.is_some_and(|s| s.row == row && s.x0 <= x && x < s.x1))
            })
            .and_then(|i| sung.get(i).copied())
            .unwrap_or(false)
        })
        .collect()
}

/// What one terminal cell of the swept line shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cell {
    Sung,
    Unsung,
    /// Under the soft edge: drawn from the small edge image.
    Soft,
}

/// Where the sweep has got to.
#[derive(Clone, Copy)]
enum Sweep {
    /// No single edge (words overlap): colour by whole characters.
    Columns,
    AllUnsung,
    AllSung,
    /// An edge at pixel column `x` on text row `row`.
    Edge {
        x: f32,
        row: usize,
    },
}

/// Pixel position of the playhead `head` (characters in, fractional) on the line.
fn edge_position(spans: &[Option<CharSpan>], head: f32) -> Option<(f32, usize)> {
    let index = head.floor().max(0.0) as usize;
    let frac = head - index as f32;
    let at = spans.get(index).copied().flatten();
    if let Some(span) = at {
        return Some((span.x0 + frac * (span.x1 - span.x0), span.row));
    }
    // A character with no span (a space trimmed off a wrapped row): the next one.
    spans
        .iter()
        .skip(index)
        .flatten()
        .next()
        .map(|s| (s.x0, s.row))
}

/// The text rows of `block` (first, last) whose pixels fall in `[ya, yb)`.
fn text_rows_in(block: &Block, offset: f32, px: f32, ya: f32, yb: f32) -> Option<(usize, usize)> {
    let pitch = px * LINE_PITCH;
    let top = block.top - offset;
    if block.rows.is_empty() || yb <= top || ya >= top + pitch * block.rows.len() as f32 {
        return None;
    }
    let last = block.rows.len() - 1;
    let first = (((ya - top) / pitch).floor().max(0.0) as usize).min(last);
    let end = (((yb - 1.0 - top) / pitch).floor().max(0.0) as usize).min(last);
    Some((first, end))
}

/// Draws the sheet's pixel rows `clip` (the whole view for the sheet image, a
/// band for the sweep strip) into an image of the clip's height, leaving
/// `exclude` blank. Everything is positioned in whole-view coordinates, so the
/// pieces line up exactly.
#[allow(clippy::too_many_arguments)]
fn rasterize(
    fonts: &mut FontSet,
    sheet: &Sheet,
    offset: f32,
    width: u32,
    px: f32,
    centered_text: bool,
    clip: (u32, u32),
    exclude: Option<(u32, u32)>,
    tint: Option<[u8; 3]>,
) -> image::RgbaImage {
    rasterize_region(
        fonts,
        sheet,
        offset,
        width,
        px,
        centered_text,
        ((0, width), clip),
        exclude,
        Draw::Plain(tint),
    )
}

/// A soft sung-to-unsung edge across the swept line: left of `edge_x` is sung,
/// right of it unsung, blended over `2 * half` pixels. On rows of the line above
/// `row` everything is sung and below it everything is unsung.
#[derive(Clone, Copy)]
struct Gradient {
    edge_x: f32,
    half: f32,
    row: usize,
    sung: [u8; 3],
    unsung: [u8; 3],
}

impl Gradient {
    fn colour_at(&self, row: usize, x: f32) -> [u8; 3] {
        let level = match row.cmp(&self.row) {
            std::cmp::Ordering::Less => 1.0,
            std::cmp::Ordering::Greater => 0.0,
            std::cmp::Ordering::Equal => {
                let t = ((self.edge_x - x) / (2.0 * self.half) + 0.5).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            }
        };
        let mix = |i: usize| {
            (self.unsung[i] as f32 + (self.sung[i] as f32 - self.unsung[i] as f32) * level).round()
                as u8
        };
        [mix(0), mix(1), mix(2)]
    }
}

/// How the swept line is coloured in a rasterization.
#[derive(Clone, Copy)]
enum Draw {
    /// Its own per-character colours, or one `tint` over the whole line.
    Plain(Option<[u8; 3]>),
    /// A soft edge.
    Soft(Gradient),
}

/// Draws the part of the sheet inside `region` (`(x range, y range)` in whole-view
/// pixels) into an image of that size, leaving `exclude` rows blank. Everything is
/// positioned in whole-view coordinates, so images cut from different regions line
/// up exactly.
#[allow(clippy::too_many_arguments)]
fn rasterize_region(
    fonts: &mut FontSet,
    sheet: &Sheet,
    offset: f32,
    view_w: u32,
    px: f32,
    centered_text: bool,
    region: ((u32, u32), (u32, u32)),
    exclude: Option<(u32, u32)>,
    draw: Draw,
) -> image::RgbaImage {
    let (xclip, clip) = region;
    let mut img = image::RgbaImage::new(xclip.1 - xclip.0, clip.1 - clip.0);
    let primary = fonts.faces[0]
        .font
        .as_ref()
        .and_then(Option::as_ref)
        .map(|f| f.as_scaled(PxScale::from(px)).ascent())
        .unwrap_or(px * 0.8);
    // Faux bold: the text view is bold, and system faces here are regular weight.
    let embolden = (px * 0.03).max(0.5);

    for block in &sheet.blocks {
        let block_top = block.top - offset;
        if block_top + block.height < clip.0 as f32 || block_top > clip.1 as f32 {
            continue;
        }
        for (r, row) in block.rows.iter().enumerate() {
            let y = block_top + r as f32 * px * LINE_PITCH;
            if y + px * LINE_PITCH < clip.0 as f32 || y > clip.1 as f32 {
                continue;
            }
            let baseline = y + (px * LINE_PITCH - px) / 2.0 + primary;
            let mut x = if centered_text {
                ((view_w as f32 - row.width) / 2.0).max(0.0)
            } else {
                0.0
            };
            for (k, c) in row.text.chars().enumerate() {
                let Some(face) = fonts.face_for(c) else {
                    x += px * 0.3;
                    continue;
                };
                // Only the swept line is recoloured; every other line keeps its own
                // colour, so any copy of the strip is right for them.
                let flat = match (&block.chars, draw) {
                    (Some(_), Draw::Plain(Some(tint))) => Some(tint),
                    (Some(colors), Draw::Plain(None)) => {
                        Some(colors.get(row.start + k).copied().unwrap_or(block.rgb))
                    }
                    (Some(_), Draw::Soft(_)) => None,
                    (None, _) => Some(block.rgb),
                };
                let font = fonts.font(face);
                let scale = PxScale::from(px);
                for dx in [0.0, embolden] {
                    let glyph = font
                        .glyph_id(c)
                        .with_scale_and_position(scale, point(x + dx, baseline));
                    if let Some(outlined) = font.outline_glyph(glyph) {
                        let bounds = outlined.px_bounds();
                        outlined.draw(|gx, gy, cov| {
                            let ix = bounds.min.x as i32 + gx as i32;
                            let iy = bounds.min.y as i32 + gy as i32;
                            if ix < xclip.0 as i32
                                || ix >= xclip.1 as i32
                                || iy < clip.0 as i32
                                || iy >= clip.1 as i32
                            {
                                return;
                            }
                            let iy = iy as u32;
                            if exclude.is_some_and(|(y0, y1)| (y0..y1).contains(&iy)) {
                                return;
                            }
                            let rgb = flat.unwrap_or_else(|| match draw {
                                Draw::Soft(g) => g.colour_at(r, ix as f32 + 0.5),
                                Draw::Plain(_) => block.rgb,
                            });
                            let alpha = (cov.clamp(0.0, 1.0) * 255.0) as u8;
                            let pixel = img.get_pixel_mut(ix as u32 - xclip.0, iy - clip.0);
                            if alpha > pixel.0[3] {
                                *pixel = image::Rgba([rgb[0], rgb[1], rgb[2], alpha]);
                            }
                        });
                    }
                }
                x += font.as_scaled(scale).h_advance(font.glyph_id(c));
            }
        }
    }
    img
}

/// The terminal's palette is unknown, so named colours are the usual xterm
/// values; indexed ones follow the 256-colour cube and grey ramp.
fn color_rgb(color: Color) -> [u8; 3] {
    match color {
        Color::Rgb(r, g, b) => [r, g, b],
        Color::White => [255, 255, 255],
        Color::Gray => [190, 190, 190],
        Color::DarkGray => [125, 125, 125],
        Color::Indexed(i @ 16..=231) => {
            let n = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            [level(n / 36), level(n / 6 % 6), level(n % 6)]
        }
        Color::Indexed(i @ 232..=255) => {
            let v = 8 + 10 * (i - 232);
            [v, v, v]
        }
        _ => [230, 230, 230],
    }
}

/// Everything the image depends on but the sweep: when none of it changes the
/// cached image is reused, so a redraw costs nothing.
fn cache_key(
    lines: &[StyledLine],
    current: Option<usize>,
    size: (u32, u32),
    scale: f32,
    anchor: Anchor,
    alignment: Alignment,
    sweep: bool,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    sweep.hash(&mut hasher);
    for line in lines {
        line.text.hash(&mut hasher);
        line.rgb.hash(&mut hasher);
    }
    current.hash(&mut hasher);
    size.hash(&mut hasher);
    scale.to_bits().hash(&mut hasher);
    (anchor == Anchor::Centered).hash(&mut hasher);
    (alignment == Alignment::Center).hash(&mut hasher);
    hasher.finish()
}

/// The Kitty "rowcolumn diacritics" table: the Nth combining mark names row or
/// column N of a unicode-placeholder image. Only the first rows are ever needed.
const DIACRITICS: [u32; 297] = [
    0x305, 0x30D, 0x30E, 0x310, 0x312, 0x33D, 0x33E, 0x33F, 0x346, 0x34A, 0x34B, 0x34C, 0x350,
    0x351, 0x352, 0x357, 0x35B, 0x363, 0x364, 0x365, 0x366, 0x367, 0x368, 0x369, 0x36A, 0x36B,
    0x36C, 0x36D, 0x36E, 0x36F, 0x483, 0x484, 0x485, 0x486, 0x487, 0x592, 0x593, 0x594, 0x595,
    0x597, 0x598, 0x599, 0x59C, 0x59D, 0x59E, 0x59F, 0x5A0, 0x5A1, 0x5A8, 0x5A9, 0x5AB, 0x5AC,
    0x5AF, 0x5C4, 0x610, 0x611, 0x612, 0x613, 0x614, 0x615, 0x616, 0x617, 0x657, 0x658, 0x659,
    0x65A, 0x65B, 0x65D, 0x65E, 0x6D6, 0x6D7, 0x6D8, 0x6D9, 0x6DA, 0x6DB, 0x6DC, 0x6DF, 0x6E0,
    0x6E1, 0x6E2, 0x6E4, 0x6E7, 0x6E8, 0x6EB, 0x6EC, 0x730, 0x732, 0x733, 0x735, 0x736, 0x73A,
    0x73D, 0x73F, 0x740, 0x741, 0x743, 0x745, 0x747, 0x749, 0x74A, 0x7EB, 0x7EC, 0x7ED, 0x7EE,
    0x7EF, 0x7F0, 0x7F1, 0x7F3, 0x816, 0x817, 0x818, 0x819, 0x81B, 0x81C, 0x81D, 0x81E, 0x81F,
    0x820, 0x821, 0x822, 0x823, 0x825, 0x826, 0x827, 0x829, 0x82A, 0x82B, 0x82C, 0x82D, 0x951,
    0x953, 0x954, 0xF82, 0xF83, 0xF86, 0xF87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17,
    0x1A75, 0x1A76, 0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F,
    0x1B70, 0x1B71, 0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1,
    0x1DC3, 0x1DC4, 0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3,
    0x1DD4, 0x1DD5, 0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF,
    0x1DE0, 0x1DE1, 0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5,
    0x20D6, 0x20D7, 0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0,
    0x2DE1, 0x2DE2, 0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC,
    0x2DED, 0x2DEE, 0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8,
    0x2DF9, 0x2DFA, 0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1,
    0xA8E0, 0xA8E1, 0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB,
    0xA8EC, 0xA8ED, 0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE,
    0xAABF, 0xAAC1, 0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38,
    0x1D185, 0x1D186, 0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242,
    0x1D243, 0x1D244,
];

fn diacritic(n: u16) -> char {
    char::from_u32(DIACRITICS[usize::from(n).min(DIACRITICS.len() - 1)]).unwrap_or('\u{305}')
}

/// PNG, not raw RGBA: lyric text is mostly transparent, so a screen-sized image
/// shrinks from megabytes to tens of kilobytes. Sent raw (what ratatui-image
/// does for covers) it took seconds per line change through tmux.
fn encode_png(img: &image::RgbaImage) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Sub)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(out)
}

/// Kitty escape sequence that stores `png` under `id` as a virtual placement
/// (for unicode placeholders). In tmux each piece is wrapped for passthrough,
/// like ratatui-image does.
fn transmit_sequence(png: &[u8], id: u32, tmux: bool) -> String {
    use base64::Engine;
    use std::fmt::Write;
    let (start, esc, end) = tmux_wrap(tmux);
    let mut out = String::with_capacity(png.len() * 4 / 3 + 256);
    // 4096 base64 characters is the protocol's chunk limit.
    let chunks: Vec<&[u8]> = png.chunks(3072).collect();
    let last = chunks.len().saturating_sub(1);
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i < last);
        out.push_str(start);
        let _ = write!(out, "{esc}_Gq=2,");
        if i == 0 {
            let _ = write!(out, "i={id},a=T,U=1,f=100,t=d,");
        }
        let _ = write!(out, "m={more};");
        base64::engine::general_purpose::STANDARD.encode_string(chunk, &mut out);
        let _ = write!(out, "{esc}\\{end}");
    }
    out
}

/// Frees an image's data in the terminal. Always sent *after* the replacement is
/// on screen: deleting first leaves a blank frame while the new one arrives.
fn delete_sequence(id: u32, tmux: bool) -> String {
    let (start, esc, end) = tmux_wrap(tmux);
    format!("{start}{esc}_Ga=d,d=I,i={id},q=2{esc}\\{end}")
}

fn tmux_wrap(tmux: bool) -> (&'static str, &'static str, &'static str) {
    if tmux {
        ("\x1bPtmux;", "\x1b\x1b", "\x1b\\")
    } else {
        ("", "\x1b", "")
    }
}

/// Escape sequences waiting to reach the terminal. They ride in the first
/// placeholder cell drawn, so they leave in the same write as the cells that use
/// them: `before` stores new images, `after` frees the ones they replace.
#[derive(Default)]
struct Payload {
    before: String,
    after: String,
}

/// Fills the cells of `rows` (rows of the image, top first), columns `cols`, of an
/// image whose footprint is `rect`, with the placeholders that make the terminal
/// show image `id` there. `payload` is spent on the first cell drawn.
fn draw_placeholders(
    buf: &mut ratatui::buffer::Buffer,
    rect: Rect,
    id: u32,
    rows: std::ops::Range<u16>,
    cols: std::ops::Range<u16>,
    payload: &mut Option<Payload>,
) {
    use ratatui::buffer::CellDiffOption;
    use std::fmt::Write;
    let cols = cols.start..cols.end.min(rect.width);
    if cols.is_empty() {
        return;
    }
    let [id_extra, r, g, b] = id.to_be_bytes();
    let color = format!("\x1b[38;2;{r};{g};{b}m");
    let len = usize::from(cols.end - cols.start);
    // Only the first placeholder names its row and column; the rest continue it.
    let tail: String = std::iter::repeat_n('\u{10EEEE}', len - 1).collect();
    // Where the cursor is left. A whole row restores to the end of the area (what
    // ratatui-image does); a piece of a row restores to the cell after its first,
    // which is where ratatui assumes it is, so an adjacent piece lands correctly.
    let restore = if cols.len() == usize::from(rect.width) {
        format!(
            "\x1b[u\x1b[{}C\x1b[{}B",
            rect.width.saturating_sub(1),
            rect.height.saturating_sub(1)
        )
    } else {
        "\x1b[u\x1b[1C".to_string()
    };
    for y in rows.start..rows.end.min(DIACRITICS.len() as u16) {
        let mut symbol = String::new();
        let spent = payload.take();
        if let Some(p) = &spent {
            symbol.push_str(&p.before);
        }
        let _ = write!(
            symbol,
            "\x1b[s{color}\u{10EEEE}{}{}{}",
            diacritic(y),
            diacritic(cols.start),
            diacritic(u16::from(id_extra))
        );
        symbol.push_str(&tail);
        for x in cols.start + 1..cols.end {
            if let Some(cell) = buf.cell_mut((rect.left() + x, rect.top() + y)) {
                cell.set_diff_option(CellDiffOption::Skip);
            }
        }
        symbol.push_str(&restore);
        if let Some(p) = &spent {
            symbol.push_str(&p.after);
        }
        if let Some(cell) = buf.cell_mut((rect.left() + cols.start, rect.top() + y)) {
            cell.set_symbol(&symbol)
                .set_diff_option(CellDiffOption::ForcedWidth(
                    std::num::NonZeroU16::new(1).unwrap(),
                ));
        }
    }
}

/// One image stored in the terminal.
struct Layer {
    id: u32,
}

/// Where one character of the swept line sits in the view, in pixels.
#[derive(Clone, Copy)]
struct CharSpan {
    row: usize,
    x0: f32,
    x1: f32,
}

/// The swept line as two copies, all sung colour and all unsung colour, both sent
/// once. The sweep only changes which copy each cell column shows, so nothing is
/// re-sent (or deleted) while it runs: replacing an image in place did not show up
/// in Ghostty.
struct Strip {
    sung: Layer,
    unsung: Layer,
    spans: Vec<Option<CharSpan>>,
}

/// Everything built for the current text, size and current line.
struct Built {
    key: u64,
    sheet: Sheet,
    offset: f32,
    /// Pixel rows of the swept line, when the current line is swept.
    band: Option<(u32, u32)>,
    base: Layer,
    strip: Option<Strip>,
    /// The small image under the soft edge of the sweep, replaced as it moves.
    edge: Option<EdgeLayer>,
}

struct EdgeLayer {
    /// What it was drawn for: cell columns, cell rows and the edge's pixel column.
    key: (u16, u16, u16, u16, i32),
    id: u32,
}

/// Settings plus render cache; lives in `ImageState`. `Default` is off, so
/// anything that builds an `ImageState` without config keeps the text view.
#[derive(Default)]
pub struct BigLyrics {
    pub enabled: bool,
    pub scale_fullscreen: f32,
    pub scale_compact: f32,
    fonts: Option<FontSet>,
    built: Option<Built>,
    /// Set when the terminal may have lost its images (focus returned): the next
    /// frame rebuilds and re-sends everything.
    stale: bool,
    /// Escape sequences queued for the next frame.
    payload: Payload,
    /// Distinct ids for successive images, so a stale one is never mistaken for
    /// the new one. Kept clear of the random ids ratatui-image gives covers.
    next_id: u32,
}

impl BigLyrics {
    pub fn new(cfg: &crate::config::Config) -> Self {
        let fonts = if cfg.big_lyrics {
            let fonts = FontSet::discover(cfg.lyrics_font.as_deref());
            if fonts.is_none() {
                log::warn!("big lyrics: no usable font found, using the terminal's own text");
            }
            fonts
        } else {
            None
        };
        Self {
            enabled: fonts.is_some(),
            scale_fullscreen: cfg.lyrics_scale_fullscreen.clamp(0.5, 8.0),
            scale_compact: cfg.lyrics_scale_compact.clamp(0.5, 8.0),
            fonts,
            ..Self::default()
        }
    }

    /// Switching tmux windows away and back drops placed images and gives the
    /// app no signal but focus. Re-send on the next frame.
    pub fn invalidate(&mut self) {
        self.stale = true;
    }

    fn fresh_id(&mut self) -> u32 {
        self.next_id = self.next_id.wrapping_add(1) & 0xFFFF;
        0x00B0_0000 | self.next_id.max(1)
    }
}

/// Draws the lyrics into `area` as images. Returns `false`, having drawn
/// nothing, when the caller should fall back to text: no Kitty-protocol
/// terminal (the only one this talks to directly), no font, or a tiny pane.
///
/// Two layers: the sheet (every line; rebuilt when the text, size or current
/// line changes) and, for a line with word timing, a strip covering just that
/// line (rebuilt as the sweep advances, tens of kilobytes each time).
pub(super) fn render_big_lyrics(
    frame: &mut Frame,
    app: &AppState,
    images: &mut ImageState,
    area: Rect,
    anchor: Anchor,
    alignment: Alignment,
) -> bool {
    let Some(picker) = images.picker.as_ref() else {
        return false;
    };
    if picker.protocol_type() != ratatui_image::picker::ProtocolType::Kitty {
        return false;
    }
    let (cell_w, cell_h) = real_cell_size(Some(picker));
    let (cell_w, cell_h) = (cell_w.max(1) as u32, cell_h.max(1) as u32);
    let big = &mut images.big_lyrics;
    let scale = match anchor {
        Anchor::Centered => big.scale_fullscreen,
        Anchor::Top => big.scale_compact,
    };
    if !big.enabled || big.fonts.is_none() || area.width < 8 || area.height < 3 {
        return false;
    }

    let size = (area.width as u32 * cell_w, area.height as u32 * cell_h);
    let px = cell_h as f32 * scale;
    let centered_text = alignment == Alignment::Center;
    let tmux = std::env::var_os("TMUX").is_some();

    let current_row = current_body_line_row(app);
    let swept_line = current_row.is_some() && current_line_has_words(app);
    let lines: Vec<StyledLine> = body_lines(app)
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let color_of = |style: Style| color_rgb(style.fg.unwrap_or(Color::White));
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let per_char: Vec<[u8; 3]> = line
                .spans
                .iter()
                .flat_map(|s| s.content.chars().map(move |_| color_of(s.style)))
                .collect();
            let rgb = if Some(i) == current_row {
                color_rgb(ACCENT)
            } else {
                line.spans
                    .first()
                    .map_or(color_rgb(Color::White), |s| color_of(s.style))
            };
            StyledLine {
                text,
                rgb,
                chars: (Some(i) == current_row && swept_line).then_some(per_char),
            }
        })
        .collect();
    let sweep_line = current_row
        .and_then(|i| lines.get(i))
        .filter(|l| l.chars.is_some());

    let key = cache_key(
        &lines,
        current_row,
        size,
        scale,
        anchor,
        alignment,
        sweep_line.is_some(),
    );
    let mut queued = std::mem::take(&mut big.payload);

    // The sheet, and for a swept line its two strip copies.
    if big.stale || big.built.as_ref().map(|b| b.key) != Some(key) {
        big.stale = false;
        let old = big.built.take();
        let fonts = big.fonts.as_mut().expect("checked above");
        let sheet = layout(fonts, &lines, px, size.0 as f32);
        let offset = scroll_offset(&sheet, current_row, anchor, size.1 as f32, px);
        let band = sweep_band(&sheet, current_row, offset, cell_h, size.1);
        let img = rasterize(
            fonts,
            &sheet,
            offset,
            size.0,
            px,
            centered_text,
            (0, size.1),
            band,
            None,
        );
        let Some(png) = encode_png(&img) else {
            return false;
        };
        let id = big.fresh_id();
        queued.before.push_str(&transmit_sequence(&png, id, tmux));
        let mut strip = None;
        if let (Some(band), Some(block)) = (band, current_row.and_then(|i| sheet.blocks.get(i))) {
            let mut copy = |tint: [u8; 3], big: &mut BigLyrics| -> Option<Layer> {
                let fonts = big.fonts.as_mut()?;
                let img = rasterize(
                    fonts,
                    &sheet,
                    offset,
                    size.0,
                    px,
                    centered_text,
                    band,
                    None,
                    Some(tint),
                );
                let png = encode_png(&img)?;
                let id = big.fresh_id();
                queued.before.push_str(&transmit_sequence(&png, id, tmux));
                Some(Layer { id })
            };
            let sung = copy(color_rgb(ACCENT), big);
            let unsung = copy(color_rgb(Color::White), big);
            let (Some(sung), Some(unsung)) = (sung, unsung) else {
                return false;
            };
            let fonts = big.fonts.as_mut().expect("checked above");
            let spans = char_spans(fonts, block, px, size.0, centered_text);
            strip = Some(Strip {
                sung,
                unsung,
                spans,
            });
        }
        if let Some(old) = old {
            queued.after.push_str(&delete_sequence(old.base.id, tmux));
            if let Some(old) = old.strip {
                queued.after.push_str(&delete_sequence(old.sung.id, tmux));
                queued.after.push_str(&delete_sequence(old.unsung.id, tmux));
            }
            if let Some(old) = old.edge {
                queued.after.push_str(&delete_sequence(old.id, tmux));
            }
        }
        big.built = Some(Built {
            key,
            sheet,
            offset,
            band,
            base: Layer { id },
            strip,
            edge: None,
        });
    }
    let mut built = big.built.take().expect("built above");
    let rows = area.height;
    let strip_rows = built
        .band
        .filter(|_| built.strip.is_some())
        .map(|(y0, y1)| (y0 / cell_h) as u16..(y1 / cell_h) as u16);

    // The swept line: which copy each cell shows, plus the small image under the
    // soft edge.
    let mut grid: Vec<Vec<Cell>> = Vec::new();
    if let (Some(strip), Some(range), Some((band_top, _))) = (&built.strip, &strip_rows, built.band)
        && let Some(block) = current_row.and_then(|i| built.sheet.blocks.get(i))
    {
        let n_chars = strip.spans.len();
        let sweep = match current_playhead(app) {
            None => Sweep::Columns,
            Some(p) if p <= 0.0 => Sweep::AllUnsung,
            Some(p) if p >= n_chars as f32 => Sweep::AllSung,
            Some(p) => edge_position(&strip.spans, p)
                .map_or(Sweep::Columns, |(x, row)| Sweep::Edge { x, row }),
        };
        let half = px * 0.3;
        // From this frame's colours: the cached sheet still holds the line's first.
        let sung_flags: Vec<bool> = sweep_line
            .and_then(|l| l.chars.as_ref())
            .map(|c| c.iter().map(|rgb| *rgb == color_rgb(ACCENT)).collect())
            .unwrap_or_default();
        for y in 0..(range.end - range.start) {
            let ya = (band_top + y as u32 * cell_h) as f32;
            let yb = ya + cell_h as f32;
            let overlapping = text_rows_in(block, built.offset, px, ya, yb);
            let cols = area.width as usize;
            let row_cells: Vec<Cell> = match sweep {
                Sweep::Columns => sung_columns(
                    block,
                    &strip.spans,
                    &sung_flags,
                    (built.offset, px),
                    (ya + yb) / 2.0,
                    (cell_w, area.width),
                )
                .into_iter()
                .map(|s| if s { Cell::Sung } else { Cell::Unsung })
                .collect(),
                Sweep::AllUnsung => vec![Cell::Unsung; cols],
                Sweep::AllSung => vec![
                    if overlapping.is_some() {
                        Cell::Sung
                    } else {
                        Cell::Unsung
                    };
                    cols
                ],
                Sweep::Edge { x, row } => match overlapping {
                    Some((first, last)) if first <= row && row <= last => (0..cols)
                        .map(|c| {
                            let (left, right) =
                                (c as f32 * cell_w as f32, (c + 1) as f32 * cell_w as f32);
                            if right <= x - half {
                                Cell::Sung
                            } else if left >= x + half {
                                Cell::Unsung
                            } else {
                                Cell::Soft
                            }
                        })
                        .collect(),
                    Some((_, last)) if last < row => vec![Cell::Sung; cols],
                    _ => vec![Cell::Unsung; cols],
                },
            };
            grid.push(row_cells);
        }

        // The extent of the soft cells, and the image that fills them.
        let soft_rows = grid
            .iter()
            .enumerate()
            .filter(|(_, r)| r.contains(&Cell::Soft))
            .map(|(i, _)| i as u16);
        let (rs, re) = (soft_rows.clone().min(), soft_rows.max());
        let soft_cols = grid
            .iter()
            .flat_map(|r| r.iter().enumerate())
            .filter(|(_, c)| **c == Cell::Soft)
            .map(|(i, _)| i as u16);
        let (c0, c1) = (soft_cols.clone().min(), soft_cols.max());
        let want = match (rs, re, c0, c1, sweep) {
            (Some(rs), Some(re), Some(c0), Some(c1), Sweep::Edge { x, row }) => {
                Some(((c0, c1 + 1, rs, re + 1, x.round() as i32), x, row))
            }
            _ => None,
        };
        if built.edge.as_ref().map(|e| e.key) != want.map(|w| w.0) {
            if let Some(old) = built.edge.take() {
                queued.after.push_str(&delete_sequence(old.id, tmux));
            }
            if let Some((key, x, row)) = want {
                let (c0, c1, rs, re, _) = key;
                let fonts = big.fonts.as_mut().expect("checked above");
                let img = rasterize_region(
                    fonts,
                    &built.sheet,
                    built.offset,
                    size.0,
                    px,
                    centered_text,
                    (
                        (c0 as u32 * cell_w, c1 as u32 * cell_w),
                        (band_top + rs as u32 * cell_h, band_top + re as u32 * cell_h),
                    ),
                    None,
                    Draw::Soft(Gradient {
                        edge_x: x,
                        half,
                        row,
                        sung: color_rgb(ACCENT),
                        unsung: color_rgb(Color::White),
                    }),
                );
                if let Some(png) = encode_png(&img) {
                    let id = big.fresh_id();
                    queued.before.push_str(&transmit_sequence(&png, id, tmux));
                    built.edge = Some(EdgeLayer { key, id });
                }
            }
        }
    }

    // Draw: the sheet minus the swept line's rows, then those rows run by run.
    let mut payload = Some(queued);
    let buf = frame.buffer_mut();
    // Rows above and below the strip; an empty range draws nothing.
    let sheet_rows = match &strip_rows {
        Some(s) => [0..s.start, s.end..rows],
        None => [0..rows, 0..0],
    };
    for range in sheet_rows {
        draw_placeholders(buf, area, built.base.id, range, 0..area.width, &mut payload);
    }
    if let (Some(strip), Some(range)) = (&built.strip, &strip_rows) {
        let rect = Rect {
            y: area.y + range.start,
            height: range.end - range.start,
            ..area
        };
        for (y, cells) in grid.iter().enumerate() {
            let y = y as u16;
            let mut start = 0usize;
            while start < cells.len() {
                let end = start
                    + cells[start..]
                        .iter()
                        .take_while(|c| **c == cells[start])
                        .count();
                let (layer, edge_rect) = match (cells[start], &built.edge) {
                    (Cell::Sung, _) => (strip.sung.id, None),
                    (Cell::Soft, Some(edge)) => {
                        let (c0, c1, rs, re, _) = edge.key;
                        let edge_rect = Rect {
                            x: area.x + c0,
                            y: rect.y + rs,
                            width: c1 - c0,
                            height: re - rs,
                        };
                        (edge.id, Some((edge_rect, c0, rs)))
                    }
                    _ => (strip.unsung.id, None),
                };
                match edge_rect {
                    Some((edge_rect, c0, rs)) => draw_placeholders(
                        buf,
                        edge_rect,
                        layer,
                        y - rs..y - rs + 1,
                        start as u16 - c0..end as u16 - c0,
                        &mut payload,
                    ),
                    None => draw_placeholders(
                        buf,
                        rect,
                        layer,
                        y..y + 1,
                        start as u16..end as u16,
                        &mut payload,
                    ),
                }
                start = end;
            }
        }
    }
    // Nothing drew (a strip covering every row of an empty sheet): keep it queued.
    big.payload = payload.unwrap_or_default();
    big.built = Some(built);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fonts() -> Option<FontSet> {
        FontSet::discover(None)
    }

    #[test]
    fn missing_font_file_is_not_a_font_set() {
        assert!(!std::path::Path::new("/definitely/not/a/font.ttf").is_file());
        // A bogus custom path is skipped; any system font may still be found.
        let set = FontSet::discover(Some("/definitely/not/a/font.ttf"));
        if let Some(set) = set {
            assert!(set.faces.iter().all(|f| f.path.is_file()));
        }
    }

    #[test]
    fn wrapping_never_exceeds_the_width_and_keeps_every_word() {
        let Some(mut fonts) = fonts() else { return };
        let text = "the quick brown fox jumps over the lazy dog again and again";
        let rows = wrap_rows(&mut fonts, text, 40.0, 400.0);
        assert!(rows.len() > 1);
        for Row { text, width, .. } in &rows {
            assert!(*width <= 400.0 + 0.01, "{text:?} is {width}px");
        }
        let rejoined = rows
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(rejoined, text);
    }

    #[test]
    fn one_overlong_word_is_split_rather_than_overflowing_forever() {
        let Some(mut fonts) = fonts() else { return };
        let rows = wrap_rows(
            &mut fonts,
            "supercalifragilisticexpialidocious",
            60.0,
            200.0,
        );
        assert!(rows.len() > 1);
        assert_eq!(
            rows.iter().map(|r| r.text.as_str()).collect::<String>(),
            "supercalifragilisticexpialidocious"
        );
    }

    #[test]
    fn wide_text_breaks_without_spaces() {
        let Some(mut fonts) = fonts() else { return };
        let text = "夜に駆ける夜に駆ける夜に駆ける夜に駆ける";
        let rows = wrap_rows(&mut fonts, text, 60.0, 300.0);
        if fonts.face_for('夜').is_some() {
            assert!(rows.len() > 1);
            assert_eq!(
                rows.iter().map(|r| r.text.as_str()).collect::<String>(),
                text
            );
        }
    }

    fn sheet_of(heights: &[f32]) -> Sheet {
        let mut top = 0.0;
        let blocks = heights
            .iter()
            .map(|&height| {
                let b = Block {
                    top,
                    height,
                    rgb: [0; 3],
                    chars: None,
                    rows: Vec::new(),
                };
                top += height;
                b
            })
            .collect();
        Sheet { blocks, total: top }
    }

    #[test]
    fn centered_puts_the_current_line_mid_viewport() {
        let sheet = sheet_of(&[50.0; 20]);
        let offset = scroll_offset(&sheet, Some(10), Anchor::Centered, 200.0, 40.0);
        // Block 10 spans 500..550, so its middle (525) lands at 100.
        assert_eq!(offset, 425.0);
    }

    #[test]
    fn centered_lets_the_first_line_reach_the_middle() {
        let sheet = sheet_of(&[50.0; 20]);
        assert!(scroll_offset(&sheet, Some(0), Anchor::Centered, 200.0, 40.0) < 0.0);
    }

    #[test]
    fn top_anchor_clamps_at_both_ends_like_the_text_view() {
        let sheet = sheet_of(&[50.0; 20]);
        assert_eq!(
            scroll_offset(&sheet, Some(0), Anchor::Top, 200.0, 40.0),
            0.0
        );
        assert_eq!(
            scroll_offset(&sheet, Some(19), Anchor::Top, 200.0, 40.0),
            sheet.total - 200.0
        );
    }

    #[test]
    fn a_short_status_message_is_centred_and_a_long_sheet_starts_at_the_top() {
        let short = sheet_of(&[50.0]);
        assert_eq!(
            scroll_offset(&short, None, Anchor::Centered, 250.0, 40.0),
            -100.0
        );
        let long = sheet_of(&[50.0; 20]);
        assert_eq!(
            scroll_offset(&long, None, Anchor::Centered, 250.0, 40.0),
            0.0
        );
    }

    #[test]
    fn rasterizing_draws_ink_inside_the_image_only() {
        let Some(mut fonts) = fonts() else { return };
        let lines = vec![StyledLine {
            text: "Hello".into(),
            rgb: [10, 200, 30],
            chars: None,
        }];
        let sheet = layout(&mut fonts, &lines, 40.0, 400.0);
        let img = rasterize(
            &mut fonts,
            &sheet,
            0.0,
            400,
            40.0,
            false,
            (0, 100),
            None,
            None,
        );
        let inked = img.pixels().filter(|p| p.0[3] > 0).count();
        assert!(inked > 100, "only {inked} inked pixels");
        assert!(
            img.pixels()
                .filter(|p| p.0[3] > 0)
                .all(|p| p.0[..3] == [10, 200, 30])
        );
    }

    #[test]
    fn the_cache_key_ignores_nothing_it_should_notice() {
        let key = |text: &str,
                   current: usize,
                   size: (u32, u32),
                   scale: f32,
                   anchor: Anchor,
                   sweep: bool| {
            let line = StyledLine {
                text: text.into(),
                rgb: [1, 2, 3],
                chars: None,
            };
            cache_key(
                &[line],
                Some(current),
                size,
                scale,
                anchor,
                Alignment::Left,
                sweep,
            )
        };
        let base = key("a", 0, (10, 10), 2.0, Anchor::Centered, false);
        assert_eq!(base, key("a", 0, (10, 10), 2.0, Anchor::Centered, false));
        assert_ne!(base, key("b", 0, (10, 10), 2.0, Anchor::Centered, false));
        assert_ne!(base, key("a", 1, (10, 10), 2.0, Anchor::Centered, false));
        assert_ne!(base, key("a", 0, (10, 11), 2.0, Anchor::Centered, false));
        assert_ne!(base, key("a", 0, (10, 10), 2.5, Anchor::Centered, false));
        assert_ne!(base, key("a", 0, (10, 10), 2.0, Anchor::Top, false));
        assert_ne!(base, key("a", 0, (10, 10), 2.0, Anchor::Centered, true));
    }

    #[test]
    fn the_sheet_without_the_strip_plus_the_strip_equals_the_whole_render() {
        let Some(mut fonts) = fonts() else { return };
        let sung = [0, 175, 95];
        let unsung = [255, 255, 255];
        let text = "hold me closer than before and again";
        let lines = vec![
            StyledLine {
                text: "the line before".into(),
                rgb: [190; 3],
                chars: None,
            },
            StyledLine {
                text: String::new(),
                rgb: [0; 3],
                chars: None,
            },
            StyledLine {
                text: text.into(),
                rgb: sung,
                chars: Some(
                    text.chars()
                        .enumerate()
                        .map(|(i, _)| if i < 10 { sung } else { unsung })
                        .collect(),
                ),
            },
            StyledLine {
                text: String::new(),
                rgb: [0; 3],
                chars: None,
            },
            StyledLine {
                text: "the line after".into(),
                rgb: [125; 3],
                chars: None,
            },
        ];
        let (w, h, cell_h, px) = (700u32, 600u32, 40u32, 60.0f32);
        let sheet = layout(&mut fonts, &lines, px, w as f32);
        let offset = scroll_offset(&sheet, Some(2), Anchor::Centered, h as f32, px);
        let band = sweep_band(&sheet, Some(2), offset, cell_h, h).expect("swept line has a band");
        assert_eq!(band.0 % cell_h, 0);
        assert_eq!(band.1 % cell_h, 0);

        let whole = rasterize(&mut fonts, &sheet, offset, w, px, false, (0, h), None, None);
        let base = rasterize(
            &mut fonts,
            &sheet,
            offset,
            w,
            px,
            false,
            (0, h),
            Some(band),
            None,
        );
        let strip = rasterize(&mut fonts, &sheet, offset, w, px, false, band, None, None);
        assert_eq!(strip.height(), band.1 - band.0);
        for y in 0..h {
            for x in 0..w {
                let expected = *whole.get_pixel(x, y);
                let got = if (band.0..band.1).contains(&y) {
                    *strip.get_pixel(x, y - band.0)
                } else {
                    *base.get_pixel(x, y)
                };
                assert_eq!(got, expected, "pixel ({x},{y})");
            }
        }
        // The strip really carries both colours of the sweep.
        let colours: std::collections::HashSet<[u8; 3]> = strip
            .pixels()
            .filter(|p| p.0[3] > 0)
            .map(|p| [p.0[0], p.0[1], p.0[2]])
            .collect();
        assert!(colours.contains(&sung) && colours.contains(&unsung));
    }

    #[test]
    fn a_line_without_word_timing_has_no_band() {
        let Some(mut fonts) = fonts() else { return };
        let lines = vec![StyledLine {
            text: "plain".into(),
            rgb: [1; 3],
            chars: None,
        }];
        let sheet = layout(&mut fonts, &lines, 40.0, 400.0);
        assert!(sweep_band(&sheet, Some(0), 0.0, 40, 400).is_none());
    }

    #[test]
    fn wrapped_rows_know_where_they_start_in_the_line() {
        let Some(mut fonts) = fonts() else { return };
        let text = "alpha beta gamma delta epsilon zeta eta theta";
        let chars: Vec<char> = text.chars().collect();
        let rows = wrap_rows(&mut fonts, text, 40.0, 220.0);
        assert!(rows.len() > 1);
        for row in &rows {
            let want: String = chars[row.start..row.start + row.text.chars().count()]
                .iter()
                .collect();
            assert_eq!(want, row.text);
        }
    }

    #[test]
    fn a_replaced_image_is_deleted_only_after_the_new_one_is_stored() {
        let seq_new = transmit_sequence(&[1, 2, 3], 7, false);
        let seq_del = delete_sequence(6, false);
        assert!(seq_new.contains("i=7,a=T"));
        assert!(seq_del.contains("a=d,d=I,i=6"));
        // The payload keeps `before` ahead of the cells and `after` behind them.
        use ratatui::buffer::Buffer;
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 4));
        let mut payload = Some(Payload {
            before: "NEW".into(),
            after: "DEL".into(),
        });
        draw_placeholders(
            &mut buf,
            Rect::new(0, 0, 4, 2),
            0xB0_0007,
            0..2,
            0..4,
            &mut payload,
        );
        let first = buf[(0, 0)].symbol();
        assert!(first.find("NEW").unwrap() < first.find('\u{10EEEE}').unwrap());
        assert!(first.find('\u{10EEEE}').unwrap() < first.find("DEL").unwrap());
        assert!(payload.is_none());
        assert!(!buf[(0, 1)].symbol().contains("NEW"));
    }

    #[test]
    fn only_the_requested_rows_are_drawn() {
        use ratatui::buffer::Buffer;
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 6));
        let mut payload = None;
        draw_placeholders(
            &mut buf,
            Rect::new(0, 0, 4, 6),
            0xB0_0007,
            0..2,
            0..4,
            &mut payload,
        );
        draw_placeholders(
            &mut buf,
            Rect::new(0, 0, 4, 6),
            0xB0_0007,
            4..6,
            0..4,
            &mut payload,
        );
        assert!(buf[(0, 0)].symbol().contains('\u{10EEEE}'));
        assert_eq!(buf[(0, 2)].symbol(), " ");
        assert_eq!(buf[(0, 3)].symbol(), " ");
        assert!(buf[(0, 5)].symbol().contains('\u{10EEEE}'));
    }

    #[test]
    fn indexed_accent_maps_to_its_cube_colour() {
        // xterm 35 is #00af5f.
        assert_eq!(color_rgb(Color::Indexed(35)), [0, 175, 95]);
    }

    fn sample_image() -> Option<image::RgbaImage> {
        let mut fonts = fonts()?;
        let lines: Vec<StyledLine> = (0..40)
            .flat_map(|i| {
                [
                    StyledLine {
                        text: format!("line {i} hold me closer than before and again tonight"),
                        rgb: [255; 3],
                        chars: None,
                    },
                    StyledLine {
                        text: String::new(),
                        rgb: [0; 3],
                        chars: None,
                    },
                ]
            })
            .collect();
        let px = 100.0;
        let sheet = layout(&mut fonts, &lines, px, 1150.0);
        Some(rasterize(
            &mut fonts,
            &sheet,
            600.0,
            1150,
            px,
            false,
            (0, 1200),
            None,
            None,
        ))
    }

    #[test]
    fn a_screen_sized_lyric_image_is_tens_of_kilobytes_not_megabytes() {
        let Some(img) = sample_image() else { return };
        let started = std::time::Instant::now();
        let png = encode_png(&img).unwrap();
        println!("png {} bytes in {:?}", png.len(), started.elapsed());
        assert!(png.len() < 400_000, "{} bytes", png.len());
        let back = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
        assert_eq!((back.width(), back.height()), (1150, 1200));
    }

    #[test]
    fn transmit_sequence_is_chunked_and_round_trips() {
        use base64::Engine;
        let png: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let seq = transmit_sequence(&png, 0xB0_0001, false);
        let pieces: Vec<&str> = seq
            .split("\x1b\\")
            .filter(|p| p.contains("_Gq=2,") && p.contains(";"))
            .collect();
        assert_eq!(pieces.len(), 4); // 10000 bytes / 3072 per chunk
        let mut decoded = Vec::new();
        for (i, piece) in pieces.iter().enumerate() {
            let (control, data) = piece.split_once(';').unwrap();
            assert_eq!(control.contains("i=11534337,a=T,U=1,f=100"), i == 0);
            assert_eq!(control.ends_with("m=1"), i < 3, "{control}");
            assert!(data.len() <= 4096);
            decoded.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap(),
            );
        }
        assert_eq!(decoded, png);
    }

    #[test]
    fn tmux_wraps_every_piece_for_passthrough() {
        let seq = transmit_sequence(&[1, 2, 3], 0xB0_0001, true);
        assert!(seq.starts_with("\x1bPtmux;\x1b\x1b_G"));
        assert!(seq.ends_with("\x1b\x1b\\\x1b\\"));
    }

    #[test]
    fn placeholders_carry_the_transmit_once_in_the_first_cell() {
        use ratatui::buffer::Buffer;
        let area = Rect::new(2, 1, 5, 3);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 6));
        let mut payload = Some(Payload {
            before: "TRANSMIT".into(),
            after: String::new(),
        });
        draw_placeholders(
            &mut buf,
            area,
            0xB0_0001,
            0..area.height,
            0..area.width,
            &mut payload,
        );
        assert!(buf[(2, 1)].symbol().starts_with("TRANSMIT"));
        assert!(!buf[(2, 2)].symbol().contains("TRANSMIT"));
        assert!(buf[(2, 2)].symbol().contains('\u{10EEEE}'));
        // Nothing outside the area is touched.
        assert_eq!(buf[(1, 1)].symbol(), " ");
        draw_placeholders(
            &mut buf,
            area,
            0xB0_0001,
            0..area.height,
            0..area.width,
            &mut None,
        );
        assert!(!buf[(2, 1)].symbol().contains("TRANSMIT"));
    }

    #[test]
    fn ids_are_distinct_in_a_row_and_encode_as_a_colour() {
        let mut big = BigLyrics::default();
        let (a, b) = (big.fresh_id(), big.fresh_id());
        assert_ne!(a, b);
        assert_eq!(a.to_be_bytes()[0], 0, "no id_extra needed");
        assert_ne!(a & 0xFFFFFF, 0);
    }

    fn image_ids(buf: &ratatui::buffer::Buffer) -> std::collections::BTreeSet<u32> {
        let mut ids = std::collections::BTreeSet::new();
        for cell in &buf.content {
            let symbol = cell.symbol();
            let mut rest = symbol;
            while let Some(at) = rest.find("\x1b[38;2;") {
                rest = &rest[at + 7..];
                let end = rest.find('m').unwrap();
                let rgb: Vec<u32> = rest[..end].split(';').map(|n| n.parse().unwrap()).collect();
                ids.insert(rgb[0] << 16 | rgb[1] << 8 | rgb[2]);
            }
        }
        ids
    }

    fn all_text(buf: &ratatui::buffer::Buffer) -> String {
        buf.content.iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn a_word_timed_current_line_sweeps_by_switching_cells_not_by_resending_images() {
        use crate::lyrics::{LyricLine, WordSeg};
        use ratatui::{Terminal, backend::TestBackend};
        let Some(fonts) = fonts() else { return };
        #[allow(deprecated)]
        let mut picker =
            ratatui_image::picker::Picker::from_fontsize(ratatui_image::FontSize::new(18, 40));
        picker.set_protocol_type(ratatui_image::picker::ProtocolType::Kitty);
        let mut images = ImageState {
            picker: Some(picker),
            big_lyrics: BigLyrics {
                enabled: true,
                scale_fullscreen: 2.0,
                scale_compact: 1.3,
                fonts: Some(fonts),
                ..BigLyrics::default()
            },
            ..ImageState::default()
        };
        let line = |t: f64| LyricLine {
            timestamp: Duration::from_secs_f64(t),
            text: "hello there friend".into(),
            words: ["hello ", "there ", "friend"]
                .iter()
                .enumerate()
                .map(|(i, w)| WordSeg {
                    text: (*w).into(),
                    start: t + i as f64,
                    end: t + i as f64 + 1.0,
                })
                .collect(),
        };
        let mut app = AppState::new(false, Default::default(), Default::default(), 0);
        app.lyrics = LyricsState::Synced(vec![line(0.0), line(10.0), line(20.0)]);
        app.current_line = Some(1);
        let mut terminal = Terminal::new(TestBackend::new(60, 30)).unwrap();
        let area = Rect::new(0, 0, 60, 30);
        let mut draw = |app: &AppState, images: &mut ImageState| {
            terminal
                .draw(|f| {
                    assert!(render_big_lyrics(
                        f,
                        app,
                        images,
                        area,
                        Anchor::Centered,
                        Alignment::Left
                    ));
                })
                .unwrap()
                .buffer
                .clone()
        };

        app.position = Duration::from_secs_f64(10.0);
        let first = draw(&app, &mut images);
        let ids = image_ids(&first);
        // All three are sent, but with nothing sung only two show in cells.
        assert_eq!(ids.len(), 2, "sheet and unsung copy: {ids:?}");
        assert_eq!(all_text(&first).matches("a=T").count(), 3);

        // Nothing sung yet: the swept line shows only the unsung copy.
        let built = images.big_lyrics.built.as_ref().unwrap();
        let strip = built.strip.as_ref().unwrap();
        let (sung_id, unsung_id) = (strip.sung.id, strip.unsung.id);
        let (y0, y1) = images.big_lyrics.built.as_ref().unwrap().band.unwrap();
        let strip_ids = |buf: &ratatui::buffer::Buffer| {
            let rows = (y0 / 40) as u16..(y1 / 40) as u16;
            let mut seen = std::collections::BTreeSet::new();
            for y in rows {
                for x in 0..60 {
                    seen.extend(image_ids_in(buf[(x, y)].symbol()));
                }
            }
            seen
        };
        assert!(strip_ids(&first).contains(&unsung_id));
        assert!(!strip_ids(&first).contains(&sung_id));

        // Same position again: nothing is re-sent.
        let again = draw(&app, &mut images);
        assert!(!all_text(&again).contains("a=T"));

        // The sweep moves on: the two copies stay put and only the small image under
        // the soft edge is sent, then replaced as the edge moves.
        app.position = Duration::from_secs_f64(10.5);
        let moved = draw(&app, &mut images);
        let text = all_text(&moved);
        assert_eq!(text.matches("a=T").count(), 1, "just the edge image");
        assert_eq!(text.matches("a=d").count(), 0);
        let soft_id = images
            .big_lyrics
            .built
            .as_ref()
            .unwrap()
            .edge
            .as_ref()
            .expect("an edge image while the voice is mid-line")
            .id;
        assert!(strip_ids(&moved).contains(&soft_id));
        assert!(strip_ids(&moved).contains(&unsung_id));
        assert!(
            strip_ids(&moved).contains(&sung_id),
            "text behind the edge is sung"
        );
        let sheet_ids = image_ids(&moved);
        assert!(ids.is_subset(&sheet_ids));

        app.position = Duration::from_secs_f64(10.6);
        let later = draw(&app, &mut images);
        let text = all_text(&later);
        assert_eq!(text.matches("a=T").count(), 1, "the edge image is replaced");
        assert_eq!(text.matches("a=d").count(), 1, "and the old one freed");
        let after = images
            .big_lyrics
            .built
            .as_ref()
            .unwrap()
            .edge
            .as_ref()
            .unwrap()
            .id;
        assert_ne!(after, soft_id);
        assert!(!all_text(&later).contains(&format!("i={soft_id},a=T")));

        // The line finishes: the edge image goes away, everything is sung.
        app.position = Duration::from_secs_f64(14.0);
        let done = draw(&app, &mut images);
        let text = all_text(&done);
        assert_eq!(text.matches("a=T").count(), 0);
        assert_eq!(text.matches("a=d").count(), 1);
        assert!(images.big_lyrics.built.as_ref().unwrap().edge.is_none());
        assert!(!strip_ids(&done).contains(&unsung_id));

        // A new line sends fresh images and frees the old ones.
        app.current_line = Some(2);
        app.position = Duration::from_secs_f64(20.0);
        let next = draw(&app, &mut images);
        assert_eq!(all_text(&next).matches("a=T").count(), 3);
        assert_eq!(all_text(&next).matches("a=d").count(), 3);
    }

    fn image_ids_in(symbol: &str) -> Vec<u32> {
        let mut ids = Vec::new();
        let mut rest = symbol;
        while let Some(at) = rest.find("\x1b[38;2;") {
            rest = &rest[at + 7..];
            let end = rest.find('m').unwrap();
            let rgb: Vec<u32> = rest[..end].split(';').map(|n| n.parse().unwrap()).collect();
            ids.push(rgb[0] << 16 | rgb[1] << 8 | rgb[2]);
        }
        ids
    }

    #[test]
    fn sung_columns_follow_the_sung_characters() {
        let spans: Vec<Option<CharSpan>> = (0..6)
            .map(|i| {
                Some(CharSpan {
                    row: 0,
                    x0: i as f32 * 36.0,
                    x1: (i + 1) as f32 * 36.0,
                })
            })
            .collect();
        let sung = [true, true, false, false, false, false];
        let block = Block {
            top: 100.0,
            height: 60.0,
            rgb: [0; 3],
            chars: None,
            rows: Vec::new(),
        };
        // 18 px cells: two columns per character, so columns 0..4 are sung.
        let cols = sung_columns(&block, &spans, &sung, (0.0, 50.0), 130.0, (18, 12));
        assert_eq!(
            cols,
            [
                true, true, true, true, false, false, false, false, false, false, false, false
            ]
        );
        // A cell row above the line is not part of it.
        let above = sung_columns(&block, &spans, &sung, (0.0, 50.0), 60.0, (18, 12));
        assert!(above.iter().all(|s| !s));
    }

    #[test]
    fn char_spans_cover_each_character_in_order_across_wrapped_rows() {
        let Some(mut fonts) = fonts() else { return };
        let text = "alpha beta gamma delta epsilon";
        let lines = vec![StyledLine {
            text: text.into(),
            rgb: [1; 3],
            chars: Some(vec![[1; 3]; text.chars().count()]),
        }];
        let sheet = layout(&mut fonts, &lines, 40.0, 220.0);
        let block = &sheet.blocks[0];
        assert!(block.rows.len() > 1);
        let spans = char_spans(&mut fonts, block, 40.0, 220, false);
        let mut last = (0usize, -1.0f32);
        for span in spans.iter().flatten() {
            assert!(span.x1 > span.x0);
            if span.row == last.0 {
                assert!(span.x0 >= last.1 - 0.01, "characters advance along a row");
            }
            last = (span.row, span.x1);
        }
        assert_eq!(
            spans.iter().flatten().map(|s| s.row).max(),
            Some(block.rows.len() - 1)
        );
    }

    #[test]
    fn the_gradient_is_sung_behind_unsung_ahead_and_blends_through_the_edge() {
        let g = Gradient {
            edge_x: 100.0,
            half: 20.0,
            row: 1,
            sung: [0, 200, 100],
            unsung: [255, 255, 255],
        };
        assert_eq!(g.colour_at(1, 70.0), [0, 200, 100]);
        assert_eq!(g.colour_at(1, 130.0), [255, 255, 255]);
        let mid = g.colour_at(1, 100.0);
        assert!(mid[0] > 100 && mid[0] < 155, "halfway: {mid:?}");
        // Monotonic across the edge: never a flash of the wrong colour.
        let mut last = 0u8;
        for x in 60..140 {
            let c = g.colour_at(1, x as f32)[0];
            assert!(c >= last, "red goes sung -> unsung without dipping at {x}");
            last = c;
        }
        // Rows above the edge's are all sung, below all unsung.
        assert_eq!(g.colour_at(0, 500.0), [0, 200, 100]);
        assert_eq!(g.colour_at(2, 0.0), [255, 255, 255]);
    }

    #[test]
    fn a_clipped_region_is_exactly_that_piece_of_the_full_render() {
        let Some(mut fonts) = fonts() else { return };
        let text = "hold me closer than before";
        let lines = vec![StyledLine {
            text: text.into(),
            rgb: [10, 200, 30],
            chars: None,
        }];
        let px = 60.0;
        let sheet = layout(&mut fonts, &lines, px, 700.0);
        let whole = rasterize(
            &mut fonts,
            &sheet,
            -40.0,
            700,
            px,
            false,
            (0, 300),
            None,
            None,
        );
        let piece = rasterize_region(
            &mut fonts,
            &sheet,
            -40.0,
            700,
            px,
            false,
            ((180, 396), (40, 160)),
            None,
            Draw::Plain(None),
        );
        assert_eq!((piece.width(), piece.height()), (216, 120));
        for y in 0..120 {
            for x in 0..216 {
                assert_eq!(
                    piece.get_pixel(x, y),
                    whole.get_pixel(x + 180, y + 40),
                    "({x},{y})"
                );
            }
        }
        assert!(piece.pixels().any(|p| p.0[3] > 0), "the piece has ink");
    }

    #[test]
    fn the_soft_edge_image_recolours_glyphs_smoothly_across_the_edge() {
        let Some(mut fonts) = fonts() else { return };
        let text = "mmmmmmmmmm";
        let lines = vec![StyledLine {
            text: text.into(),
            rgb: [0, 200, 100],
            chars: Some(vec![[0, 200, 100]; text.chars().count()]),
        }];
        let px = 80.0;
        let sheet = layout(&mut fonts, &lines, px, 900.0);
        let spans = char_spans(&mut fonts, &sheet.blocks[0], px, 900, false);
        let edge = spans[5].unwrap().x0;
        let img = rasterize_region(
            &mut fonts,
            &sheet,
            0.0,
            900,
            px,
            false,
            ((0, 900), (0, 120)),
            None,
            Draw::Soft(Gradient {
                edge_x: edge,
                half: 24.0,
                row: 0,
                sung: [0, 200, 100],
                unsung: [255, 255, 255],
            }),
        );
        // Ink well left of the edge is sung, well right of it unsung, and the
        // reds in between rise gradually (more than two distinct steps).
        let red_at = |x0: u32, x1: u32| -> Vec<u8> {
            let mut reds = Vec::new();
            for x in x0..x1 {
                for y in 0..120 {
                    let p = img.get_pixel(x, y);
                    if p.0[3] > 240 {
                        reds.push(p.0[0]);
                    }
                }
            }
            reds
        };
        assert!(red_at(0, (edge - 30.0) as u32).iter().all(|r| *r == 0));
        assert!(red_at((edge + 30.0) as u32, 900).iter().all(|r| *r == 255));
        let mut steps: Vec<u8> = red_at((edge - 24.0) as u32, (edge + 24.0) as u32);
        steps.sort_unstable();
        steps.dedup();
        assert!(steps.len() > 4, "a gradient, not a hard cut: {steps:?}");
    }

    #[test]
    fn the_edge_sits_inside_the_character_being_sung() {
        let spans = vec![
            Some(CharSpan {
                row: 0,
                x0: 0.0,
                x1: 40.0,
            }),
            Some(CharSpan {
                row: 0,
                x0: 40.0,
                x1: 80.0,
            }),
            None,
            Some(CharSpan {
                row: 1,
                x0: 0.0,
                x1: 40.0,
            }),
        ];
        assert_eq!(edge_position(&spans, 0.0), Some((0.0, 0)));
        assert_eq!(edge_position(&spans, 1.5), Some((60.0, 0)));
        // A character with no span (trimmed space) lands on the next real one.
        assert_eq!(edge_position(&spans, 2.5), Some((0.0, 1)));
    }

    #[test]
    fn text_rows_in_finds_the_rows_a_cell_row_touches() {
        let block = Block {
            top: 100.0,
            height: 240.0,
            rgb: [0; 3],
            chars: None,
            rows: (0..2)
                .map(|i| Row {
                    text: "x".into(),
                    width: 10.0,
                    start: i,
                })
                .collect(),
        };
        // px 100, pitch 120: row 0 is y 100..220, row 1 is 220..340.
        assert_eq!(text_rows_in(&block, 0.0, 100.0, 120.0, 160.0), Some((0, 0)));
        assert_eq!(text_rows_in(&block, 0.0, 100.0, 200.0, 240.0), Some((0, 1)));
        assert_eq!(text_rows_in(&block, 0.0, 100.0, 260.0, 300.0), Some((1, 1)));
        assert_eq!(text_rows_in(&block, 0.0, 100.0, 0.0, 80.0), None);
        assert_eq!(text_rows_in(&block, 0.0, 100.0, 400.0, 440.0), None);
    }
}
