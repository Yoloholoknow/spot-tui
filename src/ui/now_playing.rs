use super::*;

pub(super) fn render_compact(
    frame: &mut Frame,
    app: &AppState,
    images: &mut ImageState,
    area: Rect,
) {
    match (&app.track_artist, &app.track_title) {
        (Some(artist), Some(title)) => {
            render_now_playing_hero(frame, app, images, artist, title, area)
        }
        _ => render_now_playing_idle(frame, app, area),
    }
}

/// Nothing loaded yet: no art block at all, since a coloured placeholder would
/// imply a track exists.
pub(super) fn render_now_playing_idle(frame: &mut Frame, app: &AppState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(header(app, area.width as usize))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(body_lines(app)).wrap(Wrap { trim: true }),
        chunks[2],
    );
}

/// Art plus title and transport on one row, with the lyrics given the room below.
/// `hero_height` scales with the pane but is capped: this is a glance screen and
/// lyrics should dominate the vertical space.
pub(super) fn render_now_playing_hero(
    frame: &mut Frame,
    app: &AppState,
    images: &mut ImageState,
    artist: &str,
    title: &str,
    area: Rect,
) {
    // Below ART_MIN_WIDTH the block would crush the monogram illegibly --
    // in a narrow pane, skip the art entirely rather than render
    // something unreadable just to say there's art.
    let art_width = (area.width / 4).clamp(ART_MIN_WIDTH, ART_MAX_WIDTH);
    let show_art = area.width >= art_width + 24;
    // A real pixel square, not a flat "half as tall as wide" guess, which
    // made the card too tall; see `square_height_cells`.
    let art_height = square_height_cells(art_width, real_cell_size(images.picker.as_ref()));

    // Clamp: at least `art_height + 2` and at least 9 rows, at most 13. The `+ 2` is
    // slack for the two `Min(1)` spacers around the art card; with only
    // `art_height`, ratatui shrinks the `Length(art_height)` card by a row to fit
    // them and `render_art` gets a card one row short of square. The 9-row floor is
    // what `meta_chunks` below needs (the bordered gauge row is 3 high); a shorter
    // hero would shrink it the same silent way.
    let hero_height = (area.height / 2)
        .clamp(9, 13)
        .max(art_height + 2)
        .min(area.height);
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(hero_height),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(area);

    let hero_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if show_art {
            vec![Constraint::Length(art_width), Constraint::Min(1)]
        } else {
            vec![Constraint::Min(1)]
        })
        .split(outer[0]);

    let album = app.track_album.as_deref().unwrap_or(title);
    let mut art_top_row: Option<u16> = None;
    let meta_area = if show_art {
        // The column reserves `hero_height` rows but the card needs only `art_height` to
        // stay square; a taller target would leave a blank gap beside the scaled image.
        //
        // The top margin is a fixed `Length(1)`, not a symmetric `Min(1)` pair, to match
        // the text column's fixed spacer: otherwise the card's centring offset and the
        // title's fixed offset drift apart whenever `hero_height` exceeds
        // `art_height + 2`, and the title no longer lines up with the top of the frame.
        let art_area = Layout::default()
            .constraints([
                Constraint::Length(1),
                Constraint::Length(art_height),
                Constraint::Min(1),
            ])
            .split(hero_cols[0])[1];
        art_top_row = Some(art_area.y);
        render_art(frame, app, images, artist, album, art_area);
        hero_cols[1]
    } else {
        hero_cols[0]
    };

    // The title starts one row below the art border (a `Length(2)` spacer), not on
    // its row. Box-drawing corners draw their ink from the vertical centre of a cell
    // while text glyphs sit near the top, so even on the same buffer row the title
    // reads as floating above the border line. Exact alignment is impossible at row
    // granularity; this lines the title up with where the art's pixels start.
    let meta_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // spacer (one extra row -- see above)
            Constraint::Length(1), // title
            Constraint::Length(1), // artist -- album
            Constraint::Length(1), // spacer
            Constraint::Length(1), // icon + time + vol
            Constraint::Length(3), // gauge (bordered -- matches the fullscreen views)
        ])
        .split(meta_area);

    if let Some(art_row) = art_top_row {
        let title_row = meta_chunks[1].y;
        thread_local! {
            static LAST_LOGGED: std::cell::Cell<Option<(u16, u16)>> = const { std::cell::Cell::new(None) };
        }
        let pair = (art_row, title_row);
        let changed = LAST_LOGGED.with(|c| c.replace(Some(pair)) != Some(pair));
        if changed {
            log::info!(
                "hero alignment: art_top_row={art_row} title_row={title_row} (one_below_border={})",
                title_row == art_row + 1
            );
        }
    }

    frame.render_widget(
        Paragraph::new(truncate_ellipsis(title, meta_area.width as usize))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        meta_chunks[1],
    );
    let artist_album = match &app.track_album {
        Some(al) => format!("{artist} \u{2014} {al}"),
        None => artist.to_string(),
    };
    frame.render_widget(
        Paragraph::new(truncate_ellipsis(&artist_album, meta_area.width as usize))
            .style(Style::default().fg(DIM)),
        meta_chunks[2],
    );
    if let Some(label) = &app.context_label {
        frame.render_widget(
            Paragraph::new(truncate_ellipsis(
                &format!("Playing from {label}"),
                meta_area.width as usize,
            ))
            .style(Style::default().fg(DIM)),
            meta_chunks[3],
        );
    }
    frame.render_widget(
        Paragraph::new(format!(
            "{} {}   {}",
            playing_icon(app),
            time_readout(app),
            volume_readout(app)
        )),
        meta_chunks[4],
    );
    let gauge_area = Rect {
        width: meta_chunks[5].width.min(COMPACT_GAUGE_MAX_WIDTH),
        ..meta_chunks[5]
    };
    frame.render_widget(progress_gauge_bordered(app), gauge_area);

    frame.render_widget(Block::default().borders(Borders::TOP), outer[1]);
    let lyrics_area = render_lyrics_credit(frame, app, outer[2], Alignment::Left);
    let lines = body_lines(app);
    let offset = top_anchored_offset(
        &lines,
        current_body_line_row(app),
        lyrics_area.height,
        lyrics_area.width,
    );
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .scroll((offset, 0)),
        lyrics_area,
    );
}

