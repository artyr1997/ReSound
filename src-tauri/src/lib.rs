//! RePlay: a quiet music player in the ReNote style.
//! Rust scans the music folder, keeps the library and playlists in SQLite and
//! plays the audio, or a radio station (radio.rs); the Svelte UI talks to it through
//! these commands and the `player` / `scan` events.

mod db;
#[cfg(target_os = "macos")]
mod media;
mod player;
mod radio;
mod scan;

use db::{Db, Playlist, Track};
use player::{Audio, QueueItem, Queue, Repeat};
use radio::Station;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

struct AppState {
    db: Mutex<Db>,
    db_path: PathBuf,
    queue: Mutex<Queue>,
    audio: Audio,
    scanning: Mutex<bool>,
    radio: Mutex<Radio>,
}

/// The station on (instead of a song), and the list it was picked from: PREV / NEXT go along it.
#[derive(Default)]
struct Radio {
    station: Option<Station>,
    list: Vec<Station>,
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Everything the player bar shows.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Status {
    track: Option<Track>,
    /// a station instead of a song
    station: Option<Station>,
    /// the song on air, as the station names it
    on_air: Option<String>,
    playing: bool,
    position_ms: u64,
    shuffle: bool,
    repeat: Repeat,
    error: Option<String>,
}

fn status(state: &AppState) -> CmdResult<Status> {
    let audio = state.audio.status.lock().map_err(err)?.clone();
    let station = state.radio.lock().map_err(err)?.station.clone();
    let queue = state.queue.lock().map_err(err)?;
    let track = match queue.current() {
        Some(item) if station.is_none() => state.db.lock().map_err(err)?.track(item.id).ok(),
        _ => None,
    };
    Ok(Status {
        track,
        on_air: if station.is_some() { audio.on_air } else { None },
        station,
        playing: audio.playing,
        position_ms: audio.position_ms,
        shuffle: queue.shuffle,
        repeat: queue.repeat,
        error: audio.error,
    })
}

/// Starts whatever the queue now points at (or stops), and tells the UI.
fn play_current(app: &AppHandle, item: Option<QueueItem>) {
    let state = app.state::<AppState>();
    // a song: the radio is off
    if let Ok(mut radio) = state.radio.lock() {
        radio.station = None;
    }
    match item {
        Some(item) => state.audio.load(item.path, true),
        None => state.audio.stop(),
    }
    // let the audio thread pick it up before reporting
    std::thread::sleep(Duration::from_millis(30));
    if let Ok(s) = status(&state) {
        let _ = app.emit("player", s);
    }
}

fn queue_items(state: &AppState, ids: &[i64]) -> CmdResult<Vec<QueueItem>> {
    let db = state.db.lock().map_err(err)?;
    ids.iter().map(|&id| db.track(id).map(|t| QueueItem { id, path: PathBuf::from(t.path) })).collect()
}

/// Files that open RePlay arrive before setup has opened the library: they wait here for it.
static WAITING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Music opened from outside (ReFile, or Open With in Finder): the folder it's in becomes the queue,
/// starting at it (several files opened at once: just those).
fn open_files(app: &AppHandle, opened: Vec<PathBuf>) {
    if app.try_state::<AppState>().is_none() {
        if let Ok(mut waiting) = WAITING.lock() {
            waiting.extend(opened);
        }
        return;
    }
    let (videos, opened): (Vec<PathBuf>, Vec<PathBuf>) = opened.into_iter().partition(|p| scan::is_video(p));
    if !videos.is_empty() {
        open_videos(app, videos);
    }
    let opened: Vec<PathBuf> = opened.into_iter().filter(|p| scan::is_audio(p)).collect();
    if opened.is_empty() {
        return;
    }
    let first = opened[0].clone();
    let (paths, start) = with_neighbours(opened, scan::is_audio);

    let state = app.state::<AppState>();
    let ids = match state.db.lock().map_err(err).and_then(|db| scan::add(&db, &paths)) {
        Ok(ids) => ids,
        Err(e) => return eprintln!("RePlay: couldn't open {}: {e}", first.display()),
    };
    let Ok(items) = queue_items(&state, &ids) else { return };
    let item = state.queue.lock().ok().and_then(|mut q| {
        q.set(items, start);
        q.current().cloned()
    });
    play_current(app, item);
    // the library may have new tracks
    let _ = app.emit("scan", serde_json::json!({ "finished": true }));
}

/// The folder's files that `keep` takes, sorted by name, with `first` among them (several opened: just those).
fn with_neighbours(opened: Vec<PathBuf>, keep: fn(&std::path::Path) -> bool) -> (Vec<PathBuf>, usize) {
    let first = opened[0].clone();
    let mut paths = if opened.len() > 1 {
        opened
    } else {
        first
            .parent()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .map(|dir| {
                dir.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.is_file() && keep(p))
                    .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
                    .collect()
            })
            .unwrap_or_else(|| vec![first.clone()])
    };
    paths.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    let start = paths.iter().position(|p| *p == first).unwrap_or(0);
    (paths, start)
}

