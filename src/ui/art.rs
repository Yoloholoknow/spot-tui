use super::*;

pub(super) const ART_MIN_WIDTH: u16 = 14;
pub(super) const ART_MAX_WIDTH: u16 = 26;
/// The compact hero's gauge sits beside the art in a much wider text column.
/// Unconstrained it would stretch across mostly empty space, so it is capped at a
/// fixed width.
pub(super) const COMPACT_GAUGE_MAX_WIDTH: u16 = 44;

/// The real per-cell pixel size, for sizing the art card to a pixel square. Reads
/// `Picker::font_size()`, which `terminal::detect_graphics_picker` corrects once
/// at startup. Reading it from one place keeps this layout math and
/// `ratatui-image`'s encoder agreeing; querying the ioctl again here made them
/// disagree and the image came out pixelated. Falls back to a 2:1 guess when
/// there is no `Picker`.
pub(super) fn real_cell_size(picker: Option<&ratatui_image::picker::Picker>) -> (u16, u16) {
    match picker.map(|p| p.font_size()) {
        Some(font) if font.width > 0 && font.height > 0 => (font.width, font.height),
        _ => (1, 2),
    }
}

/// `render_art` wraps the image in `Borders::ALL`, which removes one cell per side.
/// Cells are not square, so those two rows and two columns cost different numbers
/// of pixels, and the skew shows on small cards (the compact hero is at most
/// `ART_MAX_WIDTH` wide). So the *inner*, post-border region is squared first and
/// the border added back, which keeps the finished card square.
pub(super) const ART_BORDER_CELLS: u16 = 2;

/// Square-sizing math for a known cell pixel size, kept apart from detection so it
/// is testable. Returns the outer (pre-border) height that makes the inner region
/// a pixel square; see `ART_BORDER_CELLS`.
pub(super) fn square_height_cells(width_cells: u16, cell_size: (u16, u16)) -> u16 {
    let (cell_w, cell_h) = cell_size;
    if cell_h == 0 {
        return (width_cells / 2).max(1);
    }
    let inner_width = width_cells.saturating_sub(ART_BORDER_CELLS).max(1);
    let inner_height = ((inner_width as u32 * cell_w as u32) / cell_h as u32).max(1) as u16;
    inner_height + ART_BORDER_CELLS
}

/// The inverse of `square_height_cells`, for the one call site that
/// picks a height first and needs the matching square width (the
/// narrow-terminal fullscreen fallback). Same border-aware math, mirrored.
pub(super) fn square_width_cells(height_cells: u16, cell_size: (u16, u16)) -> u16 {
    let (cell_w, cell_h) = cell_size;
    if cell_w == 0 {
        return (height_cells * 2).max(1);
    }
    let inner_height = height_cells.saturating_sub(ART_BORDER_CELLS).max(1);
    let inner_width = ((inner_height as u32 * cell_h as u32) / cell_w as u32).max(1) as u16;
    inner_width + ART_BORDER_CELLS
}

#[cfg(test)]
mod square_cells_tests {
    use super::*;

    #[test]
    fn zero_cell_dimension_falls_back_to_the_2_to_1_approximation() {
        assert_eq!(square_height_cells(26, (0, 0)), 13);
        assert_eq!(square_width_cells(13, (0, 0)), 26);
    }

    #[test]
    fn exact_2_to_1_cell_size_still_needs_one_extra_row_for_the_border() {
        // Naive flat math (26/2) says 13, but that's the *inner* square's
        // height -- the outer, bordered card needs 2 more cells of
        // height than a naive width/2 would suggest, then squared
        // against the inner width (24, not 26): 24*8/16 = 12, +2 border
        // rows = 14.
        assert_eq!(square_height_cells(26, (8, 16)), 14);
    }

    #[test]
    fn a_taller_real_cell_needs_fewer_rows_for_the_same_square() {
        // 8x18 (2.25:1) is taller-per-cell than the flat 2:1 fallback
        // assumes -- fewer rows are needed to reach the same real-pixel
        // width.
        let height = square_height_cells(26, (8, 18));
        assert!(
            height < 13,
            "expected fewer than the 2:1 fallback's 13 rows, got {height}"
        );
    }