pub(super) fn render_fullscreen(frame: &mut Frame, app: &AppState, images: &mut ImageState) {
    let area = frame.area();
    match (&app.track_artist, &app.track_title) {
        (Some(artist), Some(title)) => {
            render_fullscreen_hero(frame, app, images, artist, title, area)
        }
        _ => render_fullscreen_idle(frame, app, area),
    }
}

/// Same reasoning as `render_now_playing_idle`: nothing loaded, nothing
/// to depict art for.
pub(super) fn render_fullscreen_idle(frame: &mut Frame, app: &AppState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(header(app, area.width as usize))
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(body_lines(app))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        chunks[1],
    );
}

pub(super) fn bold_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|l| {
            Line::from(
                l.spans
                    .into_iter()
                    .map(|s| Span::styled(s.content, s.style.add_modifier(Modifier::BOLD)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// Fullscreen: art and song info on one half, the whole lyric sheet on the other,
/// no divider. `render_fullscreen_hero_stacked` is the fallback for terminals too
/// narrow to split.
pub(super) fn render_fullscreen_hero(
    frame: &mut Frame,
    app: &AppState,
    images: &mut ImageState,
    artist: &str,
    title: &str,
    area: Rect,
) {
    // A true even split, no divider column between them -- art+info and
    // lyrics each get half, not art squeezed into a narrow fixed sidebar
    // with the rest handed to lyrics by default.
    let can_split = area.width >= 60 && area.height >= 10;
    if !can_split {
        render_fullscreen_hero_stacked(frame, app, images, artist, title, area);
        return;
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Sized from the column width first, height derived from it, so the card fills
    // the column instead of leaving gutters. Height is the lesser of two numbers:
    // `ideal_height` (how tall a square of this width is, at the 2:1 cell ratio) and
    // `height_budget` (75% of the vertical room left after the 8 fixed rows below,
    // the rest being slack for the two `Min(1)` spacers). A flat cap cannot work: it
    // is right at only one terminal height, too big on short ones and too small on
    // tall ones.
    let art_width_candidate = cols[0].width.saturating_sub(8).clamp(20, 70);
    let cell_size = real_cell_size(images.picker.as_ref());
    let ideal_height = square_height_cells(art_width_candidate, cell_size);
    let max_safe_height = area.height.saturating_sub(8);
    let height_budget = ((max_safe_height as f32) * 0.75) as u16;
    // Floor of 10 rows only when the terminal has that much room. An unconditional
    // `.max(10)` pushed `art_height + 8` past `area.height` on a short terminal and
    // ratatui dropped the fixed title/transport rows to zero height.
    let floor = 10.min(max_safe_height);
    let effective_ceiling = height_budget.max(floor);
    // When height is the limiting dimension, width is re-derived from it with
    // `square_width_cells` so the card stays a true square. Clamping only the height
    // produced a card shorter than square but never narrower.
    let (art_width, art_height) = if ideal_height <= effective_ceiling {
        (art_width_candidate, ideal_height.max(1))
    } else {
        let constrained_height = effective_ceiling.max(1);
        let constrained_width = square_width_cells(constrained_height, cell_size)
            .min(art_width_candidate)
            .max(1);
        (constrained_width, constrained_height)
    };
    // Symmetric `Min(1)` spacers centre the block in the pane.
    let side_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1), // top spacer
            Constraint::Length(art_height),
            Constraint::Length(1), // spacer
            Constraint::Length(1), // title
            Constraint::Length(1), // artist -- album
            Constraint::Length(1), // spacer
            Constraint::Length(1), // icon + time + vol
            Constraint::Length(3), // gauge (bordered -- needs its own top/bottom rows)
            Constraint::Min(1),    // bottom spacer
        ])
        .split(cols[0]);

    let album = app.track_album.as_deref().unwrap_or(title);
    render_art(
        frame,
        app,
        images,
        artist,
        album,
        capsule_row(side_rows[1], art_width),
    );

    frame.render_widget(
        Paragraph::new(truncate_ellipsis(title, cols[0].width as usize))
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::BOLD)),
        side_rows[3],
    );
    let artist_album = match &app.track_album {
        Some(al) => format!("{artist} \u{2014} {al}"),
        None => artist.to_string(),
    };
    frame.render_widget(
        Paragraph::new(truncate_ellipsis(&artist_album, cols[0].width as usize))
            .alignment(Alignment::Center)
            .style(Style::default().fg(DIM)),
        side_rows[4],
    );
    if let Some(label) = &app.context_label {
        frame.render_widget(
            Paragraph::new(truncate_ellipsis(
                &format!("Playing from {label}"),
                cols[0].width as usize,
            ))
            .alignment(Alignment::Center)
            .style(Style::default().fg(DIM)),
            side_rows[5],
        );
    }
    frame.render_widget(
        Paragraph::new(format!(
            "{} {}   {}",
            playing_icon(app),
            time_readout(app),
            volume_readout(app)
        ))
        .alignment(Alignment::Center),
        side_rows[6],
    );
    // The capsule is the same width as the art card, from the same `capsule_row`
    // call, so their left and right edges match by construction.
    frame.render_widget(
        progress_gauge_bordered(app),
        capsule_row(side_rows[7], art_width),
    );

    // No divider rule between the two halves -- asked for directly,
    // relying on the whitespace gap alone (lyrics get a left inset
    // below) to separate them rather than a drawn line.
    let lyrics_area = Rect {
        x: cols[1].x + 2,
        width: cols[1].width.saturating_sub(2),
        ..cols[1]
    };
    render_fullscreen_lyrics(frame, app, lyrics_area, Alignment::Left);
}

