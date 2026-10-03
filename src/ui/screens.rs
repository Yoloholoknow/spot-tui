use super::*;

pub(super) fn render_library_home(frame: &mut Frame, app: &AppState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new("Library").style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    let items: Vec<ListItem> = LIBRARY_ENTRIES
        .iter()
        .enumerate()
        .map(|(i, (label, _))| {
            if i == app.library.home_selected {
                ListItem::new(*label).style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            } else {
                ListItem::new(*label)
            }
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.library.home_selected));
    frame.render_stateful_widget(List::new(items), chunks[1], &mut state);
}

/// Not the shared `render_list_screen`, for the same reason Your
/// Playlists isn't: pinned tracks (spot-tui's own local-only substitute,
/// separate from pinned playlists -- see `pins.rs`) bubble to the top and
/// get a marker glyph, a concept the generic 4-screen renderer doesn't
/// know about. Reuses `pinned_first`/`filtered_sorted` exactly as Your
/// Playlists does, just keyed on `app.pinned_tracks` instead.
pub(super) fn render_playlist_detail(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let Some(pd) = &app.playlist_detail else {
        frame.render_widget(Paragraph::new("no playlist selected"), area);
        return;
    };
    let label = |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    if pd.move_mode.is_some() {
        // Key hints moved to the status bar (`render_status`'s own
        // move-mode takeover) -- this row is `Constraint::Length(1)` with
        // no wrap, so the hint text was already silently truncated on a
        // narrow terminal; three distinct weights (name, badge, nothing
        // else) read more clearly than one undifferentiated ACCENT+BOLD
        // line ever did.
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(pd.playlist.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled("MOVE MODE", Style::default().fg(WARN).add_modifier(Modifier::BOLD)),
            ])),
            chunks[0],
        );
    } else {
        frame.render_widget(
            Paragraph::new(filter_header(&pd.playlist.name, &pd.filter))
                .style(Style::default().add_modifier(Modifier::BOLD)),
            chunks[0],
        );
    }
    match &pd.tracks {
        Fetch::NotStarted | Fetch::Loading => {
            render_loading(frame, chunks[1]);
        }
        Fetch::Failed(e) => {
            render_fetch_error(frame, chunks[1], e);
        }
        Fetch::Ready(items) => {
            // Move-mode intentionally does NOT bubble pinned tracks to the
            // top here, even though every other rendering of this list
            // does -- pinned_first is what breaks the display-position ==
            // real-array-position identity move-mode depends on. Skipping
            // it during the move keeps that identity exact regardless of
            // what's pinned, rather than blocking reorder whenever
            // anything in the playlist happens to be pinned.
            let natural = filtered_sorted(items, &pd.filter, &label);
            if pd.move_mode.is_some() {
                // The moving row gets a WARN `\u{2192}` marker independent
                // of selection (the second caller of `render_display_list_lines`,
                // after Devices) -- previously this row was visually
                // identical to any other selected row. `pd.selected` is a
                // real index into `natural` here (move mode's whole point
                // is keeping display position == real array position), so
                // the moving track's URI is looked up once, not per-row.
                let moving_uri = natural.get(pd.selected).map(|(_, t)| t.uri.as_str());
                let move_line = |t: &TrackResult| {
                    let marker = if Some(t.uri.as_str()) == moving_uri { "\u{2192} " } else { "  " };
                    Line::from(vec![Span::styled(marker, Style::default().fg(WARN)), Span::raw(label(t))])
                };
                render_display_list_lines(
                    frame,
                    chunks[1],
                    &natural,
                    pd.selected,
                    &move_line,
                    pd.filter.query.is_empty(),
                    list_state,
                );
            } else {
                let display = pinned_first(natural, &app.pinned_tracks, |t| t.uri.as_str());
                let pin_label = |t: &TrackResult| {
                    let marker = if app.pinned_tracks.contains(&t.uri) { "* " } else { "  " };
                    format!("{marker}{}", label(t))
                };
                render_display_list(
                    frame,
                    chunks[1],
                    &display,
                    pd.selected,
                    &pin_label,
                    pd.filter.query.is_empty(),
                    list_state,
                );
            }
        }
    }
}