/// A video to show: the window takes it when it asks (take_video), told by a "video" event.
#[derive(Clone, Serialize)]
struct VideoOpen {
    paths: Vec<String>,
    start: usize,
}
static VIDEO: Mutex<Option<VideoOpen>> = Mutex::new(None);

/// Videos opened (ReFile, Open With, dropped on the window, O): shown over the library, the folder's
/// other videos next; the music pauses.
fn open_videos(app: &AppHandle, opened: Vec<PathBuf>) {
    let (paths, start) = with_neighbours(opened, scan::is_video);
    if let Ok(mut video) = VIDEO.lock() {
        *video = Some(VideoOpen { paths: paths.iter().map(|p| p.to_string_lossy().into_owned()).collect(), start });
    }
    if let Some(state) = app.try_state::<AppState>() {
        if state.audio.status.lock().is_ok_and(|s| s.playing) {
            state.audio.pause();
        }
    }
    // shown in the window in front (or the first): only that one takes it
    let windows = app.webview_windows();
    let front = windows.values().find(|w| w.is_focused().unwrap_or(false)).or_else(|| windows.get("main")).or_else(|| windows.values().next());
    if let Some(window) = front {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        let _ = app.emit_to(window.label(), "video", ());
    }
}

#[tauri::command]
fn take_video() -> Option<VideoOpen> {
    VIDEO.lock().ok().and_then(|mut v| v.take())
}

/// Files dropped on the window, or picked with O: videos are shown, music is played.
#[tauri::command]
fn open_paths(app: AppHandle, paths: Vec<String>) {
    open_files(&app, paths.into_iter().map(PathBuf::from).collect());
}

// ── library ──

#[tauri::command]
fn music_folder(state: State<AppState>) -> CmdResult<Option<String>> {
    state.db.lock().map_err(err)?.setting("folder")
}

#[tauri::command]
fn set_music_folder(app: AppHandle, state: State<AppState>, folder: String) -> CmdResult<()> {
    state.db.lock().map_err(err)?.set_setting("folder", &folder)?;
    rescan(app, state)
}

/// Scans the music folder in the background; progress and the end come as `scan` events.
#[tauri::command]
fn rescan(app: AppHandle, state: State<AppState>) -> CmdResult<()> {
    let Some(folder) = state.db.lock().map_err(err)?.setting("folder")? else {
        return Err("Choose a music folder first.".into());
    };
    {
        let mut busy = state.scanning.lock().map_err(err)?;
        if *busy {
            return Ok(());
        }
        *busy = true;
    }
    let db_path = state.db_path.clone();
    std::thread::spawn(move || {
        // a connection of its own, so the library stays usable while scanning
        let result = Db::open(&db_path).and_then(|db| {
            scan::scan(&db, &PathBuf::from(&folder), |done, total| {
                let _ = app.emit("scan", serde_json::json!({ "done": done, "total": total, "finished": false }));
            })
        });
        if let Ok(mut busy) = app.state::<AppState>().scanning.lock() {
            *busy = false;
        }
        let payload = match result {
            Ok(r) => serde_json::json!({ "finished": true, "result": r }),
            Err(e) => serde_json::json!({ "finished": true, "error": e }),
        };
        let _ = app.emit("scan", payload);
    });
    Ok(())
}

#[tauri::command]
fn tracks(state: State<AppState>) -> CmdResult<Vec<Track>> {
    state.db.lock().map_err(err)?.tracks()
}

// ── playlists ──

#[tauri::command]
fn playlists(state: State<AppState>) -> CmdResult<Vec<Playlist>> {
    state.db.lock().map_err(err)?.playlists()
}