    #[test]
    fn a_wider_real_cell_needs_more_rows_for_the_same_square() {
        // 8x12 (1.5:1) is wider-per-cell (relatively) than the flat 2:1
        // fallback assumes -- more rows are needed, not fewer. This is
        // the actual shape of the live-reported bug: a wrongly-trusted
        // font size closer to this end of the ratio than assumed is
        // what left the card too wide (too few rows) relative to a true
        // square.
        let height = square_height_cells(26, (8, 12));
        assert!(
            height > 13,
            "expected more than the 2:1 fallback's 13 rows, got {height}"
        );
    }

    #[test]
    fn the_inner_post_border_region_is_the_true_square_not_the_outer_card() {
        // The actual property this fix exists for: it's the *inner*
        // (post-border) region that must be a real pixel square, not
        // the outer card -- verified directly here rather than only
        // indirectly through the exact-2:1 case above.
        let cell = (8u32, 20u32); // 2.5:1, deliberately not exactly 2:1
        let outer_width = 26u16;
        let outer_height = square_height_cells(outer_width, (cell.0 as u16, cell.1 as u16));
        let inner_width = (outer_width - ART_BORDER_CELLS) as u32;
        let inner_height = (outer_height - ART_BORDER_CELLS) as u32;
        let (inner_w_px, inner_h_px) = (inner_width * cell.0, inner_height * cell.1);
        assert!(
            inner_w_px.abs_diff(inner_h_px) <= cell.1,
            "inner region not square: {inner_w_px}px wide vs {inner_h_px}px tall"
        );
    }

    #[test]
    fn square_width_cells_round_trips_within_integer_rounding_slack() {
        // Two floor-divisions in a row (width->height, then height back
        // to width) can lose at most a couple of cells to truncation --
        // not an exact inverse, but close enough that the resulting card
        // still reads as square, which is all this is for.
        let width = 26;
        let height = square_height_cells(width, (8, 18));
        let round_tripped = square_width_cells(height, (8, 18));
        assert!(
            width.abs_diff(round_tripped) <= 2,
            "expected {round_tripped} to be within 2 cells of the original {width}"
        );
    }
}

pub(super) fn hash_bytes(s: &str) -> u32 {
    let mut h: u32 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    h
}

/// A deterministic placeholder for album art, used when no graphics protocol
/// is available. `Color::Indexed`, not `Rgb`, like `ACCENT`:
/// renders correctly on plain 256-color terminals, not just truecolor
/// ones. Kept to the middle of the 6-step color cube's range (1..=4 per
/// channel, out of 0..=5) so it reads as "colorful art," not a
/// near-black or near-white cube corner that would wash out the
/// monogram text sitting on top of it.
pub(super) fn art_color(artist: &str, album: &str) -> Color {
    let h = hash_bytes(&format!("{artist}{album}"));
    let r = 1 + (h % 4) as u16;
    let g = 1 + ((h / 4) % 4) as u16;
    let b = 1 + ((h / 16) % 4) as u16;
    Color::Indexed((16 + 36 * r + 6 * g + b) as u8)
}

pub(super) fn monogram(artist: &str, album: &str) -> String {
    let first_upper = |s: &str| {
        s.chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default()
    };
    format!("{}{}", first_upper(artist), first_upper(album))
}

/// A visible frame around the fill, not a borderless rectangle -- reads
/// as a distinct thumbnail card sitting on the pane background, closer
/// to the reference's crisp album-art card, instead of the color block
/// blending into whatever's behind it with no edge at all.
/// A fixed-width strip horizontally centered within a wider one. Applied
/// to two rows of the *same* parent column (the art card's row and the
/// progress gauge's row), it returns byte-identical x/width for both --
/// the actual fix for the gauge stretching to the full column while the
/// art card above it stayed narrow: they were computing their own
/// centering independently before, which is how the two drifted apart.
pub(super) fn capsule_row(area: Rect, width: u16) -> Rect {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(width),
            Constraint::Min(0),
        ])
        .split(area)[1]
}

#[cfg(test)]
mod capsule_row_tests {
    use super::*;

    #[test]
    fn the_gauge_capsule_lands_on_the_same_columns_as_the_art_card() {
        let column = Rect::new(3, 0, 100, 40);
        let art = capsule_row(
            Rect {
                y: 2,
                height: 20,
                ..column
            },
            44,
        );
        let gauge = capsule_row(
            Rect {
                y: 26,
                height: 1,
                ..column
            },
            44,
        );
        assert_eq!((art.x, art.width), (gauge.x, gauge.width));
    }
}