/// Your Playlists' own renderer, not the shared `render_list_screen`:
/// pinned playlists (spot-tui's own local-only substitute for Spotify's
/// pinning, which the public Web API doesn't expose at all) bubble to
/// the top and get a marker glyph -- a concept none of the other 4 list
/// screens have.
pub(super) fn render_your_playlists(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let label =
        |p: &crate::api::library::PlaylistSummary| format!("{} ({} tracks)", p.name, p.track_count);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(filter_header("Your Playlists", &app.library.playlists_filter))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    match &app.library.playlists {
        Fetch::NotStarted | Fetch::Loading => {
            render_loading(frame, chunks[1]);
        }
        Fetch::Failed(e) => {
            render_fetch_error(frame, chunks[1], e);
        }
        Fetch::Ready(items) => {
            let display = pinned_first(
                filtered_sorted(items, &app.library.playlists_filter, &label),
                &app.pinned_playlists,
                |p| p.uri.as_str(),
            );
            let pin_label = |p: &crate::api::library::PlaylistSummary| {
                let marker = if app.pinned_playlists.contains(&p.uri) { "* " } else { "  " };
                format!("{marker}{}", label(p))
            };
            render_display_list(
                frame,
                chunks[1],
                &display,
                app.library.playlists_selected,
                &pin_label,
                app.library.playlists_filter.query.is_empty(),
                list_state,
            );
        }
    }
}

/// The Connect queue (Phase 7): a static "currently playing" caption
/// above a plain list of what's up next. No filter/sort/pin concept --
/// unlike every other list screen, this one has no local mutation
/// surface at all (see `api::queue`'s own doc comment for why: the
/// public Web API has no remove/reorder endpoint for it), so it's the
/// one list screen that's genuinely just a view.
pub(super) fn render_queue(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let chunks =
        Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1), Constraint::Min(1)]).split(area);
    frame.render_widget(Paragraph::new(screen_header_line("Queue", Some("auto-refreshing"))), chunks[0]);
    match &app.queue.fetch {
        // Full-height body, not squeezed into a 1-row caption slot --
        // `render_fetch_error`'s bordered box needs real rows to draw a
        // border in, which it never had before this fix.
        Fetch::NotStarted | Fetch::Loading => {
            render_loading(frame, chunks[1]);
        }
        Fetch::Failed(e) => {
            render_fetch_error(frame, chunks[1], e);
        }
        Fetch::Ready(summary) => {
            let body = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
                .split(chunks[1]);
            let now_playing = Line::from(vec![
                Span::styled("now playing  ", Style::default().fg(DIM)),
                Span::raw(match &summary.currently_playing {
                    Some(t) => format!("{} \u{2014} {}", t.artist, t.title),
                    None => "(nothing)".to_string(),
                }),
            ]);
            frame.render_widget(Paragraph::new(now_playing), body[0]);
            if summary.queue.is_empty() {
                render_empty_state(frame, body[1], "queue is empty", None);
                return;
            }
            let label = |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title);
            let display: Vec<(usize, &TrackResult)> = summary.queue.iter().enumerate().collect();
            render_display_list(frame, body[1], &display, app.queue.selected, &label, true, list_state);
        }
    }
}