#[tauri::command]
fn playlist_tracks(state: State<AppState>, id: i64) -> CmdResult<Vec<Track>> {
    state.db.lock().map_err(err)?.playlist_tracks(id)
}

#[tauri::command]
fn create_playlist(state: State<AppState>, name: String, track_ids: Vec<i64>) -> CmdResult<i64> {
    let db = state.db.lock().map_err(err)?;
    let id = db.create_playlist(&name, now())?;
    db.add_to_playlist(id, &track_ids)?;
    Ok(id)
}

#[tauri::command]
fn rename_playlist(state: State<AppState>, id: i64, name: String) -> CmdResult<()> {
    state.db.lock().map_err(err)?.rename_playlist(id, &name)
}

#[tauri::command]
fn delete_playlist(state: State<AppState>, id: i64) -> CmdResult<()> {
    state.db.lock().map_err(err)?.delete_playlist(id)
}

#[tauri::command]
fn add_to_playlist(state: State<AppState>, id: i64, track_ids: Vec<i64>) -> CmdResult<()> {
    state.db.lock().map_err(err)?.add_to_playlist(id, &track_ids)
}

#[tauri::command]
fn remove_from_playlist(state: State<AppState>, id: i64, index: usize) -> CmdResult<()> {
    state.db.lock().map_err(err)?.remove_from_playlist(id, index)
}

// ── playback ──

/// Plays `track_ids` as the new queue, starting at `start`.
#[tauri::command]
fn play(app: AppHandle, state: State<AppState>, track_ids: Vec<i64>, start: usize) -> CmdResult<()> {
    let items = queue_items(&state, &track_ids)?;
    let item = {
        let mut q = state.queue.lock().map_err(err)?;
        q.set(items, start);
        q.current().cloned()
    };
    play_current(&app, item);
    Ok(())
}

#[tauri::command]
fn toggle_play(app: AppHandle, state: State<AppState>) -> CmdResult<()> {
    let audio = state.audio.status.lock().map_err(err)?.clone();
    if state.radio.lock().map_err(err)?.station.is_some() {
        // a station: off, or tuned in again
        if audio.playing { state.audio.pause() } else { state.audio.play() }
        return Ok(());
    }
    if audio.playing {
        state.audio.pause();
    } else if audio.position_ms == 0 && !state.queue.lock().map_err(err)?.is_empty() {
        // stopped at the end (or never started): start the current track again
        let item = state.queue.lock().map_err(err)?.current().cloned();
        play_current(&app, item);
        return Ok(());
    } else {
        state.audio.play();
    }
    Ok(())
}

#[tauri::command]
fn next(app: AppHandle, state: State<AppState>) -> CmdResult<()> {
    if state.radio.lock().map_err(err)?.station.is_some() {
        return turn_dial(&app, &state, 1);
    }
    let item = state.queue.lock().map_err(err)?.advance(false).cloned();
    if item.is_some() {
        play_current(&app, item);
    }
    Ok(())
}

/// Like Music: more than 3 s in → back to the start of the track, otherwise the previous one.
#[tauri::command]
fn previous(app: AppHandle, state: State<AppState>) -> CmdResult<()> {
    if state.radio.lock().map_err(err)?.station.is_some() {
        return turn_dial(&app, &state, -1);
    }
    let position = state.audio.status.lock().map_err(err)?.position_ms;
    if position > 3000 {
        state.audio.seek(Duration::ZERO);
        return Ok(());
    }
    let item = state.queue.lock().map_err(err)?.back().cloned();
    play_current(&app, item);
    Ok(())
}

#[tauri::command]
fn seek(state: State<AppState>, position_ms: u64) -> CmdResult<()> {
    state.audio.seek(Duration::from_millis(position_ms));
    Ok(())
}

#[tauri::command]
fn set_shuffle(state: State<AppState>, on: bool) -> CmdResult<()> {
    state.queue.lock().map_err(err)?.set_shuffle(on);
    Ok(())
}

#[tauri::command]
fn cycle_repeat(state: State<AppState>) -> CmdResult<Repeat> {
    Ok(state.queue.lock().map_err(err)?.cycle_repeat())
}

#[tauri::command]
fn play_next(state: State<AppState>, track_ids: Vec<i64>) -> CmdResult<()> {
    let items = queue_items(&state, &track_ids)?;
    state.queue.lock().map_err(err)?.play_next(items);
    Ok(())
}

