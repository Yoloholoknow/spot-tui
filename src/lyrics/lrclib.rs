use super::cache::{read_cache, write_cache};
use super::lrc::parse_lrc;
use super::*;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LrcLibEntry {
    #[allow(dead_code)] // kept for schema completeness / debugging
    #[serde(rename = "trackName")]
    pub track_name: String,
    #[allow(dead_code)] // kept for schema completeness / future disambiguation
    #[serde(rename = "artistName")]
    pub artist_name: String,
    pub duration: f64,
    pub instrumental: bool,
    #[serde(rename = "plainLyrics")]
    pub plain_lyrics: Option<String>,
    #[serde(rename = "syncedLyrics")]
    pub synced_lyrics: Option<String>,
}

/// Picks the best `/api/search` candidate: must have synced lyrics, then
/// minimize distance from the known track duration (search results span
/// covers, remixes, etc. with mismatched runtimes).
pub fn best_search_candidate(
    candidates: &[LrcLibEntry],
    target_duration_secs: f64,
) -> Option<&LrcLibEntry> {
    candidates
        .iter()
        .filter(|c| c.synced_lyrics.is_some())
        .min_by(|a, b| {
            let da = (a.duration - target_duration_secs).abs();
            let db = (b.duration - target_duration_secs).abs();
            da.partial_cmp(&db).unwrap()
        })
}

pub struct LyricsClient {
    agent: ureq::Agent,
    base_url: String,
    cache_dir: std::path::PathBuf,
}

impl LyricsClient {
    pub fn new(cache_dir: std::path::PathBuf) -> Self {
        Self {
            agent: crate::http::agent(),
            base_url: "https://lrclib.net".to_string(),
            cache_dir,
        }
    }

    #[cfg(test)]
    fn with_base_url(cache_dir: std::path::PathBuf, base_url: &str) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_millis(500))
                .build(),
            base_url: base_url.to_string(),
            cache_dir,
        }
    }

    pub fn fetch(
        &self,
        track_uri: &str,
        artist: &str,
        title: &str,
        album: Option<&str>,
        duration_ms: u32,
    ) -> CachedLyrics {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        if let Some(cached) = read_cache(&self.cache_dir, track_uri, now) {
            return cached;
        }

        let result = self.fetch_from_network(artist, title, album, duration_ms);
        let _ = write_cache(&self.cache_dir, track_uri, &result, now);
        result
    }

    fn fetch_from_network(
        &self,
        artist: &str,
        title: &str,
        album: Option<&str>,
        duration_ms: u32,
    ) -> CachedLyrics {
        let duration_secs = duration_ms as f64 / 1000.0;

        let mut req = self
            .agent
            .get(&format!("{}/api/get", self.base_url))
            .query("artist_name", artist)
            .query("track_name", title)
            .query("duration", &duration_secs.to_string())
            .set("User-Agent", "spot-tui/0.1 (personal use)");
        if let Some(album) = album {
            req = req.query("album_name", album);
        }

        if let Ok(resp) = req.call()
            && let Ok(entry) = crate::http::read_json::<LrcLibEntry>(resp)
        {
            return classify(&entry);
        }

        // Fall back to fuzzy search when the exact match misses.
        let search = self
            .agent
            .get(&format!("{}/api/search", self.base_url))
            .query("artist_name", artist)
            .query("track_name", title)
            .set("User-Agent", "spot-tui/0.1 (personal use)")
            .call();

        if let Ok(resp) = search
            && let Ok(candidates) = crate::http::read_json::<Vec<LrcLibEntry>>(resp)
            && let Some(best) = best_search_candidate(&candidates, duration_secs)
        {
            return classify(best);
        }

        CachedLyrics::NotFound
    }
}

fn classify(entry: &LrcLibEntry) -> CachedLyrics {
    if entry.instrumental {
        return CachedLyrics::Instrumental;
    }
    if let Some(synced) = &entry.synced_lyrics {
        let lines = parse_lrc(synced)
            .into_iter()
            .map(|l| (l.timestamp.as_secs_f64(), l.text))
            .collect();
        return CachedLyrics::Synced {
            lines,
            credit: None,
            words: Vec::new(),
        };
    }
    if let Some(plain) = &entry.plain_lyrics {
        return CachedLyrics::Plain {
            text: plain.clone(),
        };
    }
    CachedLyrics::NotFound
}

#[cfg(test)]
mod client_tests {
    use super::*;
    use crate::lyrics::cache::NEGATIVE_CACHE_TTL_SECS;

    fn entry(track: &str, duration: f64, synced: Option<&str>) -> LrcLibEntry {
        LrcLibEntry {
            track_name: track.to_string(),
            artist_name: "A".to_string(),
            duration,
            instrumental: false,
            plain_lyrics: None,
            synced_lyrics: synced.map(|s| s.to_string()),
        }
    }

    #[test]
    fn best_search_candidate_prefers_synced_and_closest_duration() {
        let candidates = vec![
            entry("far, no sync", 500.0, None),
            entry("far but synced", 300.0, Some("[00:01.00] x")),
            entry("closest, synced", 240.0, Some("[00:01.00] y")),
        ];
        let best = best_search_candidate(&candidates, 238.0).unwrap();
        assert_eq!(best.track_name, "closest, synced");
    }