/// Connect devices (Phase 8): a plain list, the active one marked. `r`
/// refetches manually (see `DevicesState`'s own doc comment for why this
/// doesn't poll like Queue does).
pub(super) fn render_devices(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(screen_header_line("Devices", Some("Enter transfer playback, Shift+R refresh"))),
        chunks[0],
    );
    match &app.devices.fetch {
        Fetch::NotStarted | Fetch::Loading => {
            render_loading(frame, chunks[1]);
        }
        Fetch::Failed(e) => {
            render_fetch_error(frame, chunks[1], e);
        }
        Fetch::Ready(items) if items.is_empty() => {
            render_empty_state(
                frame,
                chunks[1],
                "no devices found",
                Some("open Spotify on another device, or press Shift+R to check again"),
            );
        }
        Fetch::Ready(items) => {
            // The active marker keeps ACCENT whether or not this row is
            // also selected -- an explicitly-styled span's own color
            // patches over the row's selection style per-cell, so
            // `render_display_list_lines` (not the plain-string
            // `render_display_list`) is what makes this possible.
            // Previously the marker was plain text baked into the label,
            // so an active-but-unselected device read identically to an
            // inactive one except for the bare glyph.
            let line = |d: &crate::api::devices::DeviceSummary| {
                let marker = if d.is_active { "\u{25cf} " } else { "  " };
                let volume = d.volume_percent.map(|v| format!(", {v}%")).unwrap_or_default();
                Line::from(vec![
                    Span::styled(marker, Style::default().fg(ACCENT)),
                    Span::raw(format!("{} ({}{volume})", d.name, d.kind)),
                ])
            };
            let display: Vec<(usize, &crate::api::devices::DeviceSummary)> = items.iter().enumerate().collect();
            render_display_list_lines(frame, chunks[1], &display, app.devices.selected, &line, true, list_state);
        }
    }
}

/// The two drill-down detail screens (Artist, Album) share this shape: a
/// one-row styled header over a plain list of the thing's children.
/// Previously duplicated in full, including re-rendering the header
/// separately inside each of the three `Fetch` arms -- second concrete
/// case of the identical shell, past this codebase's own established
/// "extract on the second case" bar. `fallback_title` is what the header
/// shows before the real name is known (loading/error), so it's never
/// blank and never written three times.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_detail_screen<D, T>(
    frame: &mut Frame,
    area: Rect,
    detail: &Fetch<D>,
    selected: usize,
    list_state: &mut ListState,
    fallback_title: &str,
    header: impl Fn(&D) -> Line<'static>,
    items: impl Fn(&D) -> &[T],
    label: impl Fn(&T) -> String,
    empty_headline: &str,
    empty_hint: Option<&str>,
) {
    let chunks =
        Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1), Constraint::Min(1)]).split(area);
    match detail {
        Fetch::NotStarted | Fetch::Loading => {
            frame.render_widget(Paragraph::new(screen_header_line(fallback_title, None)), chunks[0]);
            render_loading(frame, chunks[1]);
        }
        Fetch::Failed(e) => {
            frame.render_widget(Paragraph::new(screen_header_line(fallback_title, None)), chunks[0]);
            render_fetch_error(frame, chunks[1], e);
        }
        Fetch::Ready(d) => {
            frame.render_widget(Paragraph::new(header(d)), chunks[0]);
            let child_items = items(d);
            if child_items.is_empty() {
                render_empty_state(frame, chunks[1], empty_headline, empty_hint);
                return;
            }
            let display: Vec<(usize, &T)> = child_items.iter().enumerate().collect();
            render_display_list(frame, chunks[1], &display, selected, &label, true, list_state);
        }
    }
}

/// Artist Detail (Phase 9): name + genres in the header, a plain list of
/// albums below. No top-tracks section -- see `api::artist`'s own doc
/// comment for why (Spotify removed that endpoint).
pub(super) fn render_artist_detail(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let Some(state) = &app.artist_detail else {
        frame.render_widget(Paragraph::new("no artist selected"), area);
        return;
    };
    render_detail_screen(
        frame,
        area,
        &state.detail,
        state.selected,
        list_state,
        "Artist",
        |artist: &crate::api::artist::ArtistDetail| {
            let genres = (!artist.genres.is_empty()).then(|| artist.genres.join(", "));
            screen_header_line(&artist.name, genres.as_deref())
        },
        |artist: &crate::api::artist::ArtistDetail| artist.albums.as_slice(),
        |a: &crate::api::library::SavedAlbumSummary| a.name.clone(),
        "no albums for this artist",
        None,
    );
}