#[tauri::command]
fn add_to_queue(state: State<AppState>, track_ids: Vec<i64>) -> CmdResult<()> {
    let items = queue_items(&state, &track_ids)?;
    state.queue.lock().map_err(err)?.append(items);
    Ok(())
}

#[tauri::command]
fn player_status(state: State<AppState>) -> CmdResult<Status> {
    status(&state)
}

/// The tracks still to come, in play order.
#[tauri::command]
fn upcoming(state: State<AppState>) -> CmdResult<Vec<Track>> {
    let ids = state.queue.lock().map_err(err)?.upcoming();
    let db = state.db.lock().map_err(err)?;
    Ok(ids.into_iter().filter_map(|id| db.track(id).ok()).collect())
}

// ── radio ──

#[tauri::command]
fn radio_genres() -> Vec<&'static str> {
    radio::GENRES.iter().map(|g| g.name).collect()
}

/// A genre's stations (asked of Radio Browser, off the main thread).
#[tauri::command]
async fn radio_stations(genre: String) -> CmdResult<Vec<Station>> {
    tauri::async_runtime::spawn_blocking(move || radio::stations(&genre)).await.map_err(err)?
}

#[tauri::command]
async fn radio_search(query: String) -> CmdResult<Vec<Station>> {
    tauri::async_runtime::spawn_blocking(move || radio::search(&query)).await.map_err(err)?
}

fn favorites(db: &Db) -> Vec<Station> {
    db.setting("radio_favorites").ok().flatten().and_then(|json| serde_json::from_str(&json).ok()).unwrap_or_default()
}

/// The stations kept (newest first), here in the library: there without the network too.
#[tauri::command]
fn radio_favorites(state: State<AppState>) -> CmdResult<Vec<Station>> {
    let db = state.db.lock().map_err(err)?;
    Ok(favorites(&db))
}

#[tauri::command]
fn set_radio_favorite(state: State<AppState>, station: Station, on: bool) -> CmdResult<()> {
    let db = state.db.lock().map_err(err)?;
    let mut kept: Vec<Station> = favorites(&db).into_iter().filter(|s| s.uuid != station.uuid).collect();
    if on {
        kept.insert(0, station);
    }
    db.set_setting("radio_favorites", &serde_json::to_string(&kept).map_err(err)?)
}

/// Plays `station`; `list` is where it was picked from, for PREV / NEXT.
#[tauri::command]
fn play_station(app: AppHandle, state: State<AppState>, station: Station, list: Vec<Station>) -> CmdResult<()> {
    tune_in(&app, &state, station, Some(list))
}

fn tune_in(app: &AppHandle, state: &AppState, station: Station, list: Option<Vec<Station>>) -> CmdResult<()> {
    radio::count_click(&station.uuid);
    state.audio.stream(station.url.clone());
    {
        let mut radio = state.radio.lock().map_err(err)?;
        if let Some(list) = list {
            radio.list = list;
        }
        radio.station = Some(station);
    }
    std::thread::sleep(Duration::from_millis(30));
    if let Ok(s) = status(state) {
        let _ = app.emit("player", s);
    }
    Ok(())
}

/// PREV / NEXT on the radio: the station before or after this one in its list.
fn turn_dial(app: &AppHandle, state: &AppState, step: isize) -> CmdResult<()> {
    let next = {
        let radio = state.radio.lock().map_err(err)?;
        let Some(on) = &radio.station else { return Ok(()) };
        let count = radio.list.len() as isize;
        if count == 0 {
            return Ok(());
        }
        let at = radio.list.iter().position(|s| s.uuid == on.uuid).map(|i| i as isize).unwrap_or(-step.min(0));
        radio.list[((at + step) % count + count) as usize % count as usize].clone()
    };
    tune_in(app, state, next, None)
}

#[tauri::command]
fn reveal_in_finder(path: String) -> CmdResult<()> {
    std::process::Command::new("open").args(["-R", &path]).status().map(|_| ()).map_err(err)
}

