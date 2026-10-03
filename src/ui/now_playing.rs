use super::*;

pub(super) fn render_compact(frame: &mut Frame, app: &AppState, images: &mut ImageState, area: Rect) {
    match (&app.track_artist, &app.track_title) {
        (Some(artist), Some(title)) => render_now_playing_hero(frame, app, images, artist, title, area),
        _ => render_now_playing_idle(frame, app, area),
    }
}

/// The unglamorous state, designed on its own terms rather than as a
/// stripped-down hero: nothing is loaded yet, so there's nothing to
/// depict art for -- showing a colorful block anyway would be a lie
/// about there being a track, not a placeholder for one.
pub(super) fn render_now_playing_idle(frame: &mut Frame, app: &AppState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(header(app, area.width as usize)).style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    frame.render_widget(Paragraph::new(body_lines(app)).wrap(Wrap { trim: true }), chunks[2]);
}

/// Art block + larger title/transport on one row, lyrics given real room
/// below -- the hero treatment validated in the browser mockup, ported
/// into the real terminal for the first time. `hero_height` scales with
/// the pane but stays capped: this is a glance screen, not the whole
/// app, and lyrics still need to be the dominant use of vertical space
/// (calibrating density to what this screen is actually for, not
/// maximizing decoration).
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
    // A real pixel square, not a flat "half as tall as wide" guess --
    // see `square_height_cells`'s own doc comment for why that flat
    // assumption produced a card reported live as visibly too tall.
    let art_height = square_height_cells(art_width, real_cell_size(images.picker.as_ref()));

    // +2 over the original 6-11 clamp: the gauge row grew from
    // `Length(1)` to `Length(3)` below (a border, requested live to
    // match the fullscreen views' already-bordered gauge) and needs the
    // 2 extra rows of slack. `.max(art_height + 2)`, not just
    // `.max(art_height)`: the art column below splits into
    // `[Min(1), Length(art_height), Min(1)]` to center the card --
    // a real, log-confirmed bug (not assumed) was `hero_height` only
    // ever guaranteeing *exactly* `art_height`, leaving zero room for
    // those two spacers; ratatui's layout solver then shrank the
    // `Length(art_height)` allocation by 1 to make room for them,
    // silently handing `render_art` a card one row short of the square
    // it asked for (confirmed directly from the `"art size"` diagnostic
    // log: computed `art_height` was 12, actually-rendered `area.height`
    // was 11). `+2` reserves the spacers' own minimum up front instead.
    // Clamp floor raised 8 -> 9: `meta_chunks` below now needs 9 rows
    // minimum (its leading spacer grew to `Length(2)`, see that comment),
    // and a `hero_height` that's too short for its own content would
    // reproduce the exact same silent-shrink failure mode named above,
    // just against `meta_chunks` instead of the art column this time.
    let hero_height = (area.height / 2).clamp(9, 13).max(art_height + 2).min(area.height);
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(hero_height), Constraint::Length(1), Constraint::Min(1)])
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
        // The art column reserves `hero_height` rows (matching the text
        // column beside it), but the card itself only ever needs
        // `art_height` of them to stay square -- rendering into the
        // whole column would hand `render_art`/`Resize::Scale` a taller-
        // than-square target, and while `Scale` still preserves the
        // image's own aspect (it won't distort), it does leave a blank
        // gap on one side to do it, right back to the shape of bug
        // `Resize::Scale` was originally introduced to fix.
        //
        // Top margin is a fixed `Length(1)`, not `Min(1)` on both ends --
        // a symmetric top+bottom `Min(1)` split centers the card within
        // `hero_height`, but `meta_chunks` below starts its own content
        // (the title) after a *fixed* one-row spacer regardless of
        // `hero_height`'s leftover slack. Whenever the two didn't agree
        // (any time `hero_height` exceeded `art_height + 2`, which is
        // the common case once the 8-13 row clamp binds), the card's
        // computed centering offset and the title's fixed offset drifted
        // apart -- reported live as "the song name still not aligned
        // with top of the frame". Matching this column's own top margin
        // to the text column's fixed spacer keeps both starting at the
        // exact same row, by construction, regardless of `hero_height`.
        let art_area = Layout::default()
            .constraints([Constraint::Length(1), Constraint::Length(art_height), Constraint::Min(1)])
            .split(hero_cols[0])[1];
        art_top_row = Some(art_area.y);
        render_art(frame, app, images, artist, album, art_area);
        hero_cols[1]
    } else {
        hero_cols[0]
    };

    // Round 9 put the title on the *same* row as the art border's own top
    // edge (`Length(1)` spacer, matching the art column's own), confirmed
    // live via the diagnostic below to actually land on the identical row
    // -- and still reported as visibly misaligned. Root cause, reasoned
    // out rather than guessed further: Unicode box-drawing corner
    // characters (the border's `┌`) render their ink starting from the
    // *vertical center* of their cell, not the top -- that's what makes
    // stacked box-drawing rows connect seamlessly. Regular text glyphs
    // sit near the top of their cell. So even on the *identical* buffer
    // row, the border's visible line sits at that row's middle while the
    // title's visible glyph-top sits near that row's top -- the title
    // reads as floating above the border line no matter what row it's
    // on, because the two kinds of glyph don't align to the same point
    // within a shared cell. Asked directly which side of that gap is
    // preferred, since eliminating it entirely isn't reachable at
    // integer-row granularity (the border's true visual position sits
    // *between* two text rows, not on either one): the leading spacer
    // grew from `Length(1)` to `Length(2)`, moving the title one row
    // *below* the border line -- lining up with where the art's actual
    // pixel content starts (`inner.y`, one row below the border) instead
    // of the border line's own row.
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
            Paragraph::new(truncate_ellipsis(&format!("Playing from {label}"), meta_area.width as usize))
                .style(Style::default().fg(DIM)),
            meta_chunks[3],
        );
    }
    frame.render_widget(
        Paragraph::new(format!("{} {}   {}", playing_icon(app), time_readout(app), volume_readout(app))),
        meta_chunks[4],
    );
    let gauge_area = Rect { width: meta_chunks[5].width.min(COMPACT_GAUGE_MAX_WIDTH), ..meta_chunks[5] };
    frame.render_widget(progress_gauge_bordered(app), gauge_area);

    frame.render_widget(Block::default().borders(Borders::TOP), outer[1]);
    let lyrics_area = render_lyrics_credit(frame, app, outer[2], Alignment::Left);
    let lines = body_lines(app);
    let offset = top_anchored_offset(&lines, current_body_line_row(app), lyrics_area.height, lyrics_area.width);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).scroll((offset, 0)), lyrics_area);
}

