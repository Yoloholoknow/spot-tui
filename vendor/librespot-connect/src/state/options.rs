use crate::{
    core::Error,
    protocol::player::ContextPlayerOptions,
    state::{
        ConnectState, StateError,
        context::{ContextType, ResetContext},
        metadata::Metadata,
    },
};
use protobuf::MessageField;
use rand::Rng;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

const CONTEXT_ENHANCEMENT: &str = "context_enhancement";
const RECOMMENDATION: &str = "RECOMMENDATION";
const NO_ENHANCEMENT: &str = "NONE";

static SMART_SHUFFLE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether the published player options currently say "smart shuffle":
/// shuffle on, with `modes.context_enhancement` set to `RECOMMENDATION`
/// (captured live from the official app). Read by spot-tui's playbar; there
/// is no player event for it, and adding one would mean vendoring
/// `librespot-playback` too.
pub fn smart_shuffle_active() -> bool {
    SMART_SHUFFLE_ACTIVE.load(Ordering::Relaxed)
}

fn is_smart_shuffle(shuffling: bool, modes: &HashMap<String, String>) -> bool {
    shuffling && modes.get(CONTEXT_ENHANCEMENT).map(String::as_str) == Some(RECOMMENDATION)
}

/// Turning shuffle off ends smart shuffle too (the official app sends
/// `NONE` with every shuffle-off). Only rewrites a key that's already there,
/// so a state that never had smart shuffle isn't given one.
fn clear_enhancement(modes: &mut HashMap<String, String>) {
    if let Some(value) = modes.get_mut(CONTEXT_ENHANCEMENT) {
        *value = NO_ENHANCEMENT.to_string();
    }
}

#[derive(Default, Debug)]
pub(crate) struct ShuffleState {
    pub seed: u64,
    pub initial_track: String,
}

impl ConnectState {
    fn add_options_if_empty(&mut self) {
        if self.player().options.is_none() {
            self.player_mut().options = MessageField::some(ContextPlayerOptions::new())
        }
    }

    pub fn set_repeat_context(&mut self, repeat: bool) {
        self.add_options_if_empty();
        if let Some(options) = self.player_mut().options.as_mut() {
            options.repeating_context = repeat;
        }
    }

    pub fn set_repeat_track(&mut self, repeat: bool) {
        self.add_options_if_empty();
        if let Some(options) = self.player_mut().options.as_mut() {
            options.repeating_track = repeat;
        }
    }

    pub fn set_shuffle(&mut self, shuffle: bool) {
        self.add_options_if_empty();
        if let Some(options) = self.player_mut().options.as_mut() {
            options.shuffling_context = shuffle;
            if !shuffle {
                clear_enhancement(&mut options.modes);
            }
        }
        self.publish_smart_shuffle();
    }

    /// Mirrors the current options into [`smart_shuffle_active`].
    pub(crate) fn publish_smart_shuffle(&self) {
        let options = &self.player().options;
        SMART_SHUFFLE_ACTIVE.store(
            is_smart_shuffle(options.shuffling_context, &options.modes),
            Ordering::Relaxed,
        );
    }

    /// Merges `modes` (e.g. smart shuffle's `context_enhancement`) into the
    /// published player options. Merged, not replaced: the transfer payload
    /// also carries unrelated keys such as `jam` that must survive.
    pub fn set_modes(&mut self, modes: HashMap<String, String>) {
        self.add_options_if_empty();
        if let Some(options) = self.player_mut().options.as_mut() {
            options.modes.extend(modes);
        }
        self.publish_smart_shuffle();
    }

    /// Sets `context_enhancement` to `RECOMMENDATION` (smart shuffle), leaving
    /// every other mode alone.
    pub fn set_smart_shuffle_mode(&mut self) {
        self.set_modes(HashMap::from([(
            CONTEXT_ENHANCEMENT.to_string(),
            RECOMMENDATION.to_string(),
        )]));
    }

    pub fn reset_options(&mut self) {
        self.set_shuffle(false);
        self.set_repeat_track(false);
        self.set_repeat_context(false);
    }

    fn validate_shuffle_allowed(&self) -> Result<(), Error> {
        if let Some(reason) = self
            .player()
            .restrictions
            .disallow_toggling_shuffle_reasons
            .first()
        {
            Err(StateError::CurrentlyDisallowed {
                action: "shuffle",
                reason: reason.clone(),
            })?
        } else {
            Ok(())
        }
    }

    pub fn shuffle_restore(&mut self, shuffle_state: ShuffleState) -> Result<(), Error> {
        self.validate_shuffle_allowed()?;

        self.shuffle(shuffle_state.seed, &shuffle_state.initial_track)
    }

    pub fn shuffle_new(&mut self) -> Result<(), Error> {
        self.validate_shuffle_allowed()?;

        let new_seed = rand::rng().random_range(100_000_000_000..1_000_000_000_000);
        let current_track = self.current_track(|t| t.uri.clone());

        self.shuffle(new_seed, &current_track)
    }

    fn shuffle(&mut self, seed: u64, initial_track: &str) -> Result<(), Error> {
        self.clear_prev_track();
        self.clear_next_tracks();

        self.reset_context(ResetContext::DefaultIndex);

        let ctx = self.get_context_mut(ContextType::Default)?;
        ctx.tracks
            .shuffle_with_seed(seed, |f| f.uri == initial_track);

        ctx.set_initial_track(initial_track);
        ctx.set_shuffle_seed(seed);

        self.fill_up_next_tracks()?;

        Ok(())
    }

    pub fn shuffling_context(&self) -> bool {
        self.player().options.shuffling_context
    }

    pub fn repeat_context(&self) -> bool {
        self.player().options.repeating_context
    }

    pub fn repeat_track(&self) -> bool {
        self.player().options.repeating_track
    }
}

#[cfg(test)]
mod smart_shuffle_tests {
    use super::*;

    fn modes(value: &str) -> HashMap<String, String> {
        HashMap::from([(CONTEXT_ENHANCEMENT.to_string(), value.to_string())])
    }

    #[test]
    fn recommendation_with_shuffle_on_is_smart_shuffle() {
        assert!(is_smart_shuffle(true, &modes("RECOMMENDATION")));
    }

    #[test]
    fn plain_shuffle_is_not_smart() {
        assert!(!is_smart_shuffle(true, &modes("NONE")));
        assert!(!is_smart_shuffle(true, &HashMap::new()));
    }

    #[test]
    fn a_stale_recommendation_mode_with_shuffle_off_is_not_smart() {
        assert!(!is_smart_shuffle(false, &modes("RECOMMENDATION")));
    }

    #[test]
    fn shuffle_off_resets_the_enhancement_but_keeps_unrelated_modes() {
        let mut m = modes("RECOMMENDATION");
        m.insert("jam".to_string(), "off".to_string());
        clear_enhancement(&mut m);
        assert_eq!(m.get(CONTEXT_ENHANCEMENT).map(String::as_str), Some("NONE"));
        assert_eq!(m.get("jam").map(String::as_str), Some("off"));
    }

    #[test]
    fn shuffle_off_does_not_invent_an_enhancement_key() {
        let mut m = HashMap::new();
        clear_enhancement(&mut m);
        assert!(m.is_empty());
    }
}