/// No title bar at all (decorations are off in tauri.conf.json): macOS rims every titled window with
/// a light line that can't be turned off. The window is clear instead, its page is rounded here like a
/// window's corners, and the UI draws its own close / minimise / zoom buttons. No shadow either
/// (tauri.conf.json).
#[cfg(target_os = "macos")]
fn round_window(window: &tauri::WebviewWindow) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    let _ = window.with_webview(|platform| {
        let view = platform.inner() as *mut AnyObject;
        if view.is_null() {
            return;
        }
        // SAFETY: `inner()` is the live WKWebView (an NSView); this runs on the main thread.
        unsafe {
            let _: () = msg_send![view, setWantsLayer: true];
            let layer: *mut AnyObject = msg_send![view, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, setCornerRadius: 12.0f64];
                let _: () = msg_send![layer, setMasksToBounds: true];
            }
        }
    });
}

/// The window starts hidden (tauri.conf.json) and fades in once the interface has drawn itself
/// (window_ready), so the app opens softly instead of appearing empty.
#[cfg(target_os = "macos")]
fn fade_in(window: &tauri::Window) {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};

    if window.is_visible().unwrap_or(true) {
        return;
    }
    let Ok(ns_window) = window.ns_window() else {
        let _ = window.show();
        return;
    };
    let ns_window = ns_window as *mut AnyObject;
    // SAFETY: the live NSWindow of `window`, on the main thread (see reveal).
    unsafe {
        let _: () = msg_send![ns_window, setAlphaValue: 0.0f64];
    }
    let _ = window.show();
    let _ = window.set_focus();
    unsafe {
        let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
        let context: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
        let _: () = msg_send![context, setDuration: 0.3f64];
        let name: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: c"easeOut".as_ptr()];
        let timing: *mut AnyObject = msg_send![class!(CAMediaTimingFunction), functionWithName: name];
        let _: () = msg_send![context, setTimingFunction: timing];
        let animator: *mut AnyObject = msg_send![ns_window, animator];
        let _: () = msg_send![animator, setAlphaValue: 1.0f64];
        let _: () = msg_send![class!(NSAnimationContext), endGrouping];
    }
}

/// Fades the window in on the main thread; `after` waits first (the fallback, should the interface never say it is ready).
fn reveal(window: &tauri::Window, after: std::time::Duration) {
    let window = window.clone();
    std::thread::spawn(move || {
        std::thread::sleep(after);
        let target = window.clone();
        let _ = window.run_on_main_thread(move || {
            #[cfg(target_os = "macos")]
            fade_in(&target);
            #[cfg(not(target_os = "macos"))]
            let _ = target.show();
        });
    });
}

/// The interface has drawn itself (main.ts): the window can fade in.
#[tauri::command]
fn window_ready(webview: tauri::Webview) {
    // the webview's window: ReSurf's holds its tabs' webviews too
    reveal(&webview.window(), std::time::Duration::ZERO);
}

/// How long the windows take to fade out (hiding, closing) and in (showing again).
#[cfg(target_os = "macos")]
const FADE_SECONDS: f64 = 0.4;

/// `windows` animated to `alpha` over FADE_SECONDS, then `done`.
#[cfg(target_os = "macos")]
unsafe fn fade_windows(windows: &[*mut objc2::runtime::AnyObject], alpha: f64, done: Option<&block2::Block<dyn Fn()>>) {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
    let context: *mut AnyObject = msg_send![class!(NSAnimationContext), currentContext];
    let _: () = msg_send![context, setDuration: FADE_SECONDS];
    let name: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: c"easeInEaseOut".as_ptr()];
    let timing: *mut AnyObject = msg_send![class!(CAMediaTimingFunction), functionWithName: name];
    let _: () = msg_send![context, setTimingFunction: timing];
    if let Some(done) = done {
        let _: () = msg_send![context, setCompletionHandler: done];
    }
    for &window in windows {
        let animator: *mut AnyObject = msg_send![window, animator];
        let _: () = msg_send![animator, setAlphaValue: alpha];
    }
    let _: () = msg_send![class!(NSAnimationContext), endGrouping];
}

/// all of the app's NSWindows
#[cfg(target_os = "macos")]
unsafe fn app_windows() -> Vec<*mut objc2::runtime::AnyObject> {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
    let windows: *mut AnyObject = msg_send![app, windows];
    let count: usize = msg_send![windows, count];
    (0..count).map(|i| msg_send![windows, objectAtIndex: i]).collect()
}