pub(super) fn render_fullscreen(frame: &mut Frame, app: &AppState, images: &mut ImageState) {
    let area = frame.area();
    match (&app.track_artist, &app.track_title) {
        (Some(artist), Some(title)) => render_fullscreen_hero(frame, app, images, artist, title, area),
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
        Paragraph::new(body_lines(app)).alignment(Alignment::Center).wrap(Wrap { trim: true }),
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

/// The most immersive treatment: art + song info on one half, the full
/// lyric sheet on the other, no divider between them -- replacing the
/// previous single stacked column (which `render_fullscreen_hero_stacked`
/// below still covers, as the fallback for a terminal too narrow to split).
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

    // Sized off the column's own width first, height derived from that --
    // the previous version did the opposite (`art_width = art_height*2`,
    // with `art_height` itself just half the pane's height), which on a
    // wide fullscreen column left a large, unfilled gutter on both sides
    // of the art card no matter how tall the terminal was -- reported
    // live as "the left side... looks empty." Deriving width from the
    // column directly fills far more of it; height still follows width
    // at the same 2:1 ratio the stacked fallback uses (a terminal cell
    // is roughly twice as tall as it is wide, so this is what reads as
    // a visually square card).
    //
    // Two flat-cap attempts both failed for the same underlying reason:
    // a fixed cell ceiling only looks right at one specific terminal
    // height. 70 cols / 35 rows was tuned for a solid-color placeholder
    // and, once Phase 11 started rendering a real photo there, consumed
    // nearly the entire column on a normal terminal, squeezing the
    // fixed content below to nothing ("still not completely right").
    // 50 cols / 20 rows fixed that squeeze but then read as too small
    // ("shrunk") on a taller terminal, where 20 rows is a shrinking
    // fraction of the available height the taller the terminal gets --
    // a flat cap can't scale with the pane, by definition.
    //
    // Fixed properly this time: height is the *lesser* of two numbers
    // that each answer a different question, instead of one flat
    // ceiling trying to answer both. `ideal_height` keeps the card
    // visually square against whatever width the column produced ("how
    // tall should a square card of this width be"). `height_budget` is
    // 75% of the vertical room actually left after the 8 fixed rows
    // below the card ("how tall can the card get before the fixed
    // content below it, and both breathing-room spacers, get squeezed
    // out") -- the other 25% covers those two `Min(1)` spacers plus
    // slack. Taking the smaller of the two means: on a short terminal,
    // `height_budget` is the binding constraint and the card shrinks to
    // fit safely (this round's original goal); on a tall terminal,
    // `ideal_height` is the binding constraint and the card simply stays
    // a natural, width-matched square instead of an arbitrarily tiny
    // fixed size ("shrunk" complaint, now fixed by not having a flat
    // ceiling at all).
    let art_width_candidate = cols[0].width.saturating_sub(8).clamp(20, 70);
    let cell_size = real_cell_size(images.picker.as_ref());
    let ideal_height = square_height_cells(art_width_candidate, cell_size);
    let max_safe_height = area.height.saturating_sub(8);
    let height_budget = ((max_safe_height as f32) * 0.75) as u16;
    // A real bug from an unconditional `.max(10)` here, caught live: on
    // a short enough terminal, `height_budget` (already `<= max_safe_height`
    // by construction, since it's 75% of it) could fall under 10, but
    // the old floor forced `art_height` back up to 10 regardless --
    // pushing `art_height + 8` past `area.height` and starving the
    // fixed title/artist/transport rows below it of any space at all
    // (ratatui's layout solver dropped them to zero height under the
    // resulting pressure, so they silently disappeared rather than just
    // looking cramped). The floor now aims for 10 only when the terminal
    // actually has that much room to give.
    let floor = 10.min(max_safe_height);
    let effective_ceiling = height_budget.max(floor);
    // The real bug the diagnostic log confirmed: previously `art_height`
    // alone was clamped down to `height_budget` whenever the terminal
    // didn't have room for a true square at `art_width_candidate`, but
    // `art_width` never shrank to match -- producing a card that was
    // *shorter* than square without ever becoming *narrower* to match,
    // i.e. not a square at all (logged live: 70x30 cells at an 18x40px
    // cell came out 1224x1120 real pixels, 9% wider than tall). Fixed by
    // re-deriving width from the constrained height with
    // `square_width_cells` (already built for Phase 11's own narrow-
    // stacked fallback) whenever height ends up being the limiting
    // dimension, so the card is a true square either way -- "adjust one
    // or the other but get them to fit flush," per the live report --
    // instead of only ever adjusting height and leaving the mismatch.
    let (art_width, art_height) = if ideal_height <= effective_ceiling {
        (art_width_candidate, ideal_height.max(1))
    } else {
        let constrained_height = effective_ceiling.max(1);
        let constrained_width = square_width_cells(constrained_height, cell_size).min(art_width_candidate).max(1);
        (constrained_width, constrained_height)
    };
    // Symmetric `Min(1)` on both ends -- true centering. This looked
    // wrong once before only because the fixed content above it (art +
    // 6 text rows) was too small a block to center inside a tall pane
    // without the leftover space reading as excessive; with the art
    // itself now scaling to the pane, the same centering reads as
    // balanced instead of top- or bottom-heavy.
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
            Constraint::Min(1), // bottom spacer
        ])
        .split(cols[0]);

    let album = app.track_album.as_deref().unwrap_or(title);
    render_art(frame, app, images, artist, album, capsule_row(side_rows[1], art_width));

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
            Paragraph::new(truncate_ellipsis(&format!("Playing from {label}"), cols[0].width as usize))
                .alignment(Alignment::Center)
                .style(Style::default().fg(DIM)),
            side_rows[5],
        );
    }
    frame.render_widget(
        Paragraph::new(format!("{} {}   {}", playing_icon(app), time_readout(app), volume_readout(app)))
            .alignment(Alignment::Center),
        side_rows[6],
    );
    // A contained capsule the same width as the art card above it, not a
    // bar stretching to the full column -- same `capsule_row` call
    // applied to a different row of the same parent guarantees
    // byte-identical left/right edges with the art card by construction,
    // which is the actual fix (previously computed independently, which
    // is how the two drifted apart).
    frame.render_widget(progress_gauge_bordered(app), capsule_row(side_rows[7], art_width));

    // No divider rule between the two halves -- asked for directly,
    // relying on the whitespace gap alone (lyrics get a left inset
    // below) to separate them rather than a drawn line.
    let lyrics_area = Rect { x: cols[1].x + 2, width: cols[1].width.saturating_sub(2), ..cols[1] };
    render_fullscreen_lyrics(frame, app, lyrics_area, Alignment::Left);
}

