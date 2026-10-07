//! Smart shuffle recommendations (Phase 25, step 5). Spotify's own clients get
//! them by POSTing a `reset` signal carrying the `enhance` lens to the
//! playlist; the reply is the playlist's items with recommendations already
//! included. This first step only *looks*: it builds that request and
//! summarizes the reply for the log, and changes no playback behavior.

use crate::protocol::{
    lens_model::Lens,
    player::ProvidedTrack,
    playlist4_external::{ListSignals, SelectedListContent},
    signal_model::Signal,
};
use protobuf::Message;
use std::collections::BTreeSet;
use std::sync::Mutex;
use uuid::Uuid;

/// Uris of the recommendations currently injected into the context, for
/// whoever draws the queue or the now-playing line. A process-global, like
/// `smart_shuffle_active`: there is no player event to carry it.
static RECOMMENDED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Whether `uri` is a track smart shuffle added, as opposed to one from the
/// playlist itself. False once smart shuffle is turned off.
pub fn is_recommended(uri: &str) -> bool {
    RECOMMENDED.lock().is_ok_and(|set| set.contains(uri))
}

/// Replaces the recorded set with the recommendations in `order`.
pub(crate) fn record_recommendations(order: &[PlaybackItem]) {
    if let Ok(mut set) = RECOMMENDED.lock() {
        *set = recommended_uris(order);
    }
}

pub(crate) fn clear_recommendations() {
    if let Ok(mut set) = RECOMMENDED.lock() {
        set.clear();
    }
}

fn recommended_uris(order: &[PlaybackItem]) -> BTreeSet<String> {
    order
        .iter()
        .filter(|item| item.is_recommendation)
        .map(|item| item.uri.clone())
        .collect()
}

/// Marks a `ProvidedTrack` this app built for a smart-shuffle recommendation,
/// as opposed to a real entry from the playlist itself. Not one of librespot's
/// own recognized providers (`context`/`queue`/`autoplay`/`unavailable`), so
/// it is inert everywhere else -- only code that looks for it specifically
/// (stripping recommendations back out when smart shuffle turns off) does.
pub const PROVIDER_RECOMMENDATION: &str = "smart_shuffle_recommendation";

/// A minimal `ProvidedTrack` for a recommended uri that wasn't already in the
/// playlist's own context. The player only needs the uri to load and play a
/// track; the rest of `ProvidedTrack` is what this app tells *other* Connect
/// clients about the queue, which a fresh, mostly-empty entry is an honest
/// (if sparse) answer to.
pub fn recommended_track(uri: &str) -> ProvidedTrack {
    ProvidedTrack {
        uri: uri.to_string(),
        uid: Uuid::new_v4().as_simple().to_string(),
        provider: PROVIDER_RECOMMENDATION.to_string(),
        ..Default::default()
    }
}

/// The playlist Spotify's own clients send the `enhance` signal to for Liked
/// Songs. The web player hard-codes it (`spotify:playlist:37i9dQZF1F5p3rmiWPIYgZ`,
/// the same for every account): the backend answers it as the asking user's
/// Liked Songs. `getEligibility` for a collection finds no per-user playlist
/// and falls back to this constant.
pub const LIKED_SONGS_LENS_PLAYLIST: &str = "spotify:playlist:37i9dQZF1F5p3rmiWPIYgZ";

/// Where smart shuffle recommendations come from for a playing context: the
/// playlist itself, or the shared Liked Songs playlist for a user's
/// collection (`spotify:user:<id>:collection`). `None` for anything else
/// (albums, artists, single tracks), which have no smart shuffle.
pub fn lens_playlist_uri(context_uri: &str) -> Option<String> {
    if context_uri.starts_with("spotify:playlist:") {
        return Some(context_uri.to_string());
    }
    let mut parts = context_uri.split(':');
    match (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("spotify"), Some("user"), Some(user), Some("collection"), None) if !user.is_empty() => {
            Some(LIKED_SONGS_LENS_PLAYLIST.to_string())
        }
        _ => None,
    }
}

/// The lens Spotify applies for smart shuffle.
pub const ENHANCE_LENS: &str = "enhance";

/// The attribute that (per an earlier reverse-engineering attempt) says where
/// an item sits in the shuffled order. Logged, not yet relied on.
const DISTRIBUTION_KEY: &str = "shuffle.distribution";

