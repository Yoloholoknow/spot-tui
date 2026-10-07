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
    // Move mode's key hints live here, not in the one-line header, which does not
    // wrap. Checked before `app.status`: Enter and Esc clear move mode before any
    // mutation is dispatched, so no result can land while this is showing.
    if *app.nav.top() == Screen::PlaylistDetail
        && app
            .playlist_detail
            .as_ref()
            .is_some_and(|pd| pd.move_mode.is_some())
    {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "MOVE MODE",
                    Style::default().fg(WARN).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "  \u{2191}/\u{2193} relocate, g jump to position, Enter confirm, Esc cancel",
                    Style::default().fg(DIM),
                ),
            ])),
            area,
        );
        return;
    }
    // A mutation's result takes over this line until the next key press; the
    // depth readout resumes after.
    if let Some((message, is_error)) = &app.status {
        let color = if *is_error { DANGER } else { ACCENT };
        frame.render_widget(
            Paragraph::new(message.clone()).style(Style::default().fg(color)),
            area,
        );
        return;
    }
    let text = format!(
        "stack depth {} \u{2014} Tab switch pane, Esc back",
        app.nav.depth()
    );
    frame.render_widget(Paragraph::new(text).style(Style::default().fg(DIM)), area);
}

/// Marks a track smart shuffle added, as opposed to one from the playlist
/// itself. Only while smart shuffle is on, so a stale set never shows.
pub(super) fn recommended(app: &AppState, uri: &str) -> bool {
    app.smart_shuffle && librespot_connect::is_recommended(uri)
}

/// `text` with the sparkle prefix when `marked`.
pub(super) fn with_recommendation_marker(text: String, marked: bool) -> String {
    if marked {
        format!("\u{2726} {text}")
    } else {
        text
    }
}

pub(super) fn header(app: &AppState, max_chars: usize) -> String {
    match (&app.track_artist, &app.track_title) {
        (Some(a), Some(t)) => {
            let marked = app
                .current_track_uri
                .as_deref()
                .is_some_and(|u| recommended(app, u));
            truncate_ellipsis(
                &with_recommendation_marker(format!("{a} \u{2014} {t}"), marked),
                max_chars,
            )
        }
        // The playbar renders on every screen, so during a reconnect it must not say
        // "press / to search", which is only true when idle after launch.
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
    format!(
        "{} / {}",
        format_mmss(app.position),
        format_mmss(app.duration)
    )
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
    Gauge::default()
        .gauge_style(Style::default().fg(color))
        .label("")
        .ratio(progress_ratio(app))
}

/// Same gauge with a border, for the fullscreen layouts. Unbordered, a gauge at
/// low progress is nearly all background and reads as a tiny square; the border
/// always outlines the whole capsule.
pub(super) fn progress_gauge_bordered(app: &AppState) -> Gauge<'static> {
    progress_gauge(app).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(DIM)),
    )
}

#[cfg(test)]
mod marker_tests {
    use super::*;

    #[test]
    fn a_recommendation_gets_the_sparkle_and_anything_else_is_untouched() {
        assert_eq!(
            with_recommendation_marker("A \u{2014} B".into(), true),
            "\u{2726} A \u{2014} B"
        );
        assert_eq!(
            with_recommendation_marker("A \u{2014} B".into(), false),
            "A \u{2014} B"
        );
    }
}