/// NSApplication's own hide:, kept to be called once the windows have faded out
#[cfg(target_os = "macos")]
static HIDE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// hide: (⌘H, the Hide menu item, ReDesk's Hide / Show Windows): the windows fade out first
#[cfg(target_os = "macos")]
unsafe extern "C-unwind" fn fading_hide(this: *mut objc2::runtime::AnyObject, sel: objc2::runtime::Sel, sender: *mut objc2::runtime::AnyObject) {
    use block2::RcBlock;
    let this = this as usize;
    let sender = sender as usize;
    let done = RcBlock::new(move || {
        // SAFETY: HIDE holds NSApplication's hide:, which takes these arguments
        unsafe {
            let hide: unsafe extern "C-unwind" fn(*mut objc2::runtime::AnyObject, objc2::runtime::Sel, *mut objc2::runtime::AnyObject) =
                std::mem::transmute(HIDE.load(std::sync::atomic::Ordering::Relaxed));
            hide(this as *mut _, sel, sender as *mut _);
            // hidden now: back to whole, for when the app shows again (which fades them in)
            for window in app_windows() {
                let _: () = objc2::msg_send![window, setAlphaValue: 1.0f64];
            }
        }
    });
    // SAFETY: on the main thread, as hide: is
    unsafe { fade_windows(&app_windows(), 0.0, Some(&done)) };
}

/// Windows fade out when the app hides and when one closes, and fade in when the app shows again.
/// ReDesk's Hide / Show Windows and Close All Windows ask with a distributed notification, "hide", "show" or "close".
#[cfg(target_os = "macos")]
fn fade_windows_in_and_out() {
    use block2::RcBlock;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send, sel};
    use std::ffi::CStr;

    // SAFETY: AppKit's own objects, on the main thread (setup); hide: keeps its signature
    unsafe {
        let method = objc2::ffi::class_getInstanceMethod(class!(NSApplication), sel!(hide:));
        if !method.is_null() {
            let ours: objc2::runtime::Imp = std::mem::transmute(fading_hide as *const ());
            // only once: a second time would keep fading_hide as the "original"
            if HIDE.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                if let Some(old) = objc2::ffi::method_setImplementation(method, ours) {
                    HIDE.store(old as usize, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }

        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let null = std::ptr::null::<AnyObject>();
        let will = RcBlock::new(|_: *mut AnyObject| {
            for window in app_windows() {
                let _: () = msg_send![window, setAlphaValue: 0.0f64];
            }
        });
        let did = RcBlock::new(|_: *mut AnyObject| fade_windows(&app_windows(), 1.0, None));
        let will_name: *mut AnyObject =
            msg_send![class!(NSString), stringWithUTF8String: c"NSApplicationWillUnhideNotification".as_ptr()];
        let did_name: *mut AnyObject =
            msg_send![class!(NSString), stringWithUTF8String: c"NSApplicationDidUnhideNotification".as_ptr()];
        let _: *mut AnyObject = msg_send![center, addObserverForName: will_name, object: null, queue: null, usingBlock: &*will];
        let _: *mut AnyObject = msg_send![center, addObserverForName: did_name, object: null, queue: null, usingBlock: &*did];

        let desk = RcBlock::new(|note: *mut AnyObject| {
            let object: *mut AnyObject = msg_send![note, object];
            if object.is_null() {
                return;
            }
            let text: *const std::os::raw::c_char = msg_send![object, UTF8String];
            if text.is_null() {
                return;
            }
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let hidden: bool = msg_send![app, isHidden];
            let said = CStr::from_ptr(text).to_bytes();
            // "hide <pid>": this app alone (ReDesk's own desktops)
            let mine = format!("hide {}", std::process::id());
            if said == mine.as_bytes() {
                if !hidden {
                    let _: () = msg_send![app, hide: null];
                }
                return;
            }
            match said {
                b"hide" if !hidden => {
                    let _: () = msg_send![app, hide: null];
                }
                b"show" if hidden => {
                    let _: () = msg_send![app, unhideWithoutActivation];
                }
                b"close" => {
                    let windows = app_windows();
                    let shown: Vec<_> = windows.iter().copied().filter(|&w| msg_send![w, isVisible]).collect();
                    let gone = windows.iter().map(|&w| w as usize).collect::<Vec<_>>();
                    let done = RcBlock::new(move || {
                        for &window in &gone {
                            let _: () = msg_send![window as *mut AnyObject, close];
                        }
                    });
                    fade_windows(&shown, 0.0, Some(&done));
                }
                _ => {}
            }
        });
        let distributed: *mut AnyObject = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
        let name: *mut AnyObject =
            msg_send![class!(NSString), stringWithUTF8String: c"com.artursavytskyi.re.windows".as_ptr()];
        let queue: *mut AnyObject = msg_send![class!(NSOperationQueue), mainQueue];
        let _: *mut AnyObject = msg_send![distributed, addObserverForName: name, object: null, queue: queue, usingBlock: &*desk];
    }
}

/// The app, for ReDesk's word on which corners to round (listen_for_desk_corners).
#[cfg(target_os = "macos")]
static DESK_APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

/// ReDesk's Flush tiling: the windows side by side as one, so only the outer corners of them all are
/// round. ReDesk says, for each window, which corners (1 top left, 2 top right, 4 bottom left, 8 bottom
/// right) and which of its edges touch another window (1 top, 2 right, 4 bottom, 8 left): the window
/// rounds only those corners (no window has a rim or a shadow anyway).
#[cfg(target_os = "macos")]
fn listen_for_desk_corners(app: &tauri::AppHandle) {
    use block2::RcBlock;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use tauri::Manager;

    let _ = DESK_APP.set(app.clone());
    let handler = RcBlock::new(|note: *mut AnyObject| {
        // SAFETY: on the main queue; the userInfo ReDesk sends holds numbers only
        unsafe {
            let info: *mut AnyObject = msg_send![note, userInfo];
            if info.is_null() {
                return;
            }
            let number = |key: &std::ffi::CStr| -> f64 {
                let key: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: key.as_ptr()];
                let value: *mut AnyObject = msg_send![info, objectForKey: key];
                if value.is_null() { -1.0 } else { msg_send![value, doubleValue] }
            };
            if number(c"pid") as u32 != std::process::id() {
                return;
            }
            let corners = number(c"corners") as u32;
            // pinned by ReDesk (⌃⌥P): the page writes PIN down its left side
            let pinned = number(c"pinned") == 1.0;
            let Some(app) = DESK_APP.get() else { return };
            // which of the app's windows it is: the one with that frame
            let near = |a: f64, b: f64| (a - b).abs() < 24.0;
            for window in app.webview_windows().into_values() {
                let scale = window.scale_factor().unwrap_or(1.0);
                let (Ok(at), Ok(size)) = (window.outer_position(), window.outer_size()) else { continue };
                if near(at.x as f64 / scale, number(c"x")) && near(at.y as f64 / scale, number(c"y"))
                    && near(size.width as f64 / scale, number(c"w")) && near(size.height as f64 / scale, number(c"h"))
                {
                    set_corners(&window, corners);
                    let _ = tauri::Emitter::emit_to(app, window.label(), "pinned", pinned);
                }
            }
        }
    });
    // SAFETY: AppKit's own objects, on the main thread (setup); the center keeps the block
    unsafe {
        let center: *mut AnyObject = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
        let name: *mut AnyObject =
            msg_send![class!(NSString), stringWithUTF8String: c"com.artursavytskyi.re.corners".as_ptr()];
        let queue: *mut AnyObject = msg_send![class!(NSOperationQueue), mainQueue];
        let _: *mut AnyObject = msg_send![center, addObserverForName: name, object: std::ptr::null::<AnyObject>(),
                                          queue: queue, usingBlock: &*handler];
    }
}