/// Splits off the last row of a lyrics pane for the credit line. Fewer than
/// three rows and the credit gives way instead: two rows of lyrics is the
/// least worth keeping.
pub(super) fn credit_split(area: Rect, has_credit: bool) -> (Rect, Option<Rect>) {
    if !has_credit || area.height < 3 {
        return (area, None);
    }
    let lyrics = Rect { height: area.height - 1, ..area };
    let credit = Rect { y: area.y + area.height - 1, height: 1, ..area };
    (lyrics, Some(credit))
}

/// Draws the dim "where these lyrics came from" line (when there is one)
/// and returns the area left for the lyrics themselves.
pub(super) fn render_lyrics_credit(frame: &mut Frame, app: &AppState, area: Rect, alignment: Alignment) -> Rect {
    let (lyrics, credit_area) = credit_split(area, app.lyrics_credit.is_some());
    if let (Some(credit_area), Some(credit)) = (credit_area, app.lyrics_credit.as_deref()) {
        let text = truncate_ellipsis(credit, credit_area.width as usize);
        frame.render_widget(Paragraph::new(text).style(Style::default().fg(DIM)).alignment(alignment), credit_area);
    }
    lyrics
}

// Six attempts at "bigger" here, in order -- see git history for each
// one's full detail. First: `tui-big-text` enlarged only the current
// line, jarringly inconsistent next to its normal-size neighbors.
// Second and third: `tui-big-text` uniformly at `PixelSize::Quadrant`,
// reported "way too large" twice, with a song's opening line pinned to
// the top instead of centered. Fourth: letter-spacing instead of block
// glyphs -- "irregularly big spacing," then still "way too large" and
// "unnatural" even after fixing the spacing ratio. Fifth and sixth:
// block glyphs reopened at the user's explicit request, first at
// `PixelSize::Sextant` (confirmed to render with correct, non-garbled
// glyphs -- the font-coverage risk was real but didn't materialize),
// then `PixelSize::Octant` for even smaller -- both still reported "way
// too big," and, decisively this round, rejected on a different axis
// entirely: "not pixelized... more curved," an explicit preference for
// how the text looks, not just how big it is. Block-glyph rendering
// (font8x8-backed, inherently blocky at any `PixelSize`) cannot satisfy
// that -- it's not a parameter to tune, it's the technique itself. Six
// attempts across two families (glyph-scaling, letter-spacing) both
// eventually rejected is well past the systematic-debugging "question
// the architecture" threshold a second and third time over: `tui-big-
// text`/`font8x8` removed from the project outright (`cargo remove`,
// confirmed `Cargo.toml`/`Cargo.lock` clean via `git diff`), the same
// full removal already proven twice this session for letter-spacing and
// the original block-glyph code. What's left -- `bold_lines` +
// `lyric_tier_color`'s 4-tier fade + `center_current_line`/
// `top_anchored_offset` -- renders with the terminal's own font, which
// is exactly what "curved" means in a terminal context: normal
// anti-aliased glyphs, not a bitmap approximation. Literally bigger
// *and* curved at the same time isn't achievable through text alone in
// a fixed-size cell grid -- that would need rendering text to a real
// raster image via an actual font and displaying it through a terminal
// graphics protocol (kitty/iTerm2/sixel), the same category of
// investment Phase 11 already tracks for album art specifically, not
// attempted here without discussing that scope and cost first.
pub(super) fn render_fullscreen_lyrics(frame: &mut Frame, app: &AppState, area: Rect, alignment: Alignment) {
    let area = render_lyrics_credit(frame, app, area, alignment);
    let (lines, offset) =
        center_current_line(bold_lines(body_lines(app)), current_body_line_row(app), area.height, area.width);
    frame.render_widget(
        Paragraph::new(lines).alignment(alignment).wrap(Wrap { trim: true }).scroll((offset, 0)),
        area,
    );
}