    #[test]
    fn best_search_candidate_returns_none_if_no_synced_available() {
        let candidates = vec![entry("a", 100.0, None), entry("b", 200.0, None)];
        assert!(best_search_candidate(&candidates, 150.0).is_none());
    }

    #[test]
    fn classify_prefers_synced_over_plain() {
        let e = LrcLibEntry {
            track_name: "t".into(),
            artist_name: "a".into(),
            duration: 10.0,
            instrumental: false,
            plain_lyrics: Some("plain text".into()),
            synced_lyrics: Some("[00:01.00] synced line".into()),
        };
        match classify(&e) {
            CachedLyrics::Synced { lines, .. } => {
                assert_eq!(lines, vec![(1.0, "synced line".to_string())]);
            }
            other => panic!("expected Synced, got {other:?}"),
        }
    }

    #[test]
    fn classify_falls_back_to_plain_when_no_sync() {
        let e = LrcLibEntry {
            track_name: "t".into(),
            artist_name: "a".into(),
            duration: 10.0,
            instrumental: false,
            plain_lyrics: Some("plain text".into()),
            synced_lyrics: None,
        };
        assert_eq!(
            classify(&e),
            CachedLyrics::Plain {
                text: "plain text".to_string()
            }
        );
    }

    #[test]
    fn classify_reports_instrumental_regardless_of_lyrics_fields() {
        let e = LrcLibEntry {
            track_name: "t".into(),
            artist_name: "a".into(),
            duration: 10.0,
            instrumental: true,
            plain_lyrics: None,
            synced_lyrics: None,
        };
        assert_eq!(classify(&e), CachedLyrics::Instrumental);
    }

    #[test]
    fn classify_reports_not_found_when_nothing_present() {
        let e = LrcLibEntry {
            track_name: "t".into(),
            artist_name: "a".into(),
            duration: 10.0,
            instrumental: false,
            plain_lyrics: None,
            synced_lyrics: None,
        };
        assert_eq!(classify(&e), CachedLyrics::NotFound);
    }

    #[test]
    fn cache_read_returns_none_when_file_absent() {
        let dir = std::env::temp_dir().join(format!("spot-tui-test-{}", std::process::id()));
        assert!(read_cache(&dir, "spotify:track:missing", 1_000_000).is_none());
    }

    #[test]
    fn cache_round_trips_write_then_read() {
        let dir =
            std::env::temp_dir().join(format!("spot-tui-test-roundtrip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let result = CachedLyrics::Synced {
            lines: vec![(1.0, "hi".to_string())],
            words: Vec::new(),
            credit: None,
        };
        write_cache(&dir, "spotify:track:x", &result, 1_000_000).unwrap();
        let read_back = read_cache(&dir, "spotify:track:x", 1_000_010).unwrap();
        assert_eq!(read_back, result);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn negative_cache_expires_after_ttl() {
        let dir = std::env::temp_dir().join(format!("spot-tui-test-ttl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_cache(&dir, "spotify:track:y", &CachedLyrics::NotFound, 1_000_000).unwrap();

        // Just under 7 days later: still cached as NotFound.
        assert_eq!(
            read_cache(
                &dir,
                "spotify:track:y",
                1_000_000 + NEGATIVE_CACHE_TTL_SECS - 1
            ),
            Some(CachedLyrics::NotFound)
        );
        // Just over 7 days later: treated as stale, allow refetch.
        assert_eq!(
            read_cache(
                &dir,
                "spotify:track:y",
                1_000_000 + NEGATIVE_CACHE_TTL_SECS + 1
            ),
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Hits the real lrclib.net API. Values verified by hand with curl
    /// before writing this test (see docs/phase0-calibration.md sibling
    /// note in the plan): Radiohead/Creep/Pablo Honey/238s returns synced
    /// lyrics starting "When you were here before".
    #[test]
    fn integration_fetches_real_synced_lyrics_from_lrclib() {
        let dir =
            std::env::temp_dir().join(format!("spot-tui-test-integration-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let client = LyricsClient::new(dir.clone());

        let result = client.fetch(
            "spotify:track:test-creep",
            "Radiohead",
            "Creep",
            Some("Pablo Honey"),
            238_000,
        );

        match result {
            CachedLyrics::Synced { lines, .. } => {
                assert!(!lines.is_empty());
                assert_eq!(lines[0].1, "When you were here before");
            }
            other => panic!("expected Synced lyrics, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn integration_second_fetch_is_cache_only_no_network() {
        let dir =
            std::env::temp_dir().join(format!("spot-tui-test-cachehit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let live = LyricsClient::new(dir.clone());
        let first = live.fetch(
            "spotify:track:test-creep-2",
            "Radiohead",
            "Creep",
            Some("Pablo Honey"),
            238_000,
        );
        assert!(matches!(first, CachedLyrics::Synced { .. }));

        // Bogus, unroutable base URL: if this call reached the network at
        // all it would fail/timeout instead of returning Synced.
        let cached_only = LyricsClient::with_base_url(dir.clone(), "http://127.0.0.1:1");
        let second = cached_only.fetch(
            "spotify:track:test-creep-2",
            "Radiohead",
            "Creep",
            Some("Pablo Honey"),
            238_000,
        );
        assert_eq!(first, second);
        std::fs::remove_dir_all(&dir).ok();
    }
}