/// Scales `image` up just enough that it fully covers a `target_w` x
/// `target_h` box (never leaves either axis short), then center-crops
/// whatever overflows on the other axis -- CSS `background-size: cover`,
/// not `contain`. See `render_art`'s own doc comment on this call site for
/// why: `ratatui-image`'s `Resize::Scale`/`Fit` both *fit within* their
/// target (confirmed by reading its `resize_pixels`), so any mismatch
/// between an integer terminal-cell count and the real pixel square it's
/// meant to approximate always showed up as a blank letterboxed gap, never
/// distortion. Doing the crop ourselves, before `ratatui-image` ever sees
/// the image, means there's no aspect mismatch left for it to mishandle.
pub(super) fn cover_crop(
    image: &image::DynamicImage,
    target_w: u32,
    target_h: u32,
) -> image::DynamicImage {
    let (target_w, target_h) = (target_w.max(1), target_h.max(1));
    let (src_w, src_h) = (image.width().max(1), image.height().max(1));
    let scale = (target_w as f64 / src_w as f64).max(target_h as f64 / src_h as f64);
    let scaled_w = ((src_w as f64 * scale).ceil() as u32).max(target_w);
    let scaled_h = ((src_h as f64 * scale).ceil() as u32).max(target_h);
    let scaled = image.resize_exact(scaled_w, scaled_h, image::imageops::FilterType::Lanczos3);
    let x = (scaled_w - target_w) / 2;
    let y = (scaled_h - target_h) / 2;
    scaled.crop_imm(x, y, target_w, target_h)
}

#[cfg(test)]
mod cover_crop_tests {
    use super::*;

    fn image(w: u32, h: u32) -> image::DynamicImage {
        image::DynamicImage::ImageRgb8(image::RgbImage::new(w, h))
    }

    #[test]
    fn a_square_source_into_a_wider_target_fills_it_exactly() {
        let out = cover_crop(&image(640, 640), 24, 10);
        assert_eq!((out.width(), out.height()), (24, 10));
    }

    #[test]
    fn a_square_source_into_a_taller_target_fills_it_exactly() {
        let out = cover_crop(&image(640, 640), 10, 24);
        assert_eq!((out.width(), out.height()), (10, 24));
    }

    #[test]
    fn an_already_matching_aspect_ratio_still_produces_the_exact_target_size() {
        let out = cover_crop(&image(400, 400), 20, 20);
        assert_eq!((out.width(), out.height()), (20, 20));
    }

    #[test]
    fn a_target_larger_than_the_source_upscales_rather_than_leaving_a_gap() {
        let out = cover_crop(&image(10, 10), 100, 40);
        assert_eq!((out.width(), out.height()), (100, 40));
    }

    #[test]
    fn a_wide_source_into_a_square_target_still_fills_it_exactly() {
        let out = cover_crop(&image(1000, 200), 30, 30);
        assert_eq!((out.width(), out.height()), (30, 30));
    }
}