/// Splits off the last row of a lyrics pane for the credit line. Fewer than
/// three rows and the credit gives way instead: two rows of lyrics is the
/// least worth keeping.
pub(super) fn credit_split(area: Rect, has_credit: bool) -> (Rect, Option<Rect>) {
    if !has_credit || area.height < 3 {
        return (area, None);
    }
    let lyrics = Rect {
        height: area.height - 1,
        ..area
    };
    let credit = Rect {
        y: area.y + area.height - 1,
        height: 1,
        ..area
    };
    (lyrics, Some(credit))
}

/// Draws the dim "where these lyrics came from" line (when there is one)
/// and returns the area left for the lyrics themselves.
pub(super) fn render_lyrics_credit(
    frame: &mut Frame,
    app: &AppState,
    area: Rect,
    alignment: Alignment,
) -> Rect {
    let (lyrics, credit_area) = credit_split(area, app.lyrics_credit.is_some());
    if let (Some(credit_area), Some(credit)) = (credit_area, app.lyrics_credit.as_deref()) {
        let text = truncate_ellipsis(credit, credit_area.width as usize);
        frame.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(DIM))
                .alignment(alignment),
            credit_area,
        );
    }
    lyrics
}

// Lyrics are not enlarged. Block-glyph big text (`tui-big-text`, at several
// sizes) and letter-spacing were each tried and rejected: too large, uneven, or
// blocky rather than smooth. Bold text with a four-tier colour fade
// (`bold_lines`, `lyric_tier_color`), centred by `center_current_line` /
// `top_anchored_offset`, renders in the terminal's own font. Truly larger smooth
// text would need rasterising lyrics to an image and showing it through a
// graphics protocol, a separate piece of work.
pub(super) fn render_fullscreen_lyrics(
    frame: &mut Frame,
    app: &AppState,
    area: Rect,
    alignment: Alignment,
) {
    let area = render_lyrics_credit(frame, app, area, alignment);
    let (lines, offset) = center_current_line(
        bold_lines(body_lines(app)),
        current_body_line_row(app),
        area.height,
        area.width,
    );
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(alignment)
            .wrap(Wrap { trim: true })
            .scroll((offset, 0)),
        area,
    );
}