/// Album Detail (Phase 9): name + artist in the header, the track list
/// below -- `Enter` plays the album as context starting from the
/// selected track, same convention `render_playlist_detail` already
/// established.
pub(super) fn render_album_detail(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let Some(state) = &app.album_detail else {
        frame.render_widget(Paragraph::new("no album selected"), area);
        return;
    };
    render_detail_screen(
        frame,
        area,
        &state.detail,
        state.selected,
        list_state,
        "Album",
        |album: &crate::api::album::AlbumDetail| {
            // Single-space em dash, matching every track label in the
            // app (including this same screen's own list) -- the
            // previous double-padded "  --  " was the only one of its
            // kind in the file.
            let meta = format!("\u{2014} {} \u{00b7} {} tracks \u{00b7} v view artist", album.artist, album.tracks.len());
            screen_header_line(&album.name, Some(&meta))
        },
        |album: &crate::api::album::AlbumDetail| album.tracks.as_slice(),
        |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title),
        "no tracks on this album",
        None,
    );
}

/// Renders one of the 4 uniform fetched-list screens (Liked Songs, Saved
/// Albums, Followed Artists, Playlist Detail tracks). Your Playlists gets
/// its own renderer instead -- it's the one list with an extra per-item
/// concept (pinning) this generic version has no notion of.
pub(super) fn render_list_screen<T>(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    fetch: &Fetch<Vec<T>>,
    filter: &ListFilter,
    selected: usize,
    list_state: &mut ListState,
    label: impl Fn(&T) -> String,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(filter_header(title, filter)).style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    match fetch {
        Fetch::NotStarted | Fetch::Loading => {
            render_loading(frame, chunks[1]);
        }
        Fetch::Failed(e) => {
            render_fetch_error(frame, chunks[1], e);
        }
        Fetch::Ready(items) => {
            let display = filtered_sorted(items, filter, &label);
            render_display_list(
                frame,
                chunks[1],
                &display,
                selected,
                &label,
                filter.query.is_empty(),
                list_state,
            );
        }
    }
}

/// Renders `query` with a block cursor sitting at char index `cursor` --
/// the app's one real text-input convention. Previously open-coded
/// separately at every call site (`filter_header`, Search's own query
/// line, the text-prompt overlay); the playlist-picker and quick-jump
/// overlays skipped this entirely and drew a trailing-only cursor, which
/// actively lied -- both already support real `Left`/`Right` cursor
/// movement in their key handlers.
pub(super) fn cursor_text(query: &str, cursor: usize) -> String {
    let byte_pos = query.char_indices().nth(cursor).map(|(b, _)| b).unwrap_or(query.len());
    let (before, after) = query.split_at(byte_pos);
    format!("{before}\u{2588}{after}")
}

#[cfg(test)]
mod cursor_text_tests {
    use super::*;

    #[test]
    fn cursor_at_zero_leads() {
        assert_eq!(cursor_text("hello", 0), "\u{2588}hello");
    }

    #[test]
    fn cursor_mid_string_splits_there() {
        assert_eq!(cursor_text("hello", 2), "he\u{2588}llo");
    }

    #[test]
    fn cursor_past_the_end_trails() {
        assert_eq!(cursor_text("hello", 99), "hello\u{2588}");
    }

    #[test]
    fn char_boundary_safe_on_multibyte_text() {
        // Same real fixture the SearchState cursor tests already use --
        // chars are 0:友 1:人 2:A 3:君, so cursor=2 sits immediately
        // before 'A', not mid-codepoint.
        assert_eq!(cursor_text("\u{53cb}\u{4eba}A\u{541b}", 2), "\u{53cb}\u{4eba}\u{2588}A\u{541b}");
    }
}

