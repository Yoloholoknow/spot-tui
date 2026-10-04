use super::*;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    fetched_at_unix: u64,
    result: CachedLyrics,
}

pub(super) const NEGATIVE_CACHE_TTL_SECS: u64 = 7 * 24 * 60 * 60;

fn cache_path(cache_dir: &std::path::Path, track_uri: &str) -> std::path::PathBuf {
    use sha1::{Digest, Sha1};
    let hash = format!("{:x}", Sha1::digest(track_uri.as_bytes()));
    cache_dir.join(format!("{hash}.json"))
}

pub(super) fn read_cache(cache_dir: &std::path::Path, track_uri: &str, now_unix: u64) -> Option<CachedLyrics> {
    let path = cache_path(cache_dir, track_uri);
    let data = std::fs::read_to_string(path).ok()?;
    let entry: CacheEntry = serde_json::from_str(&data).ok()?;
    if matches!(entry.result, CachedLyrics::NotFound)
        && now_unix.saturating_sub(entry.fetched_at_unix) > NEGATIVE_CACHE_TTL_SECS
    {
        return None; // stale negative cache entry, allow refetch
    }
    Some(entry.result)
}

pub(super) fn write_cache(
    cache_dir: &std::path::Path,
    track_uri: &str,
    result: &CachedLyrics,
    now_unix: u64,
) -> std::io::Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let entry = CacheEntry {
        fetched_at_unix: now_unix,
        result: result.clone(),
    };
    let path = cache_path(cache_dir, track_uri);
    std::fs::write(path, serde_json::to_string(&entry).unwrap())
}

/// A cached *synced* result, or `None` for anything else. The Spicy path
/// only accepts synced hits on purpose: an earlier lrclib `Plain` or
/// negative `NotFound` entry for the same track is exactly the case Spicy
/// exists to improve, so it must not count as "already answered".
pub fn cached_synced(cache_dir: &std::path::Path, track_uri: &str, now_unix: u64) -> Option<CachedLyrics> {
    read_cache(cache_dir, track_uri, now_unix).filter(|cached| matches!(cached, CachedLyrics::Synced { .. }))
}

/// Stores a synced result, overwriting any weaker entry for the track.
pub fn store_synced(
    cache_dir: &std::path::Path,
    track_uri: &str,
    lines: Vec<(f64, String)>,
    words: Vec<Vec<WordSeg>>,
    credit: Option<String>,
    now_unix: u64,
) -> std::io::Result<()> {
    write_cache(cache_dir, track_uri, &CachedLyrics::Synced { lines, words, credit }, now_unix)
}

/// The cache key for a Spicy Lyrics result. Its own namespace, so entries
/// written before word timing existed (under the plain track uri) are never
/// served without words, and a lrclib entry can't be mistaken for one.
pub fn spicy_cache_key(track_uri: &str) -> String {
    format!("spicy:{track_uri}")
}

