//! OS media integration: publishes the current track to macOS Now Playing
//! (Control Center, the lock screen, apps like Boring Notch) and turns media
//! keys and Now Playing buttons into `MediaCommand`s for the main loop.
//!
//! macOS delivers media-key callbacks through the main thread's run loop, and
//! the TUI loop blocks whatever thread it runs on, so `run_on_main_thread`
//! moves the app to a worker thread and keeps the main thread pumping the run
//! loop. Other platforms get a no-op stub.

/// A transport request from outside the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCommand {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    /// Jump to an absolute position.
    SeekTo(u32),
    /// Move by a signed number of milliseconds.
    SeekBy(i64),
}

#[cfg(target_os = "macos")]
mod imp {
    use super::MediaCommand;
    use crate::player::SEEK_STEP_MS;
    use souvlaki::{
        MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition,
        PlatformConfig, SeekDirection,
    };
    use std::ffi::c_void;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    pub struct MediaSession {
        controls: MediaControls,
        rx: Receiver<MediaCommand>,
    }

    fn seek_delta(direction: SeekDirection, amount_ms: i64) -> i64 {
        match direction {
            SeekDirection::Forward => amount_ms,
            SeekDirection::Backward => -amount_ms,
        }
    }

    /// `None` for events the app has no use for (Stop, volume, Raise, Quit...).
    pub(super) fn map_event(event: MediaControlEvent) -> Option<MediaCommand> {
        Some(match event {
            MediaControlEvent::Play => MediaCommand::Play,
            MediaControlEvent::Pause => MediaCommand::Pause,
            MediaControlEvent::Toggle => MediaCommand::Toggle,
            MediaControlEvent::Next => MediaCommand::Next,
            MediaControlEvent::Previous => MediaCommand::Previous,
            MediaControlEvent::Seek(direction) => {
                MediaCommand::SeekBy(seek_delta(direction, SEEK_STEP_MS))
            }
            MediaControlEvent::SeekBy(direction, amount) => {
                MediaCommand::SeekBy(seek_delta(direction, amount.as_millis() as i64))
            }
            MediaControlEvent::SetPosition(MediaPosition(position)) => {
                MediaCommand::SeekTo(position.as_millis().min(u32::MAX as u128) as u32)
            }
            _ => return None,
        })
    }

    impl MediaSession {
        /// `None` when the system refuses media controls; the app runs fine
        /// without them.
        pub fn new() -> Option<Self> {
            let mut controls = MediaControls::new(PlatformConfig {
                dbus_name: "spot_tui",
                display_name: "spot-tui",
                hwnd: None,
            })
            .map_err(|e| log::warn!("media controls unavailable: {e}"))
            .ok()?;
            let (tx, rx) = mpsc::channel();
            controls
                .attach(move |event| {
                    log::info!("media: event {event:?}");
                    if let Some(command) = map_event(event) {
                        let _ = tx.send(command);
                    }
                })
                .map_err(|e| log::warn!("media controls: could not attach handlers: {e}"))
                .ok()?;
            log::info!("media: controls attached");
            Some(Self { controls, rx })
        }

        pub fn try_recv(&self) -> Option<MediaCommand> {
            self.rx.try_recv().ok()
        }

        /// Replaces the Now Playing entry. This also drops the elapsed time, so
        /// follow it with `set_playback`.
        pub fn set_track(
            &mut self,
            title: &str,
            artist: Option<&str>,
            album: Option<&str>,
            duration: Duration,
            cover_url: Option<&str>,
        ) {
            log::info!(
                "media: now playing \"{title}\" ({} ms)",
                duration.as_millis()
            );
            let _ = self.controls.set_metadata(MediaMetadata {
                title: Some(title),
                artist,
                album,
                duration: Some(duration),
                cover_url,
            });
        }

        pub fn set_playback(&mut self, playing: bool, progress: Duration) {
            log::info!("media: playing={playing} at {} ms", progress.as_millis());
            let progress = Some(MediaPosition(progress));
            let _ = self.controls.set_playback(if playing {
                MediaPlayback::Playing { progress }
            } else {
                MediaPlayback::Paused { progress }
            });
        }