/// Narrow-terminal fallback for fullscreen: a header row (art beside
/// title, artist, transport and gauge, as in the compact hero) with the lyric
/// sheet spanning the full width below. Used when a split would be too narrow to
/// read either half.
pub(super) fn render_fullscreen_hero_stacked(
    frame: &mut Frame,
    app: &AppState,
    images: &mut ImageState,
    artist: &str,
    title: &str,
    area: Rect,
) {
    let art_width = (area.width / 4).clamp(ART_MIN_WIDTH, ART_MAX_WIDTH);
    let art_height = square_height_cells(art_width, real_cell_size(images.picker.as_ref()));
    let show_art = area.width >= art_width + 24 && area.height >= art_height + 4;

    // At least 9 rows: `meta_chunks` below needs that many, and a shorter header
    // would silently shrink its content.
    let header_height = (art_height + 2)
        .max(9)
        .min(area.height.saturating_sub(2).max(1));
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(area);

    let header_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if show_art {
            vec![Constraint::Length(art_width), Constraint::Min(1)]
        } else {
            vec![Constraint::Min(1)]
        })
        .split(outer[0]);

    let album = app.track_album.as_deref().unwrap_or(title);
    let mut art_top_row: Option<u16> = None;
    let meta_area = if show_art {
        // Fixed `Length(1)` top margin, matching `meta_chunks[0]`, as in
        // `render_now_playing_hero`. Two `Min(1)` spacers do not reliably split the
        // slack evenly, and the title would float above the art card.
        let art_area = Layout::default()
            .constraints([
                Constraint::Length(1),
                Constraint::Length(art_height),
                Constraint::Min(1),
            ])
            .split(header_cols[0])[1];
        art_top_row = Some(art_area.y);
        render_art(frame, app, images, artist, album, art_area);
        header_cols[1]
    } else {
        header_cols[0]
    };

    // Title one row below the border line; see `render_now_playing_hero`.
    let meta_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // spacer (one extra row -- see above)
            Constraint::Length(1), // title
            Constraint::Length(1), // artist -- album
            Constraint::Length(1), // spacer / context label
            Constraint::Length(1), // icon + time + vol
            Constraint::Length(3), // gauge (bordered)
        ])
        .split(meta_area);

    // Logs once per change in value, as in `render_now_playing_hero`.
    if let Some(art_row) = art_top_row {
        let title_row = meta_chunks[1].y;
        thread_local! {
            static LAST_LOGGED: std::cell::Cell<Option<(u16, u16)>> = const { std::cell::Cell::new(None) };
        }
        let pair = (art_row, title_row);
        let changed = LAST_LOGGED.with(|c| c.replace(Some(pair)) != Some(pair));
        if changed {
            log::info!(
                "fullscreen-stacked hero alignment: art_top_row={art_row} title_row={title_row} (one_below_border={})",
                title_row == art_row + 1
            );
        }
    }

    frame.render_widget(
        Paragraph::new(truncate_ellipsis(title, meta_area.width as usize))
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::BOLD)),
        meta_chunks[1],
    );
    let artist_album = match &app.track_album {
        Some(al) => format!("{artist} \u{2014} {al}"),
        None => artist.to_string(),
    };
    frame.render_widget(
        Paragraph::new(truncate_ellipsis(&artist_album, meta_area.width as usize))
            .alignment(Alignment::Center)
            .style(Style::default().fg(DIM)),
        meta_chunks[2],
    );
    if let Some(label) = &app.context_label {
        frame.render_widget(
            Paragraph::new(truncate_ellipsis(
                &format!("Playing from {label}"),
                meta_area.width as usize,
            ))
            .alignment(Alignment::Center)
            .style(Style::default().fg(DIM)),
            meta_chunks[3],
        );
    }
    frame.render_widget(
        Paragraph::new(format!(
            "{} {}   {}",
            playing_icon(app),
            time_readout(app),
            volume_readout(app)
        ))
        .alignment(Alignment::Center),
        meta_chunks[4],
    );
    frame.render_widget(progress_gauge_bordered(app), meta_chunks[5]);

    frame.render_widget(Block::default().borders(Borders::TOP), outer[1]);
    render_fullscreen_lyrics(frame, app, outer[2], Alignment::Center);
}