/// The app's screen-header row: title bold, an optional secondary fact
/// demoted beside it in `DIM` -- replaces screens that were cramming key
/// hints or extra facts into the title string at equal visual weight
/// (Devices' "(Enter: transfer, r: refresh)", Album Detail's "(v: view
/// artist)").
pub(super) fn screen_header_line(title: &str, meta: Option<&str>) -> Line<'static> {
    let mut spans = vec![Span::styled(title.to_string(), Style::default().add_modifier(Modifier::BOLD))];
    if let Some(meta) = meta {
        spans.push(Span::styled(format!("  {meta}"), Style::default().fg(DIM)));
    }
    Line::from(spans)
}

/// `/` (start typing a filter) shows the live query with a cursor, same
/// convention as the global Search screen's own query line. Otherwise
/// shows whatever filter/sort is currently applied, if any.
pub(super) fn filter_header(title: &str, filter: &ListFilter) -> String {
    if filter.editing {
        // Cursor renders at its real position, same as Search's own
        // query line -- Left/Right move it mid-string here too.
        format!("{title}  /{}", cursor_text(&filter.query, filter.cursor))
    } else if !filter.query.is_empty() || filter.sort_alpha {
        let mut parts = Vec::new();
        if !filter.query.is_empty() {
            parts.push(format!("filter: \"{}\"", filter.query));
        }
        if filter.sort_alpha {
            parts.push("A-Z".to_string());
        }
        format!("{title}  ({})", parts.join(", "))
    } else {
        title.to_string()
    }
}

/// Same contract as `render_display_list`, but rows arrive as `Line`s
/// instead of a plain label string -- lets a screen color one span (a
/// pin/playing/moving marker) independently of whether that row happens
/// to be selected. Ratatui patches an explicitly-styled span's own color
/// over the row's base style per-cell, so a marker span with its own
/// `.fg(...)` keeps that color even on a selected (ACCENT+BOLD) row,
/// while any unstyled span in the same line still follows selection
/// normally. This is what makes Devices' active-device marker and
/// move-mode's moving-row arrow possible without changing
/// `render_display_list`'s own signature or its 9 existing callers.
pub(super) fn render_display_list_lines<T>(
    frame: &mut Frame,
    area: Rect,
    display: &[(usize, &T)],
    selected: usize,
    line: &impl Fn(&T) -> Line<'static>,
    filter_empty: bool,
    list_state: &mut ListState,
) {
    if display.is_empty() {
        let msg = if filter_empty { "nothing here yet" } else { "no matches" };
        render_empty_state(frame, area, msg, None);
        return;
    }
    let list_items: Vec<ListItem> = display
        .iter()
        .enumerate()
        .map(|(i, (_, it))| {
            let text = line(it);
            if i == selected {
                ListItem::new(text).style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            } else {
                ListItem::new(text)
            }
        })
        .collect();
    // Mutates just `.selected`, keeping whatever `.offset` this list_state
    // already had from the previous frame -- ratatui only moves the
    // offset if `selected` would otherwise fall outside the current
    // viewport, exactly the "only scroll at the edges" behavior a plain
    // `ListState::default()` (offset reset to 0 every frame) broke.
    list_state.select(Some(selected));
    frame.render_stateful_widget(List::new(list_items), area, list_state);
}

pub(super) fn render_display_list<T>(
    frame: &mut Frame,
    area: Rect,
    display: &[(usize, &T)],
    selected: usize,
    label: &impl Fn(&T) -> String,
    filter_empty: bool,
    list_state: &mut ListState,
) {
    render_display_list_lines(frame, area, display, selected, &|it: &T| Line::from(label(it)), filter_empty, list_state);
}