#[cfg(test)]
mod spicy_cache_tests {
    use super::*;

    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("spot-tui-test-spicy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    const URI: &str = "spotify:track:7knSngLX3gWTH8ch4Y5aGr";

    #[test]
    fn cache_files_written_before_credits_existed_still_load() {
        let old = r#"{"kind":"Synced","lines":[[1.0,"a"]]}"#;
        let parsed: CachedLyrics = serde_json::from_str(old).unwrap();
        assert_eq!(parsed, CachedLyrics::Synced { lines: vec![(1.0, "a".to_string())], credit: None, words: Vec::new() });
    }

    #[test]
    fn a_credit_survives_a_round_trip() {
        let original = CachedLyrics::Synced {
            lines: vec![(1.0, "a".to_string())],
            words: Vec::new(),
            credit: Some("Apple Music via Spicy Lyrics".to_string()),
        };
        let back: CachedLyrics = serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn no_credit_is_left_out_of_the_file_entirely() {
        let json = serde_json::to_string(&CachedLyrics::Synced { lines: vec![], credit: None, words: Vec::new() }).unwrap();
        assert!(!json.contains("credit"), "{json}");
    }

    #[test]
    fn only_a_synced_entry_counts_as_a_hit_for_the_spicy_path() {
        // A stale lrclib Plain or NotFound for the same track must not stop
        // Spicy being asked -- that is exactly the track it exists for.
        let dir = fresh_dir("hits");
        for (i, weaker) in [
            CachedLyrics::Plain { text: "x".to_string() },
            CachedLyrics::NotFound,
            CachedLyrics::Instrumental,
        ]
        .iter()
        .enumerate()
        {
            let uri = format!("{URI}{i}");
            write_cache(&dir, &uri, weaker, 100).unwrap();
            assert_eq!(cached_synced(&dir, &uri, 101), None, "{weaker:?}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_synced_entry_is_a_hit_and_keeps_its_credit() {
        let dir = fresh_dir("synced");
        store_synced(&dir, URI, vec![(2.5, "line".to_string())], Vec::new(), Some("credit".to_string()), 100)
            .unwrap();
        assert_eq!(
            cached_synced(&dir, URI, 101),
            Some(CachedLyrics::Synced {
                lines: vec![(2.5, "line".to_string())],
                words: Vec::new(),
                credit: Some("credit".to_string())
            })
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn seg(text: &str, start: f64, end: f64) -> WordSeg {
        WordSeg { text: text.to_string(), start, end }
    }

    #[test]
    fn word_timing_survives_a_round_trip() {
        let original = CachedLyrics::Synced {
            lines: vec![(1.0, "hi there".to_string())],
            words: vec![vec![seg("hi ", 1.0, 1.4), seg("there", 1.4, 2.0)]],
            credit: None,
        };
        let back: CachedLyrics = serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn no_word_timing_is_left_out_of_the_file_so_other_sources_are_unchanged() {
        let json = serde_json::to_string(&CachedLyrics::Synced {
            lines: vec![(1.0, "a".to_string())],
            words: Vec::new(),
            credit: None,
        })
        .unwrap();
        assert!(!json.contains("words"), "{json}");
    }

    #[test]
    fn cache_files_written_before_word_timing_existed_still_load() {
        let old = r#"{"kind":"Synced","lines":[[1.0,"a"]],"credit":"Apple Music via Spicy Lyrics"}"#;
        let parsed: CachedLyrics = serde_json::from_str(old).unwrap();
        assert_eq!(
            parsed,
            CachedLyrics::Synced {
                lines: vec![(1.0, "a".to_string())],
                words: Vec::new(),
                credit: Some("Apple Music via Spicy Lyrics".to_string()),
            }
        );
    }

    #[test]
    fn spicy_results_live_under_their_own_key() {
        // Older line-level-only entries already on disk sit under the
        // plain track uri. A different key means they are ignored and simply
        // refreshed -- "missing words" can't be read as "stale", because an
        // Apple Music line sync legitimately has none.
        assert_eq!(spicy_cache_key(URI), format!("spicy:{URI}"));
        let dir = fresh_dir("keys");
        store_synced(&dir, URI, vec![(1.0, "old line-level".to_string())], Vec::new(), None, 100).unwrap();
        assert_eq!(cached_synced(&dir, &spicy_cache_key(URI), 101), None);
        store_synced(&dir, &spicy_cache_key(URI), vec![(1.0, "new".to_string())], Vec::new(), None, 102).unwrap();
        assert!(cached_synced(&dir, &spicy_cache_key(URI), 103).is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_synced_result_upgrades_an_earlier_plain_entry() {
        let dir = fresh_dir("upgrade");
        write_cache(&dir, URI, &CachedLyrics::Plain { text: "unsynced".to_string() }, 100).unwrap();
        assert_eq!(cached_synced(&dir, URI, 101), None);
        store_synced(&dir, URI, vec![(1.0, "now synced".to_string())], Vec::new(), None, 102).unwrap();
        assert!(matches!(cached_synced(&dir, URI, 103), Some(CachedLyrics::Synced { .. })));
        let _ = std::fs::remove_dir_all(dir);
    }
}