/// Only `corners` round (the view's layer). No rim and no shadow, alone or not.
#[cfg(target_os = "macos")]
fn set_corners(window: &tauri::WebviewWindow, corners: u32) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    let _ = window.with_webview(move |platform| {
        let view = platform.inner() as *mut AnyObject;
        if view.is_null() {
            return;
        }
        // SAFETY: the live WKWebView, on the main thread
        unsafe {
            let layer: *mut AnyObject = msg_send![view, layer];
            if layer.is_null() {
                return;
            }
            // the layer's own corners (CACornerMask: min X min Y 1, max X min Y 2, min X max Y 4, max X max Y 8);
            // its min Y is at the top when its contents are flipped
            let flipped: bool = msg_send![layer, contentsAreFlipped];
            let (top_left, top_right, bottom_left, bottom_right) = if flipped { (1, 2, 4, 8) } else { (4, 8, 1, 2) };
            let mut mask: usize = 0;
            if corners & 1 != 0 { mask |= top_left; }
            if corners & 2 != 0 { mask |= top_right; }
            if corners & 4 != 0 { mask |= bottom_left; }
            if corners & 8 != 0 { mask |= bottom_right; }
            let _: () = msg_send![layer, setMaskedCorners: mask];
        }
    });
}

/// A window asked to close fades out first, then goes.
fn fade_on_close(window: &tauri::Window, event: &tauri::WindowEvent) {
    #[cfg(target_os = "macos")]
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        let Ok(ns_window) = window.ns_window() else { return };
        api.prevent_close();
        let target = window.clone();
        let done = block2::RcBlock::new(move || {
            let _ = target.destroy();
        });
        // SAFETY: the live NSWindow of `window`, on the main thread (window events are)
        unsafe { fade_windows(&[ns_window as *mut objc2::runtime::AnyObject], 0.0, Some(&done)) };
    }
}


