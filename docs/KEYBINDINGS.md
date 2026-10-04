# Key bindings

The same reference is available in the app: press `?`. Keys that print a
character do not apply while a text field (Search, a list filter) is being
typed into, except where noted.

## Everywhere

| Key | Action |
|-----|--------|
| `Tab` | Switch focus between the sidebar and the main pane |
| `Esc` | Back one level; at the root, focus moves to the sidebar |
| `?` | Help |
| `Space` / `n` / `p` | Play-pause / next / previous |
| `+` / `-` | Volume up / down |
| `m` | Mute / unmute (restores the exact previous volume). Not on Playlist Detail, where `m` is move mode |
| `s` | Cycle shuffle: off, shuffle, smart shuffle, off |
| `r` | Cycle repeat: off, album/playlist, this song, off |
| `t` | Toggle romanized lyrics |
| `f` | Fullscreen Now Playing; jumps there from anywhere |
| `l` | Go to Library |
| `/` | Go to Search (sidebar, Now Playing), or open a list's filter |
| `Ctrl+P` | Quick jump: find any playlist, liked track, artist, album, device or screen by name, or choose **Sign out**. Works mid-query in Search; press again to close |
| `c` | Create a playlist |
| `Shift+Q` | Quit (asks first by default, see [configuration](CONFIGURATION.md)) |
| `Ctrl+C` | Quit immediately, always |

## Now Playing

| Key | Action |
|-----|--------|
| `←` / `→` | Seek 5 s |
| `↑` / `↓` | Volume |
| `a` | Add the playing track to a playlist |
| `Shift+L` | Like the playing track |
| `v` / `Shift+V` | Open the playing track's album / artist |

## Sidebar

| Key | Action |
|-----|--------|
| `↑` / `↓` | Move |
| `Enter` / `→` | Open |
| `Shift+P` | Pin / unpin the selected playlist |

## Lists (Liked Songs, Saved Albums, Followed Artists, Your Playlists, Playlist Detail)

| Key | Action |
|-----|--------|
| `↑` / `↓` | Move selection |
| `Enter` | Play the selected track (track lists) or open it (albums, artists, playlists) |
| `→` | Open (albums, artists, playlists) |
| `←` / `Esc` | Back. If a filter is applied, `Esc` clears it first |
| `/` | Filter; narrows as you type. `Enter` keeps it, `Esc` drops it |
| `o` | Toggle alphabetical sort |
| `Shift+P` | Pin / unpin (Your Playlists, Playlist Detail) |

## Track actions (Liked Songs, Playlist Detail, Queue, Album Detail)

| Key | Action |
|-----|--------|
| `a` | Add to a playlist. The picker asks first if the playlist already has the track |
| `q` | Add to the queue (not on the Queue screen) |
| `Shift+L` | Like. On Liked Songs it unlikes instead, and confirms |
| `v` | Open the track's album. On Album Detail, the album's artist |
| `Shift+V` | Open the track's artist |

## Playlists

| Key | Action |
|-----|--------|
| `Shift+R` | Rename (Your Playlists: the selected one; Playlist Detail: the open one) |
| `d` | Delete the selected playlist (Your Playlists), or remove the selected track (Playlist Detail). Always confirms |
| `Shift+D` | Delete the open playlist (Playlist Detail). Always confirms |
| `m` | Move mode (Playlist Detail). Needs the filter and sort off |

**Move mode:** `↑`/`↓` move the track locally, `g` jumps to a typed position,
`Enter` confirms with one reorder request, `Esc` walks it back. Every other key
is ignored while it is active.

Spotify can only remove *every* copy of a duplicated track at once, so `d` warns
when the track appears more than once.

## Library, follow and save

| Key | Action |
|-----|--------|
| `Shift+F` | Artist Detail: follow. Followed Artists: unfollow (confirms) |
| `Shift+S` | Album Detail: save. Saved Albums: unsave (confirms) |

## Devices

| Key | Action |
|-----|--------|
| `Enter` | Transfer playback to the selected device |
| `Shift+R` | Refresh |

## Search

Every printable key goes to the query, so actions on a result use modifiers.

| Key | Action |
|-----|--------|
| `Enter` | Run the search; with results showing, play the selected one |
| `↑` / `↓` | Move through results |
| `←` / `→` | Move the cursor in the query |
| `Ctrl+↑` | Like the selected result |
| `Ctrl+↓` | Add to a playlist |
| `Alt+↓` | Add to the queue |
| `Ctrl+→` / `Alt+→` | Open the result's album / artist |
| `Esc` | Back |

## Overlays

| Key | Action |
|-----|--------|
| `y` / `Enter` / `n` / `Esc` | Confirm dialog: yes, yes, no, no. Other keys are ignored |
| `Enter` / `Esc` | Text prompt: submit / cancel |
| `↑` / `↓` / type / `Enter` / `Esc` | Playlist picker and quick jump: move, narrow, choose, cancel |