        /// Nothing playing: removes the track from Now Playing.
        pub fn clear(&mut self) {
            let _ = self.controls.set_metadata(MediaMetadata::default());
            let _ = self.controls.set_playback(MediaPlayback::Stopped);
        }
    }

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    /// Registers the process as an (invisible) application. Without it macOS
    /// accepts Now Playing data but never pushes change notifications to other
    /// apps, so a reader like Boring Notch freezes on the first track it saw.
    // The objc 0.2 macros test a `cargo-clippy` feature that rustc's cfg check
    // does not know about.
    #[allow(unexpected_cfgs)]
    fn register_as_application() {
        use objc::runtime::Object;
        use objc::{class, msg_send, sel, sel_impl};
        // NSApplicationActivationPolicyAccessory: no Dock icon, no menu bar.
        const ACCESSORY: isize = 1;
        // SAFETY: plain AppKit calls, made on the main thread.
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let _: bool = msg_send![app, setActivationPolicy: ACCESSORY];
            let _: () = msg_send![app, finishLaunching];
        }
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopGetMain() -> *mut c_void;
        fn CFRunLoopStop(run_loop: *mut c_void);
        fn CFRunLoopRunInMode(
            mode: *const c_void,
            seconds: f64,
            return_after_source_handled: u8,
        ) -> i32;
    }

    /// Runs `work` on a worker thread and returns its result, while this
    /// thread (which must be the process's main thread) services the run loop
    /// so media-key callbacks are delivered. It also registers the process with
    /// AppKit; see `register_as_application`.
    pub fn run_on_main_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        register_as_application();
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = done.clone();
        let worker = std::thread::Builder::new()
            .name("app".into())
            .spawn(move || {
                // Set the flag even if `work` panics, or the main thread would
                // spin forever instead of reporting the panic.
                struct Finish(Arc<AtomicBool>);
                impl Drop for Finish {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::SeqCst);
                        // SAFETY: CFRunLoopStop may be called from any thread.
                        unsafe { CFRunLoopStop(CFRunLoopGetMain()) };
                    }
                }
                let _finish = Finish(worker_done);
                work()
            })
            .expect("could not start the app thread");
        // The timeout only bounds the lag if the stop above lands just before
        // this thread enters the run loop; normally the stop wakes it at once.
        while !done.load(Ordering::SeqCst) {
            // SAFETY: plain CoreFoundation calls on the main thread.
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.5, 0) };
        }
        match worker.join() {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn transport_events_map_one_to_one() {
            assert_eq!(map_event(MediaControlEvent::Play), Some(MediaCommand::Play));
            assert_eq!(
                map_event(MediaControlEvent::Pause),
                Some(MediaCommand::Pause)
            );
            assert_eq!(
                map_event(MediaControlEvent::Toggle),
                Some(MediaCommand::Toggle)
            );
            assert_eq!(map_event(MediaControlEvent::Next), Some(MediaCommand::Next));
            assert_eq!(
                map_event(MediaControlEvent::Previous),
                Some(MediaCommand::Previous)
            );
        }

        #[test]
        fn seeking_backward_is_negative() {
            assert_eq!(
                map_event(MediaControlEvent::SeekBy(
                    SeekDirection::Backward,
                    Duration::from_secs(15)
                )),
                Some(MediaCommand::SeekBy(-15_000))
            );
            assert_eq!(
                map_event(MediaControlEvent::Seek(SeekDirection::Forward)),
                Some(MediaCommand::SeekBy(SEEK_STEP_MS))
            );
        }

        #[test]
        fn set_position_becomes_an_absolute_seek() {
            assert_eq!(
                map_event(MediaControlEvent::SetPosition(MediaPosition(
                    Duration::from_millis(93_500)
                ))),
                Some(MediaCommand::SeekTo(93_500))
            );
        }

        #[test]
        fn events_the_app_ignores_map_to_none() {
            assert_eq!(map_event(MediaControlEvent::Stop), None);
            assert_eq!(map_event(MediaControlEvent::Quit), None);
            assert_eq!(map_event(MediaControlEvent::SetVolume(0.5)), None);
        }
    }
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
mod imp {
    use super::MediaCommand;
    use std::time::Duration;

    pub struct MediaSession;

    impl MediaSession {
        pub fn new() -> Option<Self> {
            None
        }
        pub fn try_recv(&self) -> Option<MediaCommand> {
            None
        }
        pub fn set_track(
            &mut self,
            _title: &str,
            _artist: Option<&str>,
            _album: Option<&str>,
            _duration: Duration,
            _cover_url: Option<&str>,
        ) {
        }
        pub fn set_playback(&mut self, _playing: bool, _progress: Duration) {}
        pub fn clear(&mut self) {}
    }

    pub fn run_on_main_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        work()
    }
}

pub use imp::{MediaSession, run_on_main_thread};