pub(super) fn render_sidebar(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let rows = sidebar_rows(app);
    let mut items: Vec<ListItem> = Vec::with_capacity(rows.len() + 1);
    let mut saw_playlists_header = false;
    for (i, row) in rows.iter().enumerate() {
        if matches!(row, SidebarRow::Playlist(_)) && !saw_playlists_header {
            items.push(ListItem::new("PLAYLISTS").style(Style::default().fg(DIM)));
            saw_playlists_header = true;
        }
        // "Is this what's currently showing in Main" -- checked against
        // `top()` alone, not stack depth. `goto()` always keeps NowPlaying
        // at the bottom of the stack (see Nav::goto), so anything reached
        // via the Sidebar sits at depth 2, never depth 1 -- a lingering
        // `depth() == 1` check here (from before that invariant existed)
        // meant this could only ever light up for Now Playing itself,
        // reported live as no visible "which item am I in" indicator at
        // all once you'd navigated anywhere else.
        let (text, is_open) = match row {
            SidebarRow::Menu(label, screen) => (label.to_string(), app.nav.top() == screen),
            SidebarRow::Playlist(p) => {
                let marker = if app.pinned_playlists.contains(&p.uri) { "* " } else { "  " };
                let is_open = *app.nav.top() == Screen::PlaylistDetail
                    && app.playlist_detail.as_ref().is_some_and(|pd| pd.playlist.uri == p.uri);
                (format!("{marker}{}", p.name), is_open)
            }
        };
        let is_cursor = app.nav.focus == Focus::Sidebar && i == app.sidebar_sel;
        let mut style = Style::default();
        if is_open {
            style = style.fg(ACCENT).add_modifier(Modifier::BOLD);
        }
        if is_cursor {
            style = style.add_modifier(Modifier::REVERSED);
        }
        items.push(ListItem::new(text).style(style));
    }
    // The "PLAYLISTS" header takes up one visual row that `sidebar_sel`
    // (an index into logical rows: menu entries + playlists, no header)
    // doesn't know about -- shift the on-screen selection down by one
    // once the cursor is actually on a playlist row, past where the
    // header was inserted.
    let header_offset = if saw_playlists_header && app.sidebar_sel >= SIDEBAR_ENTRIES.len() { 1 } else { 0 };
    list_state.select(Some(app.sidebar_sel + header_offset));
    frame.render_stateful_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::RIGHT | Borders::TOP)
                .border_style(focus_border_style(app.nav.focus == Focus::Sidebar)),
        ),
        area,
        list_state,
    );
}

pub(super) fn render_search(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    // Cursor renders at its real position, not always trailing -- Left/Right
    // now move it mid-string (arrow-key editing, reported live as missing).
    let query_line = format!("/ {}", cursor_text(&app.search.query, app.search.cursor));
    frame.render_widget(
        Paragraph::new(query_line).block(Block::default().borders(Borders::ALL).title("search")),
        chunks[0],
    );

    if app.search.searching {
        render_loading(frame, chunks[1]);
        return;
    }
    if let Some(err) = &app.search.error {
        render_fetch_error(frame, chunks[1], err);
        return;
    }
    if app.search.results.is_empty() {
        let headline = if !app.search.client_ready {
            "search not ready yet (loading Spotify auth\u{2026})"
        } else if app.search.query.is_empty() {
            "type a query, then Enter to search, Esc to cancel"
        } else {
            // Distinct from the empty-query message on purpose: this is
            // the state reported live as "have to click enter first and
            // then scroll" -- clarifying *why* up/down do nothing yet
            // (there's a real Web API call to make, not a local list to
            // narrow) rather than leaving it looking broken or identical
            // to having typed nothing at all.
            "press Enter to search \u{2014} this hits Spotify directly, not a live filter like Library's /"
        };
        render_empty_state(frame, chunks[1], headline, None);
        return;
    }

    let items: Vec<ListItem> = app
        .search
        .results
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let text = format!("{} \u{2014} {} [{}]", t.artist, t.title, t.album);
            if i == app.search.selected {
                ListItem::new(text).style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            } else {
                ListItem::new(text)
            }
        })
        .collect();
    list_state.select(Some(app.search.selected));
    frame.render_stateful_widget(List::new(items), chunks[1], list_state);
}

