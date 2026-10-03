use super::*;

pub(super) fn render_playbar(frame: &mut Frame, app: &AppState, area: Rect) {
    // 28 is the room the icon, time, and volume readouts already need; the
    // always-present shuffle/repeat toggles (plus their 3-space gap) come
    // out of the title's share.
    let title_room = (area.width as usize).saturating_sub(28 + 3 + PLAYBACK_MODES_WIDTH);
    let [shuffle, smart, repeat] = playback_modes(app.shuffle, app.smart_shuffle, app.repeat);
    let toggle_style = |on: bool| Style::default().fg(if on { ACCENT } else { DIM });
    let spans = vec![
        Span::raw(format!(
            "{} {}   {}   {}   ",
            playing_icon(app),
            header(app, title_room),
            time_readout(app),
            volume_readout(app),
        )),
        Span::styled(shuffle.0, toggle_style(shuffle.1)),
        Span::styled(smart.0, toggle_style(smart.1)),
        Span::styled(repeat.0, toggle_style(repeat.1)),
    ];
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::TOP)),
        area,
    );
}

pub(super) fn render_status(frame: &mut Frame, app: &AppState, area: Rect) {
    // Move mode's key hints live here, not in the header row (which is
    // `Constraint::Length(1)` with no wrap, so the old placement was
    // already silently truncated on a narrow terminal) -- checked before
    // `app.status` since both `Enter`/`Esc` in move mode already call
    // `pd.move_mode.take()` before dispatching a reorder, so no mutation
    // result can ever land while this branch would also be showing.
    if *app.nav.top() == Screen::PlaylistDetail
        && app.playlist_detail.as_ref().is_some_and(|pd| pd.move_mode.is_some())
    {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("MOVE MODE", Style::default().fg(WARN).add_modifier(Modifier::BOLD)),
                Span::styled(
                    "  \u{2191}/\u{2193} relocate, g jump to position, Enter confirm, Esc cancel",
                    Style::default().fg(DIM),
                ),
            ])),
            area,
        );
        return;
    }
    // A Phase 5 mutation's result (success or failure) takes over this
    // line until the next keypress, same lifetime a status line
    // conventionally gets -- the depth readout resumes once it's gone.
    if let Some((message, is_error)) = &app.status {
        let color = if *is_error { DANGER } else { ACCENT };
        frame.render_widget(Paragraph::new(message.clone()).style(Style::default().fg(color)), area);
        return;
    }
    let text = format!("stack depth {} \u{2014} Tab switch pane, Esc back", app.nav.depth());
    frame.render_widget(Paragraph::new(text).style(Style::default().fg(DIM)), area);
}

pub(super) fn header(app: &AppState, max_chars: usize) -> String {
    match (&app.track_artist, &app.track_title) {
        (Some(a), Some(t)) => truncate_ellipsis(&format!("{a} \u{2014} {t}"), max_chars),
        // The persistent playback bar renders every frame regardless of
        // which screen is up top, so during a reconnect it was still
        // saying "press / to search" -- true of the idle-on-launch case
        // this line is really for, false while the session is down and
        // nothing can be searched yet.
        _ if matches!(app.lyrics, LyricsState::SessionEnded) => "reconnecting\u{2026}".to_string(),
        _ => "ready \u{2014} press / to search\u{2026}".to_string(),
    }
}

pub(super) fn playing_icon(app: &AppState) -> &'static str {
    match app.playing {
        Some(true) => "\u{25b6}",  // ▶
        Some(false) => "\u{23f8}", // ⏸
        None => "\u{22ef}",        // ⋯ (no track loaded yet)
    }
}

pub(super) fn time_readout(app: &AppState) -> String {
    format!("{} / {}", format_mmss(app.position), format_mmss(app.duration))
}

/// Plain text, not an emoji/icon: an emoji here would often be
/// double-width and throw off column alignment in the header row,
/// unlike the single-width play/pause glyphs used elsewhere.
pub(super) fn volume_readout(app: &AppState) -> String {
    let percent = (app.volume as u32 * 100) / u16::MAX as u32;
    format!("vol {percent}%")
}

pub(super) fn progress_ratio(app: &AppState) -> f64 {
    if app.duration.is_zero() {
        return 0.0;
    }
    (app.position.as_secs_f64() / app.duration.as_secs_f64()).clamp(0.0, 1.0)
}

pub(super) fn progress_gauge(app: &AppState) -> Gauge<'static> {
    let color = if app.playing == Some(false) {
        DIM // frozen/paused reads as visually "asleep"
    } else {
        ACCENT
    };
    Gauge::default().gauge_style(Style::default().fg(color)).label("").ratio(progress_ratio(app))
}

/// Same gauge, with a border -- used only by the fullscreen layouts
/// (which have a whole extra row to spare for it), not the compact view.
/// At low progress (a song's first few seconds) an unbordered gauge is
/// almost entirely its own background color, which reads as "a tiny
/// colored square" with no visible indication of where the bar actually
/// ends. The border always outlines the full capsule regardless of how
/// little of it is filled.
pub(super) fn progress_gauge_bordered(app: &AppState) -> Gauge<'static> {
    progress_gauge(app).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(DIM)))
}

