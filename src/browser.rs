//! The in-app browser: search, playlists, liked songs, recents and the queue.
//! Pure state; rendering lives in ui.rs and side effects in app.rs.

use crate::web::{AlbumItem, ArtistItem, PlaylistItem, SearchResults, TrackItem};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Search,
    Playlists,
    Liked,
    Recent,
    Top,
    Queue,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::Search, Tab::Playlists, Tab::Liked, Tab::Recent, Tab::Top, Tab::Queue];
    pub fn title(self) -> &'static str {
        match self {
            Tab::Search => "search",
            Tab::Playlists => "playlists",
            Tab::Liked => "liked",
            Tab::Recent => "recent",
            Tab::Top => "top",
            Tab::Queue => "queue",
        }
    }
    pub fn next(self) -> Tab {
        let i = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Tab::ALL[(i + 1) % Tab::ALL.len()]
    }
    pub fn prev(self) -> Tab {
        let i = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Tab::ALL[(i + Tab::ALL.len() - 1) % Tab::ALL.len()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Input,
    List,
}

/// What the list currently shows.
#[derive(Clone, Debug)]
pub enum View {
    Empty,
    Search(SearchResults),
    Playlists(Vec<PlaylistItem>),
    Tracks {
        title: String,
        /// Context URI to play tracks inside (album/playlist/collection).
        context: Option<String>,
        items: Vec<TrackItem>,
    },
    Albums {
        title: String,
        items: Vec<AlbumItem>,
    },
}

/// One list row, flattened from the view.
#[derive(Clone, Debug)]
pub enum Row {
    Header(String),
    Track(TrackItem, Option<String>),
    Album(AlbumItem),
    Artist(ArtistItem),
    Playlist(PlaylistItem),
    Note(String),
}

impl Row {
    pub fn selectable(&self) -> bool {
        !matches!(self, Row::Header(_) | Row::Note(_))
    }
}

impl View {
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        match self {
            View::Empty => {}
            View::Search(r) => {
                if !r.tracks.is_empty() {
                    rows.push(Row::Header("songs".into()));
                    rows.extend(r.tracks.iter().cloned().map(|t| Row::Track(t, None)));
                }
                if !r.artists.is_empty() {
                    rows.push(Row::Header("artists".into()));
                    rows.extend(r.artists.iter().cloned().map(Row::Artist));
                }
                if !r.albums.is_empty() {
                    rows.push(Row::Header("albums".into()));
                    rows.extend(r.albums.iter().cloned().map(Row::Album));
                }
                if !r.playlists.is_empty() {
                    rows.push(Row::Header("playlists".into()));
                    rows.extend(r.playlists.iter().cloned().map(Row::Playlist));
                }
                if rows.is_empty() {
                    rows.push(Row::Note("no results".into()));
                }
            }
            View::Playlists(items) => {
                if items.is_empty() {
                    rows.push(Row::Note("no playlists".into()));
                }
                rows.extend(items.iter().cloned().map(Row::Playlist));
            }
            View::Tracks { items, context, .. } => {
                if items.is_empty() {
                    rows.push(Row::Note("nothing here".into()));
                }
                rows.extend(items.iter().cloned().map(|t| Row::Track(t, context.clone())));
            }
            View::Albums { items, .. } => {
                if items.is_empty() {
                    rows.push(Row::Note("no albums".into()));
                }
                rows.extend(items.iter().cloned().map(Row::Album));
            }
        }
        rows
    }

    pub fn title(&self) -> Option<&str> {
        match self {
            View::Tracks { title, .. } | View::Albums { title, .. } => Some(title),
            _ => None,
        }
    }
}

pub struct Browser {
    pub open: bool,
    pub tab: Tab,
    pub focus: Focus,
    pub query: String,
    pub searched: String,
    pub view: View,
    /// Drill-down history (parent views).
    pub stack: Vec<(View, usize)>,
    pub selected: usize,
    pub scroll: usize,
    pub loading: Option<String>,
    pub error: Option<String>,
    /// Monotonic request id so stale replies are ignored.
    pub req: u64,
    /// Cached per-tab views so switching back is instant.
    pub cache_playlists: Option<Vec<PlaylistItem>>,
    pub cache_liked: Option<Vec<TrackItem>>,
    /// Setup mode: collecting the client id before login.
    pub setup: bool,
    pub setup_input: String,
    pub setup_status: Vec<String>,
}