/// Draws the card border once, then dispatches to a real rendered cover
/// image (`images.cover`, when it's actually built for the track
/// currently playing) or the hashed-color monogram placeholder --
/// exactly one of the two ever renders into `inner`, never both. Falling
/// back to the placeholder is a complete, good-looking state on its own
/// (not a degraded one), covering every non-error-worthy reason a real
/// image might not be showing yet: no real graphics protocol on this
/// terminal, no cover fetched yet for this track, or the fetch/decode
/// itself failing.
pub(super) fn render_art(
    frame: &mut Frame,
    app: &AppState,
    images: &mut ImageState,
    artist: &str,
    album: &str,
    area: Rect,
) {
    if let Some(deadline) = images.startup_retransmit_at
        && std::time::Instant::now() >= deadline
    {
        images.startup_retransmit_at = None;
        images.startup_retransmit_done = true;
        images.sized_covers.clear();
    }
    if area.height == 0 || area.width == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DIM));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let current_uri = app.current_track_uri.as_deref();
    let cover = images
        .cover_image
        .as_ref()
        .filter(|(uri, _)| Some(uri.as_str()) == current_uri)
        .map(|(uri, image)| (uri.clone(), image.clone()));

    if let (Some(picker), Some((uri, image))) = (images.picker.clone(), cover) {
        let cached = images
            .sized_covers
            .iter_mut()
            .find(|(u, w, h, _)| *u == uri && *w == inner.width && *h == inner.height);

        let proto = if let Some((_, _, _, proto)) = cached {
            proto
        } else {
            // Round 9 of the art-card sizing saga: three fixes in a row
            // (font-size source, border-thickness math, centering) each
            // addressed a real, verified defect, and the compact hero's
            // border still doesn't tightly match the image. Rather than
            // guess a 4th number, log every real value that feeds the
            // square math -- exactly once per distinct size this cache
            // actually builds for, not every frame -- so the next real
            // run gives actual numbers to compare against what a true
            // square needs, instead of another screenshot to eyeball.
            let cell = real_cell_size(Some(&picker));
            // `log::debug!` would be silently dropped -- this app's own
            // env_logger filter (`main.rs`) is `"info,librespot=debug"`,
            // base level `info`, not `debug`, for anything outside the
            // librespot crates. Using `info!` here is what actually
            // makes this diagnostic show up in the log file at all.
            log::info!(
                "art size: area={area:?} inner={inner:?} cell_size={cell:?} image_native=({}, {})",
                image.width(),
                image.height()
            );
            // Nine rounds of tuning `art_width`/`art_height`'s cell-count
            // math (font-size source, border-thickness, centering) each
            // fixed a real defect, but the border still doesn't tightly
            // hug the image -- confirmed from this exact log line: a
            // 24x10-cell inner region at an 18x40px cell is 432x400 real
            // pixels, an unavoidable ~8% mismatch no integer cell count
            // can close (24 cells can't split evenly into a 40px-tall
            // grid the way 400/18 would need). `Resize::Scale` (below,
            // previously) *fits within* whatever target it's given
            // (confirmed by reading `resize_pixels`: both `Fit` and
            // `Scale` call `image.resize`, which never overflows its
            // target) -- so any such mismatch was never going to do
            // anything but letterbox. Fixed at the root instead of
            // tuning the cell math further: `cover_crop` pre-scales and
            // center-crops the image to the *exact* real pixel size this
            // cell region will occupy, before `ratatui-image` ever sees
            // it -- no cell-to-pixel rounding left for it to get wrong.
            let target_w = inner.width as u32 * cell.0.max(1) as u32;
            let target_h = inner.height as u32 * cell.1.max(1) as u32;
            let image = cover_crop(&image, target_w, target_h);
            if images.sized_covers.len() >= SIZED_COVER_CACHE_CAP {
                images.sized_covers.remove(0);
            }
            let built = picker.new_resize_protocol(image);
            images
                .sized_covers
                .push((uri, inner.width, inner.height, built));
            // Arms only off this process's very first-ever cache build,
            // never again -- see `ImageState::startup_retransmit_at`'s
            // own doc comment for the full reasoning.
            if images.startup_retransmit_at.is_none() && !images.startup_retransmit_done {
                images.startup_retransmit_at =
                    Some(std::time::Instant::now() + STARTUP_RETRANSMIT_DELAY);
            }
            &mut images.sized_covers.last_mut().unwrap().3
        };

        // `Resize::Scale`, not `Crop`: `Crop` exists for terminals that need to avoid
        // overdrawing characters over graphics, uses a different, less precise
        // transmission path, and rendered visibly pixelated even on an already-sized
        // image. `cover_crop`'s pre-sizing is what actually closes the gap beside the
        // image.
        let widget =
            ratatui_image::StatefulImage::default().resize(ratatui_image::Resize::Scale(None));
        frame.render_stateful_widget(widget, inner, proto);
        return;
    }

    render_art_placeholder(frame, artist, album, inner);
}

pub(super) fn render_art_placeholder(frame: &mut Frame, artist: &str, album: &str, inner: Rect) {
    let bg = art_color(artist, album);
    frame.render_widget(Block::default().style(Style::default().bg(bg)), inner);
    let text_style = Style::default()
        .bg(bg)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let top_pad = inner.height / 2;
    let mut lines: Vec<Line> = (0..top_pad).map(|_| Line::from("")).collect();
    lines.push(Line::from(monogram(artist, album)));
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .style(text_style),
        inner,
    );
}