/// The signal the official clients send to apply smart shuffle: `reset`, with
/// the `enhance` lens as its data.
pub fn enhance_reset_request() -> ListSignals {
    let mut lens = Lens::new();
    lens.identifier = ENHANCE_LENS.to_string();
    let mut signal = Signal::new();
    signal.identifier = "reset".to_string();
    signal.data = lens.write_to_bytes().unwrap_or_default();
    let mut request = ListSignals::new();
    request.emitted_signals.push(signal);
    request
}

/// One track from a smart-shuffle reply, in the order it should play.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackItem {
    pub uri: String,
    pub is_recommendation: bool,
}

/// The real wire encoding, confirmed live: a float wrapped as `d(22.0)`, not
/// a bare number.
fn parse_distribution(value: &str) -> Option<f64> {
    let inner = value.strip_prefix("d(")?.strip_suffix(')')?;
    inner.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Every item in a smart-shuffle reply, in the order Spotify means them to
/// play: sorted by `shuffle.distribution`, originals and recommendations
/// together, exactly what a live reply's own numbering already confirmed is
/// a full 1..=N permutation across both. An item with no usable distribution
/// keeps its position relative to the others (a stable sort against `f64::MAX`)
/// rather than being dropped -- a real reply should never have one, but this
/// never trusts an external response enough to panic or lose a track over it.
pub fn build_playback_order(content: &SelectedListContent) -> Vec<PlaybackItem> {
    let items = content.contents.as_ref().map(|c| c.items.as_slice()).unwrap_or(&[]);
    let mut order: Vec<PlaybackItem> = items
        .iter()
        .map(|item| PlaybackItem {
            uri: item.uri().to_string(),
            is_recommendation: item
                .attributes
                .as_ref()
                .and_then(|a| a.recommendation_info.as_ref())
                .and_then(|info| info.is_recommendation)
                .unwrap_or(false),
        })
        .collect();
    let distribution = |item: &crate::protocol::playlist4_external::Item| {
        item.attributes
            .as_ref()
            .map(|a| a.format_attributes.as_slice())
            .unwrap_or(&[])
            .iter()
            .find(|a| a.key.as_deref() == Some(DISTRIBUTION_KEY))
            .and_then(|a| a.value.as_deref())
            .and_then(parse_distribution)
            .unwrap_or(f64::MAX)
    };
    let mut keyed: Vec<(f64, PlaybackItem)> =
        items.iter().map(distribution).zip(order.drain(..)).collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    keyed.into_iter().map(|(_, item)| item).collect()
}

/// Where the upcoming-queue walk should resume after `current_index` (the
/// currently-playing track's own position in `order`): the position right
/// after it, unless nothing from there to the end is a recommendation --
/// confirmed live (2026-09-22) to actually happen: a 370-item reply had every
/// recommendation in positions 2-197, none in 198-369, and a real session's
/// current track had landed at 276, so continuing forward would never reach
/// one. Wrapping to the front only in that dead-zone case reaches
/// recommendations reliably without needlessly disturbing continuity when
/// they're already ahead -- most positions aren't in a dead zone at all.
/// Manually queued tracks are untouched either way: they live in a separate
/// list this mechanism never writes to.
pub fn resume_index(order: &[PlaybackItem], current_index: usize) -> usize {
    let any_recommendation = order.iter().any(|item| item.is_recommendation);
    let ahead = order.get(current_index + 1..).unwrap_or(&[]);
    if !any_recommendation || ahead.iter().any(|item| item.is_recommendation) {
        current_index + 1
    } else {
        0
    }
}

/// Where the upcoming-queue walk resumes once the recommendations are
/// removed from `before` (each entry: uri, is-recommendation). Right after the
/// current track's slot among the survivors; if the current track was itself a
/// recommendation (and so is going away), at the first original that followed
/// it; 0 if it isn't in the list at all. Without this the walk cursor keeps
/// its old position in the longer list, which can be past the end of the
/// shorter one, so the refill finds nothing and playback stops.
pub fn cursor_after_strip(before: &[(String, bool)], current_uri: &str) -> usize {
    let Some(position) = before.iter().position(|(uri, _)| uri == current_uri) else {
        return 0;
    };
    before[..=position].iter().filter(|(_, recommendation)| !recommendation).count()
}

/// Where the walk resumes in a restored `order` (the playlist's own order):
/// right after the current track, or 0 if it isn't there (it was a
/// recommendation).
pub fn cursor_in_order(order: &[String], current_uri: &str) -> usize {
    order.iter().position(|uri| uri == current_uri).map_or(0, |position| position + 1)
}

/// What a reply looks like, for the log.
#[derive(Debug, Default)]
pub struct ProbeSummary {
    pub items: usize,
    pub recommendations: usize,
    /// Every format-attribute key seen on any item.
    pub keys: BTreeSet<String>,
    /// One line per item: position, uri, recommendation flag, distribution,
    /// and any other attributes.
    pub lines: Vec<String>,
}

pub fn summarize(content: &SelectedListContent) -> ProbeSummary {
    let items = content.contents.as_ref().map(|c| c.items.as_slice()).unwrap_or(&[]);
    let mut summary = ProbeSummary { items: items.len(), ..Default::default() };
    for (position, item) in items.iter().enumerate() {
        let attributes = item.attributes.as_ref();
        let recommendation = attributes
            .and_then(|a| a.recommendation_info.as_ref())
            .and_then(|info| info.is_recommendation)
            .unwrap_or(false);
        if recommendation {
            summary.recommendations += 1;
        }
        let mut distribution = "-".to_string();
        let mut others = Vec::new();
        for attribute in attributes.map(|a| a.format_attributes.as_slice()).unwrap_or(&[]) {
            let key = attribute.key.clone().unwrap_or_default();
            let value = attribute.value.clone().unwrap_or_default();
            summary.keys.insert(key.clone());
            if key == DISTRIBUTION_KEY {
                distribution = value;
            } else {
                others.push(format!("{key}={value}"));
            }
        }
        summary.lines.push(format!(
            "{position} {} rec={recommendation} dist={distribution} [{}]",
            item.uri(),
            others.join(",")
        ));
    }
    summary
}

#[cfg(test)]
mod strip_cursor_tests {
    use super::*;

    fn list(spec: &str) -> Vec<(String, bool)> {
        // "a r1* b": a trailing * marks a recommendation
        spec.split(' ').map(|t| (t.trim_end_matches('*').to_string(), t.ends_with('*'))).collect()
    }

    #[test]
    fn resumes_right_after_an_original_current_track() {
        // a r b r c: survivors are a b c; current b is slot 1, so resume at 2.
        assert_eq!(cursor_after_strip(&list("a r* b r2* c"), "b"), 2);
        assert_eq!(cursor_after_strip(&list("a r* b r2* c"), "a"), 1);
    }

    #[test]
    fn a_current_recommendation_resumes_at_the_next_original() {
        // current r2 is removed; the next original is c, survivors a b c -> 2.
        assert_eq!(cursor_after_strip(&list("a r* b r2* c"), "r2"), 2);
        assert_eq!(cursor_after_strip(&list("r* a"), "r"), 0);
    }

    #[test]
    fn the_cursor_stays_inside_the_stripped_list() {
        // The real failure: a cursor left past the end of the shorter list.
        let before = list("a b c r* d r2*");
        let survivors = before.iter().filter(|(_, r)| !r).count();
        for (uri, _) in &before {
            assert!(cursor_after_strip(&before, uri) <= survivors, "{uri}");
        }
    }

    #[test]
    fn an_unknown_current_track_starts_over() {
        assert_eq!(cursor_after_strip(&list("a b"), "zzz"), 0);
        assert_eq!(cursor_in_order(&["a".into(), "b".into()], "zzz"), 0);
    }

    #[test]
    fn a_restored_order_resumes_after_the_current_track() {
        let order: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(cursor_in_order(&order, "b"), 2);
        assert_eq!(cursor_in_order(&order, "c"), 3);
    }
}

#[cfg(test)]
mod lens_source_tests {
    use super::*;

    #[test]
    fn a_playlist_is_its_own_source() {
        assert_eq!(
            lens_playlist_uri("spotify:playlist:3Gb1jkqu3ocILrA7TX8dHX").as_deref(),
            Some("spotify:playlist:3Gb1jkqu3ocILrA7TX8dHX")
        );
    }

    #[test]
    fn liked_songs_uses_the_shared_lens_playlist() {
        assert_eq!(
            lens_playlist_uri("spotify:user:abc123:collection").as_deref(),
            Some(LIKED_SONGS_LENS_PLAYLIST)
        );
    }

    #[test]
    fn contexts_without_smart_shuffle_have_no_source() {
        for uri in [
            "spotify:album:3jFA9WxhV1tIEzpg2VMxjm",
            "spotify:track:1L0tsbU4DaOO3DCUuJZKoL",
            "spotify:user:abc123:collection:artist:xyz",
            "spotify:user::collection",
            "spotify:artist:5GARimhxfjVVPvVBsplJlv",
            "",
        ] {
            assert_eq!(lens_playlist_uri(uri), None, "{uri}");
        }
    }
}

#[cfg(test)]
mod recommended_tests {
    use super::*;

    fn item(uri: &str, is_recommendation: bool) -> PlaybackItem {
        PlaybackItem {
            uri: uri.to_string(),
            is_recommendation,
        }
    }

    #[test]
    fn only_recommendations_are_recorded_never_the_playlists_own_tracks() {
        let order = [item("a", false), item("r1", true), item("b", false), item("r2", true)];
        let set = recommended_uris(&order);
        assert_eq!(set.into_iter().collect::<Vec<_>>(), ["r1", "r2"]);
    }

    #[test]
    fn recording_replaces_the_previous_set_and_clearing_empties_it() {
        // One test owns the global, so parallel tests can't race on it.
        record_recommendations(&[item("old", true)]);
        assert!(is_recommended("old"));
        record_recommendations(&[item("new", true), item("own", false)]);
        assert!(!is_recommended("old"));
        assert!(is_recommended("new"));
        assert!(!is_recommended("own"));
        clear_recommendations();
        assert!(!is_recommended("new"));
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;

    // Real numbers from a live probe (2026-09-22) against a real 50-track
    // playlist: 75 items came back (50 original + 25 recommendations),
    // `shuffle.distribution` was a full 1..=75 permutation across *both*
    // groups together, and the 25 recommendation uris were confirmed by a
    // direct Web API check to be genuinely new -- none were already in the
    // playlist, and the playlist itself was unchanged afterward. That
    // confirms alternative A (order the whole reply by `shuffle.distribution`)
    // over the hybrid A': there's a real, single authoritative order to sort
    // by, not just a recommendation pool to interleave.
    fn attr(key: &str, value: &str) -> crate::protocol::playlist4_external::FormatListAttribute {
        let mut a = crate::protocol::playlist4_external::FormatListAttribute::new();
        a.key = Some(key.to_string());
        a.value = Some(value.to_string());
        a
    }

    fn item(uri: &str, recommendation: bool, dist: &str) -> crate::protocol::playlist4_external::Item {
        use crate::protocol::playlist4_external::{Item, ItemAttributes, RecommendationInfo};
        use protobuf::MessageField;
        let mut attributes = ItemAttributes::new();
        let mut info = RecommendationInfo::new();
        info.is_recommendation = Some(recommendation);
        attributes.recommendation_info = MessageField::some(info);
        attributes.format_attributes = vec![attr(DISTRIBUTION_KEY, dist)];
        let mut item = Item::new();
        item.uri = Some(uri.to_string());
        item.attributes = MessageField::some(attributes);
        item
    }

    fn content(items: Vec<crate::protocol::playlist4_external::Item>) -> SelectedListContent {
        use crate::protocol::playlist4_external::ListItems;
        use protobuf::MessageField;
        let mut list = ListItems::new();
        list.items = items;
        let mut c = SelectedListContent::new();
        c.contents = MessageField::some(list);
        c
    }

    #[test]
    fn the_real_encoding_is_a_d_wrapped_float() {
        assert_eq!(parse_distribution("d(22.0)"), Some(22.0));
        assert_eq!(parse_distribution("d(1.0)"), Some(1.0));
    }

    #[test]
    fn an_unrecognized_encoding_does_not_panic() {
        assert_eq!(parse_distribution("22"), None);
        assert_eq!(parse_distribution("d()"), None);
        assert_eq!(parse_distribution(""), None);
        assert_eq!(parse_distribution("d(nan)"), None);
    }

    #[test]
    fn items_come_out_ordered_by_distribution_not_reply_order() {
        let c = content(vec![
            item("spotify:track:b", true, "d(3.0)"),
            item("spotify:track:a", false, "d(1.0)"),
            item("spotify:track:c", false, "d(2.0)"),
        ]);
        let order = build_playback_order(&c);
        assert_eq!(
            order.iter().map(|t| t.uri.as_str()).collect::<Vec<_>>(),
            vec!["spotify:track:a", "spotify:track:c", "spotify:track:b"]
        );
    }

    #[test]
    fn each_slot_says_whether_it_is_a_recommendation() {
        let c = content(vec![item("spotify:track:a", false, "d(1.0)"), item("spotify:track:b", true, "d(2.0)")]);
        let order = build_playback_order(&c);
        assert!(!order[0].is_recommendation);
        assert!(order[1].is_recommendation);
    }

    #[test]
    fn a_real_75_item_reply_round_trips_to_a_1_to_75_order() {
        // The exact shape of the live reply: 50 originals, 25 recommendations,
        // distribution values scattered across the full range in reply order.
        let mut items = Vec::new();
        for i in 0..50 {
            items.push(item(&format!("spotify:track:orig{i}"), false, &format!("d({}.0)", (i * 7 + 3) % 75 + 1)));
        }
        for i in 0..25 {
            items.push(item(&format!("spotify:track:rec{i}"), true, &format!("d({}.0)", (i * 11 + 1) % 75 + 1)));
        }
        let c = content(items);
        let order = build_playback_order(&c);
        assert_eq!(order.len(), 75);
        let recs = order.iter().filter(|t| t.is_recommendation).count();
        assert_eq!(recs, 25);
    }

    #[test]
    fn an_item_with_no_usable_distribution_keeps_its_reply_position_relative_to_the_rest() {
        // Defensive: a future reply shape shouldn't drop or duplicate an item
        // just because one attribute is missing or unparseable.
        let c = content(vec![item("spotify:track:a", false, "d(5.0)"), item("spotify:track:b", false, "garbled")]);
        let order = build_playback_order(&c);
        assert_eq!(order.len(), 2);
        assert!(order.iter().any(|t| t.uri == "spotify:track:a"));
        assert!(order.iter().any(|t| t.uri == "spotify:track:b"));
    }

    #[test]
    fn an_empty_reply_orders_to_nothing() {
        assert!(build_playback_order(&SelectedListContent::new()).is_empty());
    }
}

#[cfg(test)]
mod resume_index_tests {
    use super::*;

    fn track(is_recommendation: bool) -> PlaybackItem {
        PlaybackItem { uri: "spotify:track:x".to_string(), is_recommendation }
    }

    #[test]
    fn continues_forward_when_a_recommendation_is_still_ahead() {
        let order = vec![track(false), track(false), track(true), track(false)];
        assert_eq!(resume_index(&order, 0), 1);
    }

    #[test]
    fn wraps_to_the_front_when_nothing_ahead_is_a_recommendation() {
        // The exact shape of the live bug: recommendations only in the front
        // of the order, current position already past all of them.
        let order = vec![track(true), track(false), track(true), track(false), track(false)];
        assert_eq!(resume_index(&order, 3), 0);
    }

    #[test]
    fn the_very_last_position_with_nothing_ahead_wraps_too() {
        let order = vec![track(true), track(false), track(false)];
        assert_eq!(resume_index(&order, 2), 0);
    }

    #[test]
    fn an_order_with_no_recommendations_at_all_never_wraps() {
        // No dead zone to escape -- continuing forward is already correct,
        // and wrapping here would just be a pointless jump.
        let order = vec![track(false), track(false), track(false)];
        assert_eq!(resume_index(&order, 0), 1);
    }

    #[test]
    fn a_recommendation_immediately_next_needs_no_wrap() {
        let order = vec![track(false), track(true)];
        assert_eq!(resume_index(&order, 0), 1);
    }
}

#[cfg(test)]
mod recommended_track_tests {
    use super::*;

    #[test]
    fn carries_the_uri_and_the_recommendation_marker() {
        let track = recommended_track("spotify:track:abc");
        assert_eq!(track.uri, "spotify:track:abc");
        assert_eq!(track.provider, PROVIDER_RECOMMENDATION);
    }

    #[test]
    fn two_calls_never_collide_on_uid() {
        assert_ne!(recommended_track("spotify:track:abc").uid, recommended_track("spotify:track:abc").uid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::lens_model::Lens;
    use crate::protocol::playlist4_external::{
        FormatListAttribute, Item, ItemAttributes, ListItems, RecommendationInfo, SelectedListContent,
    };
    use protobuf::{Message, MessageField};

    fn attr(key: &str, value: &str) -> FormatListAttribute {
        let mut a = FormatListAttribute::new();
        a.key = Some(key.to_string());
        a.value = Some(value.to_string());
        a
    }

    fn item(uri: &str, recommendation: Option<bool>, attrs: Vec<FormatListAttribute>) -> Item {
        let mut attributes = ItemAttributes::new();
        if let Some(is_recommendation) = recommendation {
            let mut info = RecommendationInfo::new();
            info.is_recommendation = Some(is_recommendation);
            attributes.recommendation_info = MessageField::some(info);
        }
        attributes.format_attributes = attrs;
        let mut item = Item::new();
        item.uri = Some(uri.to_string());
        item.attributes = MessageField::some(attributes);
        item
    }

    fn reply(items: Vec<Item>) -> SelectedListContent {
        let mut list = ListItems::new();
        list.items = items;
        let mut content = SelectedListContent::new();
        content.contents = MessageField::some(list);
        content
    }

    #[test]
    fn the_request_is_one_reset_signal_carrying_the_enhance_lens() {
        let request = enhance_reset_request();
        assert_eq!(request.emitted_signals.len(), 1);
        let signal = &request.emitted_signals[0];
        assert_eq!(signal.identifier, "reset");
        let lens = Lens::parse_from_bytes(&signal.data).expect("data is an encoded Lens");
        assert_eq!(lens.identifier, "enhance");
    }

    #[test]
    fn the_summary_counts_items_and_recommendations() {
        let content = reply(vec![
            item("spotify:track:a", Some(false), vec![]),
            item("spotify:track:b", Some(true), vec![]),
            item("spotify:track:c", None, vec![]),
            item("spotify:track:d", Some(true), vec![]),
        ]);
        let summary = summarize(&content);
        assert_eq!(summary.items, 4);
        assert_eq!(summary.recommendations, 2);
    }

    #[test]
    fn the_summary_collects_every_format_attribute_key_seen() {
        let content = reply(vec![
            item("spotify:track:a", None, vec![attr("shuffle.distribution", "3")]),
            item("spotify:track:b", Some(true), vec![attr("enhanced_recommendation", "true"), attr("shuffle.distribution", "4")]),
        ]);
        let keys: Vec<_> = summarize(&content).keys.into_iter().collect();
        assert_eq!(keys, vec!["enhanced_recommendation", "shuffle.distribution"]);
    }

    #[test]
    fn each_item_line_shows_position_uri_recommendation_flag_and_distribution() {
        let content = reply(vec![item("spotify:track:b", Some(true), vec![attr("shuffle.distribution", "4")])]);
        let lines = summarize(&content).lines;
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("0 "), "{}", lines[0]);
        assert!(lines[0].contains("spotify:track:b"), "{}", lines[0]);
        assert!(lines[0].contains("rec=true"), "{}", lines[0]);
        assert!(lines[0].contains("dist=4"), "{}", lines[0]);
    }

    #[test]
    fn an_item_with_no_attributes_or_distribution_still_gets_a_line() {
        let mut bare = Item::new();
        bare.uri = Some("spotify:track:z".to_string());
        let lines = summarize(&reply(vec![bare])).lines;
        assert!(lines[0].contains("rec=false"), "{}", lines[0]);
        assert!(lines[0].contains("dist=-"), "{}", lines[0]);
    }

    #[test]
    fn an_empty_reply_summarizes_to_nothing_without_panicking() {
        let summary = summarize(&SelectedListContent::new());
        assert_eq!(summary.items, 0);
        assert_eq!(summary.recommendations, 0);
        assert!(summary.keys.is_empty());
        assert!(summary.lines.is_empty());
    }
}