/// Narrow-terminal fallback: the original single stacked column (art on
/// top, then title/transport/lyrics, all centered) -- kept rather than
/// deleted since a split too narrow to read either half legibly is
/// worse than not splitting at all.
/// Inline (art beside title/artist/transport/gauge), mirroring the
/// compact hero's own established shape -- reported live as wanted here
/// too ("the now playing format is not inline... song name is above
/// the album cover"). Previously stacked (art on top, text below,
/// lyrics under that); keeps the same two regions (an art+meta header,
/// then lyrics spanning the full width below it) but makes the header
/// row inline instead of vertically stacked, matching
/// `render_now_playing_hero`'s structure -- centered instead of
/// left-aligned, and using `render_fullscreen_lyrics` (this app's
/// bold+color-fade fullscreen lyrics treatment) rather than the compact
/// hero's small top-anchored one, since this is still a fullscreen view.
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

    // `.max(9)`, not `8`: `meta_chunks` below needs 9 rows minimum now
    // (its leading spacer grew to `Length(2)`, matching the compact
    // hero's own identical fix) -- a `header_height` too short for that
    // would silently shrink `meta_chunks`' own content instead.
    let header_height = (art_height + 2).max(9).min(area.height.saturating_sub(2).max(1));
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(header_height), Constraint::Length(1), Constraint::Min(1)])
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
        // Same bug, same fix, as `render_now_playing_hero`'s own art
        // column (see its doc comment for the full account): a top
        // `Min(1)` competes with the bottom `Min(1)` for whatever slack
        // `header_height` has beyond `art_height`, and ratatui's surplus
        // distribution between two `Min` constraints doesn't reliably
        // split it 1-and-1 the way a hand-check might assume -- while
        // `meta_chunks` below starts the title after a *fixed* `Length(1)`
        // spacer regardless. This function was missed when that fix
        // shipped for the compact hero (this is a *different* function,
        // not a leftover branch of the same one), so the exact same
        // "song name floats above the art card" report kept reproducing
        // here even after the compact view was confirmed fixed. Fixed
        // identically: a fixed `Length(1)` top margin, matching this
        // column's own `meta_chunks[0]` spacer exactly, by construction.
        let art_area = Layout::default()
            .constraints([Constraint::Length(1), Constraint::Length(art_height), Constraint::Min(1)])
            .split(header_cols[0])[1];
        art_top_row = Some(art_area.y);
        render_art(frame, app, images, artist, album, art_area);
        header_cols[1]
    } else {
        header_cols[0]
    };

    // Same shift as `render_now_playing_hero`'s own `meta_chunks` -- see
    // its doc comment for the full reasoning (box-drawing corner glyphs
    // render centered in their cell, regular text glyphs render near the
    // top, so the two can never visually align on one shared row; asked
    // directly, the title moves one row below the border line instead).
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

    // Same diagnostic as `render_now_playing_hero`, extended here after
    // finding this function had the same top-spacer bug that function's
    // own fix never reached (see the art_area comment above) -- logs once
    // per distinct value change so a live run can confirm this call site
    // too, not just the compact one.
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
            Paragraph::new(truncate_ellipsis(&format!("Playing from {label}"), meta_area.width as usize))
                .alignment(Alignment::Center)
                .style(Style::default().fg(DIM)),
            meta_chunks[3],
        );
    }
    frame.render_widget(
        Paragraph::new(format!("{} {}   {}", playing_icon(app), time_readout(app), volume_readout(app)))
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
        Rect { x: 4, y: 10, width: 60, height }
    }

    #[test]
    fn no_credit_leaves_the_lyrics_area_untouched() {
        assert_eq!(credit_split(area(20), false), (area(20), None));
    }

    #[test]
    fn a_credit_takes_exactly_the_last_row() {
        let (lyrics, credit) = credit_split(area(20), true);
        assert_eq!(lyrics, Rect { height: 19, ..area(20) });
        assert_eq!(credit, Some(Rect { x: 4, y: 29, width: 60, height: 1 }));
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