// ── more windows: ⌘N ──

/// Windows opened so far (the first is "main"): the next one's label.
static WINDOW_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

/// ⌘N: another window, as the first one is (tauri.conf.json): the size of the one in front, a little
/// lower and to the right of it. It fades in once its interface has drawn itself (window_ready).
#[tauri::command]
async fn new_window(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let n = WINDOW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let mut config = app.config().app.windows.first().cloned().ok_or("No window to copy")?;
    config.label = format!("w{n}");
    let windows = app.webview_windows();
    let front = windows.values().find(|w| w.is_focused().unwrap_or(false)).or_else(|| windows.values().next());
    let mut place = None;
    if let Some(front) = front {
        let scale = front.scale_factor().unwrap_or(1.0);
        if let (Ok(at), Ok(size)) = (front.outer_position(), front.inner_size()) {
            config.center = false;
            config.width = size.width as f64 / scale;
            config.height = size.height as f64 / scale;
            place = Some((at.x as f64 / scale + 28.0, at.y as f64 / scale + 28.0));
        }
    }
    let mut builder = tauri::WebviewWindowBuilder::from_config(&app, &config).map_err(|e| e.to_string())?;
    if let Some((x, y)) = place {
        builder = builder.position(x, y);
    }
    let window = builder.build().map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    round_window(&window);
    reveal(&window.as_ref().window(), std::time::Duration::from_secs(3));
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .on_window_event(fade_on_close)
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            fade_windows_in_and_out();
            #[cfg(target_os = "macos")]
            listen_for_desk_corners(app.handle());
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let db_path = dir.join("library.sqlite");
            let db = Db::open(&db_path)?;
            // first start: use ~/Music
            if db.setting("folder")?.is_none() {
                if let Some(music) = dirs::audio_dir() {
                    db.set_setting("folder", &music.to_string_lossy())?;
                }
            }

            // when a track ends by itself, the queue moves on from here
            let handle = app.handle().clone();
            let audio = Audio::spawn(move || {
                let state = handle.state::<AppState>();
                let item = state.queue.lock().ok().and_then(|mut q| q.advance(true).cloned());
                play_current(&handle, item);
            });

            app.manage(AppState {
                db: Mutex::new(db),
                db_path,
                queue: Mutex::new(Queue::default()),
                audio,
                scanning: Mutex::new(false),
                radio: Mutex::new(Radio::default()),
            });

            #[cfg(target_os = "macos")]
            media::listen(app.handle());
            #[cfg(target_os = "macos")]
            if let Some(window) = app.get_webview_window("main") {
                round_window(&window);
                reveal(&window.as_ref().window(), std::time::Duration::from_secs(3));
            }
            let waiting = WAITING.lock().map(|mut w| std::mem::take(&mut *w)).unwrap_or_default();
            if !waiting.is_empty() {
                open_files(app.handle(), waiting);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            window_ready,
            new_window,
            music_folder,
            set_music_folder,
            rescan,
            tracks,
            playlists,
            playlist_tracks,
            create_playlist,
            rename_playlist,
            delete_playlist,
            add_to_playlist,
            remove_from_playlist,
            play,
            toggle_play,
            next,
            previous,
            seek,
            set_shuffle,
            cycle_repeat,
            play_next,
            add_to_queue,
            player_status,
            upcoming,
            reveal_in_finder,
            radio_genres,
            radio_stations,
            radio_search,
            radio_favorites,
            set_radio_favorite,
            play_station,
            take_video,
            open_paths,
        ])
        .build(tauri::generate_context!())
        .expect("error while running RePlay")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = event {
                open_files(app, urls.iter().filter_map(|u| u.to_file_path().ok()).collect());
            }
        });
}