#[cfg(test)]
mod credit_split_tests {
    use super::*;

    fn area(height: u16) -> Rect {
        Rect {
            x: 4,
            y: 10,
            width: 60,
            height,
        }
    }

    #[test]
    fn no_credit_leaves_the_lyrics_area_untouched() {
        assert_eq!(credit_split(area(20), false), (area(20), None));
    }

    #[test]
    fn a_credit_takes_exactly_the_last_row() {
        let (lyrics, credit) = credit_split(area(20), true);
        assert_eq!(
            lyrics,
            Rect {
                height: 19,
                ..area(20)
            }
        );
        assert_eq!(
            credit,
            Some(Rect {
                x: 4,
                y: 29,
                width: 60,
                height: 1
            })
        );
    }

    #[test]
    fn the_two_never_overlap_and_together_fill_the_area() {
        let (lyrics, credit) = credit_split(area(12), true);
        let credit = credit.unwrap();
        assert_eq!(lyrics.y + lyrics.height, credit.y);
        assert_eq!(credit.y + credit.height, area(12).y + area(12).height);
    }

    #[test]
    fn a_pane_too_short_to_spare_a_row_shows_lyrics_only() {
        // Two rows of lyrics is the least worth keeping; below that the
        // credit gives way rather than squeezing the lyrics out.
        assert_eq!(credit_split(area(2), true), (area(2), None));
        assert_eq!(credit_split(area(0), true), (area(0), None));
        assert!(credit_split(area(3), true).1.is_some());
    }
}