impl Default for Browser {
    fn default() -> Self {
        Browser {
            open: false,
            tab: Tab::Search,
            focus: Focus::Input,
            query: String::new(),
            searched: String::new(),
            view: View::Empty,
            stack: Vec::new(),
            selected: 0,
            scroll: 0,
            loading: None,
            error: None,
            req: 0,
            cache_playlists: None,
            cache_liked: None,
            setup: false,
            setup_input: String::new(),
            setup_status: Vec::new(),
        }
    }
}

impl Browser {
    pub fn rows(&self) -> Vec<Row> {
        self.view.rows()
    }

    pub fn selected_row(&self) -> Option<Row> {
        self.rows().into_iter().nth(self.selected)
    }

    pub fn set_view(&mut self, view: View) {
        self.view = view;
        self.selected = 0;
        self.scroll = 0;
        self.error = None;
        self.loading = None;
        self.snap_selection(1);
    }

    pub fn push_view(&mut self, view: View) {
        let old = std::mem::replace(&mut self.view, View::Empty);
        self.stack.push((old, self.selected));
        self.set_view(view);
        self.focus = Focus::List;
    }

    pub fn pop_view(&mut self) -> bool {
        if let Some((v, sel)) = self.stack.pop() {
            self.view = v;
            self.selected = sel;
            self.error = None;
            self.loading = None;
            true
        } else {
            false
        }
    }

    pub fn next_req(&mut self) -> u64 {
        self.req = self.req.wrapping_add(1);
        self.req
    }

    /// Move selection by `delta`, skipping headers.
    pub fn move_selection(&mut self, delta: i32) {
        let rows = self.rows();
        if rows.is_empty() {
            self.selected = 0;
            return;
        }
        let mut i = self.selected as i32;
        let n = rows.len() as i32;
        let step = if delta < 0 { -1 } else { 1 };
        let mut remaining = delta.abs();
        while remaining > 0 {
            let mut j = i + step;
            while j >= 0 && j < n && !rows[j as usize].selectable() {
                j += step;
            }
            if j < 0 || j >= n {
                break;
            }
            i = j;
            remaining -= 1;
        }
        self.selected = i.max(0) as usize;
    }

    /// Ensure the selection sits on a selectable row (search forward, then back).
    pub fn snap_selection(&mut self, dir: i32) {
        let rows = self.rows();
        if rows.is_empty() {
            self.selected = 0;
            return;
        }
        let n = rows.len();
        let mut i = self.selected.min(n - 1);
        if rows[i].selectable() {
            self.selected = i;
            return;
        }
        let step = dir.signum().max(-1);
        loop {
            let j = i as i64 + step as i64;
            if j < 0 || j >= n as i64 {
                break;
            }
            i = j as usize;
            if rows[i].selectable() {
                self.selected = i;
                return;
            }
        }
        // Nothing in that direction: search the other way.
        self.selected = rows.iter().position(|r| r.selectable()).unwrap_or(0);
    }

    pub fn ensure_visible(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
    }
}

/// Sample data for layout checks without an account (`AURA_DEMO_BROWSER=1`).
pub fn demo_view() -> View {
    let track = |n: &str, a: &str, al: &str, d: u64| TrackItem {
        id: n.to_lowercase().replace(' ', ""),
        uri: format!("spotify:track:{}", n.to_lowercase().replace(' ', "")),
        name: n.into(),
        artists: a.into(),
        album: al.into(),
        duration_ms: d,
        art_url: None,
    };
    View::Search(SearchResults {
        tracks: vec![
            track("Reverse Psychology", "Temper City", "Reverse Psychology", 178_928),
            track("Calling After Me", "Wallows", "Model", 187_692),
            track("Think Fast (feat. Weezer)", "Dominic Fike, Weezer", "Sunburn", 222_839),
            track("A Very Long Song Title That Keeps Going On And On Forever", "Someone With A Long Name", "An Even Longer Album Title For Testing Truncation", 3_601_000),
        ],
        albums: vec![AlbumItem {
            id: "a1".into(),
            uri: "spotify:album:a1".into(),
            name: "Model".into(),
            artists: "Wallows".into(),
            year: "2024".into(),
            total_tracks: 12,
            art_url: None,
        }],
        artists: vec![ArtistItem { id: "ar1".into(), uri: "spotify:artist:ar1".into(), name: "Wallows".into(), art_url: None }],
        playlists: vec![PlaylistItem {
            id: "p1".into(),
            uri: "spotify:playlist:p1".into(),
            name: "late night drive".into(),
            owner: "zezo".into(),
            total: 143,
            art_url: None,
        }],
    })
}
