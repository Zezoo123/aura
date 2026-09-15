//! Rendering. Everything is painted straight into the buffer so the ambient
//! backdrop survives underneath text.

use std::time::Duration;

use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    widgets::StatefulWidget,
    Frame,
};
use ratatui_image::{FilterType, Resize, StatefulImage};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{Action, App, HelperStatus, HitAreas, Layout, LyricsStatus},
    spotify::PlayerState,
    theme::{Rgb, Theme},
};

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let theme = app.theme_now();
    let layout = app.layout_for(area.width, area.height);
    app.hit = HitAreas::default();
    let buf = f.buffer_mut();

    fill_background(buf, area, app, &theme);

    let overlay = app.browser.open || app.help;
    if overlay {
        // Modal: keep only the chrome underneath. Drawing the art here would leave
        // image fragments behind on iTerm2 and lyrics would poke out around the panel.
        draw_header(buf, Rect::new(area.x, area.y, area.width, 1), app, &theme);
        if app.snap.running && app.snap.track.is_some() && area.height > 8 {
            let y = area.bottom() - 3;
            draw_progress(buf, Rect::new(area.x + 3, y, area.width.saturating_sub(6), 1), app, &theme);
            draw_controls(buf, Rect::new(area.x, y + 1, area.width, 1), app, &theme);
        }
    } else if !app.snap.running {
        draw_message(buf, area, &theme, "Spotify isn't running", "press enter to launch it · q to quit");
        app.hit.buttons.push((area, Action::Launch));
    } else if app.snap.track.is_none() {
        draw_header(buf, Rect::new(area.x, area.y, area.width, 1), app, &theme);
        draw_message(buf, area, &theme, "Nothing playing", "start something in Spotify and it shows up here");
    } else {
        match layout {
            Layout::Cover => draw_cover(buf, area, app, &theme),
            Layout::Split => draw_split(buf, area, app, &theme),
            Layout::Lyrics => draw_lyrics_layout(buf, area, app, &theme),
            Layout::Mini => draw_mini(buf, area, app, &theme),
        }
    }

    if app.browser.open {
        draw_browser(buf, area, app, &theme);
    }
    if app.help {
        draw_help(buf, area, &theme);
    }
    draw_toast(buf, area, app, &theme);
}

// ---------------------------------------------------------------- helpers

fn st(fg: Rgb) -> Style {
    Style::default().fg(fg.color())
}

fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style, max_w: u16) -> u16 {
    if max_w == 0 || y >= buf.area.bottom() || x >= buf.area.right() {
        return 0;
    }
    let (nx, _) = buf.set_stringn(x, y, s, max_w as usize, style);
    nx - x
}

/// Truncate to `w` cells, appending an ellipsis when cut.
fn fit(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > w.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

fn wrap(s: &str, w: usize) -> Vec<String> {
    if w == 0 {
        return vec![];
    }
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0;
    for word in s.split_whitespace() {
        let ww = word.width();
        if cur_w == 0 {
            if ww > w {
                lines.push(fit(word, w));
            } else {
                cur.push_str(word);
                cur_w = ww;
            }
        } else if cur_w + 1 + ww <= w {
            cur.push(' ');
            cur.push_str(word);
            cur_w += 1 + ww;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
            if ww > w {
                lines.push(fit(word, w));
            } else {
                cur.push_str(word);
                cur_w = ww;
            }
        }
    }
    if cur_w > 0 || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

fn fmt_time(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn centered_x(area: Rect, w: usize) -> u16 {
    area.x + (area.width.saturating_sub(w as u16)) / 2
}

fn put_centered(buf: &mut Buffer, area: Rect, y: u16, s: &str, style: Style) {
    let s = fit(s, area.width as usize);
    let x = centered_x(area, s.width());
    put(buf, x, y, &s, style, area.width);
}

/// Largest square (in pixels) of art that fits `avail`, centered.
fn art_square(avail: Rect, font: ratatui_image::FontSize) -> Rect {
    let ratio = font.height.max(1) as f32 / font.width.max(1) as f32; // cols per row
    let rows = (avail.height as f32).min(avail.width as f32 / ratio).floor().max(0.0) as u16;
    let cols = ((rows as f32) * ratio).round() as u16;
    let cols = cols.min(avail.width);
    Rect::new(
        avail.x + (avail.width - cols) / 2,
        avail.y + (avail.height - rows) / 2,
        cols,
        rows,
    )
}

// ------------------------------------------------------------ background

fn fill_background(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme) {
    let ambient = if app.ambient { app.art.as_ref().map(|a| &a.ambient) } else { None };
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let bg = match ambient {
                Some(am) => am.sample(
                    (x - area.x) as f32 / area.width.max(1) as f32,
                    (y - area.y) as f32 / area.height.max(1) as f32,
                ),
                None => theme.bg,
            };
            if let Some(c) = buf.cell_mut(Position::new(x, y)) {
                c.reset();
                c.set_symbol(" ");
                c.set_style(Style::default().bg(bg.color()).fg(theme.text.color()));
            }
        }
    }
}

// ---------------------------------------------------------------- pieces

fn draw_header(buf: &mut Buffer, r: Rect, app: &mut App, theme: &Theme) {
    if r.height == 0 {
        return;
    }
    let mut x = r.x + 2;
    x += put(buf, x, r.y, "◉ aura", st(theme.accent).add_modifier(Modifier::BOLD), r.width);
    let (glyph, word) = match app.snap.state {
        PlayerState::Playing => ("▶", "playing"),
        PlayerState::Paused => ("❚❚", "paused"),
        PlayerState::Stopped => ("■", "stopped"),
    };
    x += put(buf, x + 2, r.y, glyph, st(theme.muted), r.width) + 2;
    x += put(buf, x + 1, r.y, word, st(theme.muted), r.width) + 1;
    if app.web.is_some() {
        let (heart, style) = match app.liked {
            Some(true) => ("♥", st(theme.accent)),
            Some(false) => ("♡", st(theme.dim)),
            None => ("♡", st(theme.track)),
        };
        let hw = put(buf, x + 2, r.y, heart, style, r.width);
        app.hit.buttons.push((Rect::new(x + 2, r.y, hw, 1), Action::Like));
    }

    // Right side: shuffle · repeat · volume.
    let vol = app.volume();
    let filled = ((vol as f32 / 100.0) * 8.0).round() as usize;
    let bar: String = (0..8).map(|i| if i < filled { '▮' } else { '▯' }).collect();
    let items: Vec<(String, Style, Option<Action>)> = vec![
        (
            "⇄ shuffle".into(),
            st(if app.snap.shuffle { theme.accent } else { theme.dim }),
            Some(Action::Shuffle),
        ),
        (
            "↻ repeat".into(),
            st(if app.snap.repeat { theme.accent } else { theme.dim }),
            Some(Action::Repeat),
        ),
        (
            format!("vol {bar} {vol:>3}%"),
            st(if vol == 0 { theme.dim } else { theme.muted }),
            Some(Action::Mute),
        ),
    ];
    let total: usize = items.iter().map(|(s, _, _)| s.width()).sum::<usize>() + (items.len() - 1) * 3;
    if (total as u16) + 2 < r.width.saturating_sub(x) {
        let mut cx = r.x + r.width - 2 - total as u16;
        for (s, style, action) in items {
            let w = put(buf, cx, r.y, &s, style, r.width);
            if let Some(a) = action {
                app.hit.buttons.push((Rect::new(cx, r.y, w, 1), a));
            }
            cx += w + 3;
        }
    }
}

fn draw_art(buf: &mut Buffer, rect: Rect, app: &mut App, theme: &Theme) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    app.hit.art = Some(rect);
    match app.art_proto.as_mut() {
        Some(proto) => {
            // Pin the style of every cell under the image to a constant. The ambient
            // fill and theme fade would otherwise touch these cells each frame, which
            // makes ratatui re-send the (large) image payload for protocols that keep
            // it in the cell's symbol.
            // The pinned background is the *target* theme's tone (not the animated one)
            // so it only changes together with the image itself.
            let pinned = Style::default().fg(Color::Reset).bg(app.theme.bg.color());
            for y in rect.top()..rect.bottom() {
                for x in rect.left()..rect.right() {
                    if let Some(c) = buf.cell_mut(Position::new(x, y)) {
                        c.set_style(pinned);
                    }
                }
            }
            // Scale (not Fit): Spotify art is 640px and must be upscaled on big windows.
            StatefulImage::new()
                .resize(Resize::Scale(Some(FilterType::Lanczos3)))
                .render(rect, buf, proto);
        }
        None => {
            // Placeholder frame with a note in the middle.
            let style = st(theme.dim);
            for y in rect.top()..rect.bottom() {
                for x in rect.left()..rect.right() {
                    let top = y == rect.top();
                    let bottom = y == rect.bottom() - 1;
                    let left = x == rect.left();
                    let right = x == rect.right() - 1;
                    let ch = match (top, bottom, left, right) {
                        (true, _, true, _) => "╭",
                        (true, _, _, true) => "╮",
                        (_, true, true, _) => "╰",
                        (_, true, _, true) => "╯",
                        (true, _, _, _) | (_, true, _, _) => "─",
                        (_, _, true, _) | (_, _, _, true) => "│",
                        _ => continue,
                    };
                    put(buf, x, y, ch, style, 1);
                }
            }
            let label = if app.art_loading { "♪ loading art" } else { "♪" };
            if rect.height >= 3 {
                put_centered(buf, rect, rect.y + rect.height / 2, label, st(theme.muted));
            }
        }
    }
}

fn draw_progress(buf: &mut Buffer, r: Rect, app: &mut App, theme: &Theme) {
    let Some(track) = app.snap.track.clone() else { return };
    if r.height == 0 || r.width < 12 {
        return;
    }
    let pos = app.position_ms();
    let dur = track.duration_ms.max(1);
    let left = fmt_time(pos);
    let right = format!("-{}", fmt_time(dur.saturating_sub(pos)));
    let time_w = fmt_time(dur).len().max(4) as u16 + 1;
    let bar_x = r.x + time_w + 1;
    let bar_w = r.width.saturating_sub(2 * (time_w + 1));
    put(buf, r.x, r.y, &format!("{left:>w$}", w = time_w as usize), st(theme.muted), r.width);
    put(
        buf,
        bar_x + bar_w + 1,
        r.y,
        &format!("{right:<w$}", w = time_w as usize),
        st(theme.dim),
        r.width,
    );
    if bar_w == 0 {
        return;
    }
    app.hit.progress = Some(Rect::new(bar_x, r.y, bar_w, 1));
    let frac = (pos as f64 / dur as f64).clamp(0.0, 1.0);
    let filled_cells = frac * bar_w as f64;
    let full = filled_cells.floor() as u16;
    let partial = filled_cells - full as f64;
    const EIGHTHS: [&str; 8] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    for i in 0..bar_w {
        let t = i as f32 / bar_w.max(1) as f32;
        let x = bar_x + i;
        if i < full {
            put(buf, x, r.y, "█", st(theme.gradient(t)), 1);
        } else if i == full {
            let idx = (partial * 8.0).floor() as usize;
            let style = Style::default().fg(theme.gradient(t).color()).bg(theme.track.color());
            put(buf, x, r.y, EIGHTHS[idx.min(7)], style, 1);
        } else {
            put(buf, x, r.y, "█", st(theme.track), 1);
        }
    }
}

fn draw_controls(buf: &mut Buffer, r: Rect, app: &mut App, theme: &Theme) {
    if r.height == 0 {
        return;
    }
    let playing = app.snap.state == PlayerState::Playing;
    let buttons: Vec<(&str, &str, Action)> = vec![
        ("◀◀", "p", Action::Prev),
        (if playing { "❚❚" } else { "▶" }, "space", Action::PlayPause),
        ("▶▶", "n", Action::Next),
        ("⇄", "s", Action::Shuffle),
        ("↻", "r", Action::Repeat),
        ("♪", "l", Action::ToggleLyrics),
        ("⌕", "/", Action::OpenBrowser(Some(crate::browser::Tab::Search))),
        ("?", "help", Action::ToggleHelp),
    ];
    let gap = 3usize;
    let total: usize = buttons.iter().map(|(g, k, _)| g.width() + 1 + k.width()).sum::<usize>() + gap * (buttons.len() - 1);
    if total as u16 > r.width {
        // Compact: glyphs only.
        let total: usize = buttons.len() * 2 + (buttons.len() - 1) * 2;
        let mut x = centered_x(r, total);
        for (g, _, a) in buttons {
            let w = put(buf, x, r.y, g, st(theme.accent), r.width);
            app.hit.buttons.push((Rect::new(x, r.y, w.max(1), 1), a));
            x += w + 2;
        }
        return;
    }
    let mut x = centered_x(r, total);
    for (g, k, a) in buttons {
        let start = x;
        let on = match a {
            Action::Shuffle => app.snap.shuffle,
            Action::Repeat => app.snap.repeat,
            Action::ToggleLyrics => app.show_lyrics,
            _ => true,
        };
        x += put(buf, x, r.y, g, st(if on { theme.accent } else { theme.dim }), r.width);
        x += 1;
        x += put(buf, x, r.y, k, st(theme.dim), r.width);
        app.hit.buttons.push((Rect::new(start, r.y, x - start, 1), a));
        x += gap as u16;
    }
}

fn draw_title_block(buf: &mut Buffer, r: Rect, app: &App, theme: &Theme, centered: bool) -> u16 {
    let Some(t) = &app.snap.track else { return 0 };
    let w = r.width as usize;
    let mut y = r.y;
    let title_lines = wrap(&t.name, w);
    let put_line = |buf: &mut Buffer, y: u16, s: &str, style: Style| {
        if centered {
            put_centered(buf, r, y, s, style);
        } else {
            put(buf, r.x, y, &fit(s, w), style, r.width);
        }
    };
    for line in title_lines.iter().take(2) {
        if y >= r.bottom() {
            break;
        }
        put_line(buf, y, line, st(theme.text).add_modifier(Modifier::BOLD));
        y += 1;
    }
    if y < r.bottom() {
        put_line(buf, y, &t.artist, st(theme.accent));
        y += 1;
    }
    if y < r.bottom() {
        let album = if t.album == t.name { String::new() } else { t.album.clone() };
        if !album.is_empty() {
            put_line(buf, y, &album, st(theme.muted));
            y += 1;
        }
    }
    y - r.y
}

fn draw_meta(buf: &mut Buffer, r: Rect, app: &App, theme: &Theme) {
    let Some(t) = &app.snap.track else { return };
    let mut y = r.y;
    let mut line = |buf: &mut Buffer, label: &str, value: &str, style: Style| {
        if y >= r.bottom() {
            return;
        }
        let x = r.x;
        let lw = put(buf, x, y, &format!("{label:<10}"), st(theme.dim), r.width);
        put(buf, x + lw, y, &fit(value, (r.width.saturating_sub(lw)) as usize), style, r.width.saturating_sub(lw));
        y += 1;
    };
    if t.track_number > 0 {
        line(buf, "track", &format!("#{}", t.track_number), st(theme.muted));
    }
    if t.album_artist != t.artist && !t.album_artist.is_empty() {
        line(buf, "album by", &t.album_artist, st(theme.muted));
    }
    let pop = (t.popularity as usize / 10).min(10);
    let pop_bar: String = (0..10).map(|i| if i < pop { '●' } else { '○' }).collect();
    line(buf, "popular", &format!("{pop_bar} {}", t.popularity), st(theme.muted));
    line(buf, "length", &fmt_time(t.duration_ms), st(theme.muted));
    let src = match app.helper {
        HelperStatus::Live => "Spotify · live events",
        HelperStatus::Polling => "Spotify · polling",
        HelperStatus::Starting => "Spotify",
    };
    line(buf, "source", src, st(theme.dim));
    let ly = match &app.lyrics_status {
        LyricsStatus::Idle => "off".to_string(),
        LyricsStatus::Loading => "searching…".to_string(),
        LyricsStatus::Found => {
            let l = app.lyrics.as_ref().unwrap();
            if !l.synced.is_empty() {
                format!("synced · {}", l.source)
            } else if l.instrumental {
                "instrumental".into()
            } else {
                format!("plain · {}", l.source)
            }
        }
        LyricsStatus::NotFound => "not found".to_string(),
        LyricsStatus::Error(e) => format!("error: {e}"),
    };
    line(buf, "lyrics", &ly, st(theme.dim));
}

/// Render the lyrics panel. `centered` aligns text to the middle of the area.
fn draw_lyrics(buf: &mut Buffer, r: Rect, app: &App, theme: &Theme, centered: bool) {
    if r.height == 0 || r.width < 8 {
        return;
    }
    let mid_y = r.y + r.height / 2;
    let put_line = |buf: &mut Buffer, y: u16, s: &str, style: Style| {
        if y < r.top() || y >= r.bottom() {
            return;
        }
        if centered {
            put_centered(buf, r, y, s, style);
        } else {
            put(buf, r.x, y, &fit(s, r.width as usize), style, r.width);
        }
    };
    match &app.lyrics_status {
        LyricsStatus::Loading => {
            let dots = ((app.started.elapsed().as_millis() / 400) % 4) as usize;
            put_line(buf, mid_y, &format!("searching lyrics{}", ".".repeat(dots)), st(theme.dim));
            return;
        }
        LyricsStatus::NotFound => {
            put_line(buf, mid_y, "no lyrics found", st(theme.dim));
            return;
        }
        LyricsStatus::Error(e) => {
            put_line(buf, mid_y, &fit(&format!("lyrics unavailable · {e}"), r.width as usize), st(theme.dim));
            return;
        }
        LyricsStatus::Idle => return,
        LyricsStatus::Found => {}
    }
    let Some(lyrics) = &app.lyrics else { return };
    if lyrics.instrumental && lyrics.synced.is_empty() {
        put_line(buf, mid_y, "♪ instrumental ♪", st(theme.muted));
        return;
    }
    let width = (r.width as usize).saturating_sub(if centered { 2 } else { 0 }).max(4);

    if lyrics.synced.is_empty() {
        // Plain lyrics: scrollable block.
        let mut rows: Vec<String> = Vec::new();
        for l in &lyrics.plain {
            if l.trim().is_empty() {
                rows.push(String::new());
            } else {
                rows.extend(wrap(l, width));
            }
        }
        let start = (app.plain_scroll as usize).min(rows.len().saturating_sub(1));
        for (i, row) in rows.iter().skip(start).take(r.height as usize).enumerate() {
            put_line(buf, r.y + i as u16, row, st(theme.text));
        }
        if rows.len() > r.height as usize {
            put(buf, r.right().saturating_sub(6), r.bottom() - 1, "j/k ↕", st(theme.dim), 6);
        }
        return;
    }

    // Synced: build display rows (wrapped) with the owning lyric index.
    let mut rows: Vec<(usize, String)> = Vec::new();
    let mut first_row_of: Vec<usize> = Vec::with_capacity(lyrics.synced.len());
    for (i, l) in lyrics.synced.iter().enumerate() {
        first_row_of.push(rows.len());
        if l.text.trim().is_empty() {
            rows.push((i, "♪".to_string()));
        } else {
            for w in wrap(&l.text, width) {
                rows.push((i, w));
            }
        }
    }
    let pos = app.position_ms() as i64 + app.lyrics_offset_ms;
    let current = lyrics.index_at(pos);

    // Scroll position in rows, smoothed: interpolate between the first rows of the
    // neighbouring lyric lines.
    let s = app.lyric_scroll.max(-1.0);
    let row_pos = if s < 0.0 {
        -1.0 + (s + 1.0) * (first_row_of[0] as f32 + 1.0)
    } else {
        let i0 = (s.floor() as usize).min(lyrics.synced.len() - 1);
        let i1 = (i0 + 1).min(lyrics.synced.len() - 1);
        let f = s - i0 as f32;
        first_row_of[i0] as f32 + (first_row_of[i1] as f32 - first_row_of[i0] as f32) * f
    };
    let top_row = row_pos.round() as i64 - (r.height as i64 / 2);

    for screen in 0..r.height as i64 {
        let ri = top_row + screen;
        let y = r.y + screen as u16;
        if ri < 0 || ri as usize >= rows.len() {
            if ri == -1 && current.is_none() {
                // Intro countdown before the first line.
                let first_at = lyrics.synced[0].at_ms as i64;
                let k = (pos.max(0) as f32 / first_at.max(1) as f32).clamp(0.0, 1.0);
                let n = (k * 3.0).ceil() as usize;
                let dots: String = (0..3).map(|i| if i < n { "● " } else { "○ " }).collect();
                put_line(buf, y, dots.trim_end(), st(theme.muted));
            }
            continue;
        }
        let (li, text) = &rows[ri as usize];
        let dist = match current {
            Some(c) => (*li as i64 - c as i64).unsigned_abs(),
            None => 3 + *li as u64,
        };
        let style = match dist {
            0 => st(theme.accent).add_modifier(Modifier::BOLD),
            1 => st(theme.text),
            2 => st(theme.muted),
            _ => st(theme.dim),
        };
        put_line(buf, y, text, style);
    }
}

fn draw_message(buf: &mut Buffer, area: Rect, theme: &Theme, big: &str, small: &str) {
    let y = area.y + area.height / 2;
    put_centered(buf, area, y.saturating_sub(1), big, st(theme.text).add_modifier(Modifier::BOLD));
    put_centered(buf, area, y + 1, small, st(theme.dim));
}

fn draw_toast(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme) {
    if let Some(text) = app.toast_text() {
        let s = format!(" {text} ");
        let w = s.width() as u16;
        if area.height > 1 && w + 2 <= area.width {
            let x = area.x + area.width - w - 2;
            let style = Style::default().fg(theme.bg.color()).bg(theme.accent.color()).add_modifier(Modifier::BOLD);
            put(buf, x, area.y + 1, &s, style, w);
        }
    } else if let Some((err, at)) = &app.last_error {
        if at.elapsed() < Duration::from_secs(6) && area.height > 1 {
            let s = fit(&format!("⚠ {err}"), area.width.saturating_sub(4) as usize);
            put(buf, area.x + 2, area.y + 1, &s, st(theme.muted), area.width);
        }
    }
}

fn draw_help(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let rows: [(&str, &str); 23] = [
        ("/", "search songs · artists · albums · playlists"),
        ("tab", "browse playlists · liked · recent · top · queue"),
        ("h", "♥ like / unlike"),
        ("space / enter", "play · pause"),
        ("n  p", "next · previous"),
        ("← →", "seek 5s (shift: 15s)"),
        ("↑ ↓  + -", "volume"),
        ("m", "mute"),
        ("s  r", "shuffle · repeat"),
        ("l", "lyrics on/off"),
        ("[ ]", "nudge lyrics ±250 ms"),
        ("j k", "scroll plain lyrics"),
        ("b", "ambient backdrop"),
        ("v", "cycle layout"),
        ("1 2 3", "cover · split · lyrics"),
        ("o", "bring Spotify to front"),
        ("y", "copy track link"),
        ("R", "reload art & lyrics"),
        ("?", "this help"),
        ("q / esc", "quit"),
        ("", ""),
        ("mouse", "click bar to seek · wheel = volume"),
        ("", "click art to play/pause"),
    ];
    let w: u16 = 72.min(area.width.saturating_sub(2));
    let h: u16 = (rows.len() as u16 + 4).min(area.height.saturating_sub(1));
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    let r = Rect::new(x, y, w, h);
    let bg = Style::default().bg(theme.bg.color()).fg(theme.text.color());
    for yy in r.top()..r.bottom() {
        for xx in r.left()..r.right() {
            if let Some(c) = buf.cell_mut(Position::new(xx, yy)) {
                c.reset();
                c.set_symbol(" ");
                c.set_style(bg);
            }
        }
    }
    let border = st(theme.accent);
    for xx in r.left()..r.right() {
        put(buf, xx, r.top(), "─", border, 1);
        put(buf, xx, r.bottom() - 1, "─", border, 1);
    }
    for yy in r.top()..r.bottom() {
        put(buf, r.left(), yy, "│", border, 1);
        put(buf, r.right() - 1, yy, "│", border, 1);
    }
    put(buf, r.left(), r.top(), "╭", border, 1);
    put(buf, r.right() - 1, r.top(), "╮", border, 1);
    put(buf, r.left(), r.bottom() - 1, "╰", border, 1);
    put(buf, r.right() - 1, r.bottom() - 1, "╯", border, 1);
    put(buf, r.left() + 2, r.top(), " keys ", st(theme.accent).add_modifier(Modifier::BOLD), w);
    let mut yy = r.top() + 2;
    for (k, v) in rows.iter() {
        if yy >= r.bottom() - 1 {
            break;
        }
        put(buf, r.left() + 3, yy, k, st(theme.accent), 16);
        put(buf, r.left() + 19, yy, v, st(theme.text), w.saturating_sub(22));
        yy += 1;
    }
}

// --------------------------------------------------------------- layouts

fn draw_cover(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    draw_header(buf, Rect::new(area.x, area.y, area.width, 1), app, theme);
    let has_lyric_strip = app.show_lyrics
        && area.height >= 30
        && app.lyrics.as_ref().map(|l| !l.synced.is_empty()).unwrap_or(false);
    let strip_h: u16 = if has_lyric_strip { 4 } else { 0 };
    // rows: header(1) gap(1) art gap(1) title(2) artist(1) album(1) gap(1) [strip] progress(1) controls(1) margin(1)
    let bottom_h = 2 + 1 + 1 + 1 + strip_h + 1 + 1 + 1;
    let art_avail = Rect::new(
        area.x + 2,
        area.y + 2,
        area.width.saturating_sub(4),
        area.height.saturating_sub(2 + bottom_h),
    );
    let art = art_square(art_avail, app.picker.font_size());
    draw_art(buf, art, app, theme);
    let mut y = art.bottom() + 1;
    let text_r = Rect::new(area.x + 2, y, area.width.saturating_sub(4), 4);
    let used = draw_title_block(buf, text_r, app, theme, true);
    y += used.max(3) + 1;
    if has_lyric_strip {
        let strip = Rect::new(area.x + 2, y, area.width.saturating_sub(4), 3);
        draw_lyrics(buf, strip, app, theme, true);
        y += strip_h;
    }
    let bar_w = area.width.saturating_sub(8).min(art.width.max(40));
    let bar_x = area.x + (area.width - bar_w) / 2;
    draw_progress(buf, Rect::new(bar_x, y, bar_w, 1), app, theme);
    draw_controls(buf, Rect::new(area.x, y + 1, area.width, 1), app, theme);
}

fn draw_split(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    draw_header(buf, Rect::new(area.x, area.y, area.width, 1), app, theme);
    let body = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2 + 3));
    let art_col_w = (body.width as f32 * 0.46) as u16;
    let art_avail = Rect::new(body.x + 3, body.y, art_col_w.saturating_sub(3), body.height);
    let art = art_square(art_avail, app.picker.font_size());
    draw_art(buf, art, app, theme);

    let info_x = art.right() + 4;
    let info = Rect::new(info_x, body.y, area.right().saturating_sub(info_x + 3), body.height);
    // Vertically align the info column with the art.
    let info = Rect::new(info.x, art.y, info.width, art.height.max(6));
    let used = draw_title_block(buf, Rect::new(info.x, info.y, info.width, 4), app, theme, false);
    let rest = Rect::new(info.x, info.y + used + 1, info.width, info.height.saturating_sub(used + 1));
    if app.show_lyrics && app.lyrics_status != LyricsStatus::Idle {
        draw_lyrics(buf, rest, app, theme, false);
    } else {
        draw_meta(buf, rest, app, theme);
    }

    let y = area.bottom() - 3;
    draw_progress(buf, Rect::new(area.x + 3, y, area.width.saturating_sub(6), 1), app, theme);
    draw_controls(buf, Rect::new(area.x, y + 1, area.width, 1), app, theme);
}

fn draw_lyrics_layout(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    draw_header(buf, Rect::new(area.x, area.y, area.width, 1), app, theme);
    let band_h: u16 = 6.min(area.height.saturating_sub(8));
    let band = Rect::new(area.x + 3, area.y + 2, area.width.saturating_sub(6), band_h);
    let art = art_square(Rect::new(band.x, band.y, band.width / 2, band.height), app.picker.font_size());
    let art = Rect::new(band.x, band.y, art.width, art.height);
    draw_art(buf, art, app, theme);
    let tx = art.right() + 3;
    let text_r = Rect::new(tx, band.y, band.right().saturating_sub(tx), band.height);
    draw_title_block(buf, text_r, app, theme, false);

    let ly_y = band.bottom() + 1;
    let ly = Rect::new(area.x + 3, ly_y, area.width.saturating_sub(6), (area.bottom() - 3).saturating_sub(ly_y));
    if app.show_lyrics {
        draw_lyrics(buf, ly, app, theme, true);
    } else {
        put_centered(buf, ly, ly.y + ly.height / 2, "lyrics hidden · press l", st(theme.dim));
    }
    let y = area.bottom() - 3;
    draw_progress(buf, Rect::new(area.x + 3, y, area.width.saturating_sub(6), 1), app, theme);
    draw_controls(buf, Rect::new(area.x, y + 1, area.width, 1), app, theme);
}

fn draw_mini(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    let Some(t) = app.snap.track.clone() else { return };
    let glyph = match app.snap.state {
        PlayerState::Playing => "▶",
        PlayerState::Paused => "❚❚",
        PlayerState::Stopped => "■",
    };
    let pos = app.position_ms();
    let times = format!("{} / {}", fmt_time(pos), fmt_time(t.duration_ms));
    let mut x = area.x + 1;
    x += put(buf, x, area.y, glyph, st(theme.accent), area.width) + 1;
    let avail = area.width.saturating_sub(x - area.x + times.len() as u16 + 2) as usize;
    x += put(buf, x, area.y, &fit(&t.name, avail), st(theme.text).add_modifier(Modifier::BOLD), avail as u16);
    let avail2 = area.width.saturating_sub(x - area.x + times.len() as u16 + 2) as usize;
    if avail2 > 4 {
        put(buf, x, area.y, &fit(&format!(" — {}", t.artist), avail2), st(theme.muted), avail2 as u16);
    }
    put(buf, area.right().saturating_sub(times.len() as u16 + 1), area.y, &times, st(theme.dim), area.width);
    if area.height >= 2 {
        // Bar only (no times) across the width.
        let r = Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), 1);
        let dur = t.duration_ms.max(1);
        let frac = (pos as f64 / dur as f64).clamp(0.0, 1.0);
        let full = (frac * r.width as f64) as u16;
        app.hit.progress = Some(r);
        for i in 0..r.width {
            let tt = i as f32 / r.width.max(1) as f32;
            let c = if i < full { theme.gradient(tt) } else { theme.track };
            put(buf, r.x + i, r.y, "█", st(c), 1);
        }
    }
    app.hit.buttons.push((Rect::new(area.x, area.y, 3, 1), Action::PlayPause));
}

// --------------------------------------------------------------- browser

fn fill_rect(buf: &mut Buffer, r: Rect, style: Style) {
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            if let Some(c) = buf.cell_mut(Position::new(x, y)) {
                c.reset();
                c.set_symbol(" ");
                c.set_style(style);
            }
        }
    }
}

fn draw_frame(buf: &mut Buffer, r: Rect, style: Style) {
    if r.width < 2 || r.height < 2 {
        return;
    }
    for xx in r.left() + 1..r.right() - 1 {
        put(buf, xx, r.top(), "─", style, 1);
        put(buf, xx, r.bottom() - 1, "─", style, 1);
    }
    for yy in r.top() + 1..r.bottom() - 1 {
        put(buf, r.left(), yy, "│", style, 1);
        put(buf, r.right() - 1, yy, "│", style, 1);
    }
    put(buf, r.left(), r.top(), "╭", style, 1);
    put(buf, r.right() - 1, r.top(), "╮", style, 1);
    put(buf, r.left(), r.bottom() - 1, "╰", style, 1);
    put(buf, r.right() - 1, r.bottom() - 1, "╯", style, 1);
}

pub fn draw_browser(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    use crate::browser::{Focus, Row, Tab};

    let w = area.width.saturating_sub(6).min(110).max(30);
    let h = area.height.saturating_sub(6).min(36).max(8);
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + 2, w, h);
    app.hit.browser_panel = Some(r);
    fill_rect(buf, r, Style::default().bg(theme.bg.color()).fg(theme.text.color()));
    draw_frame(buf, r, st(theme.accent));
    let inner = Rect::new(r.x + 2, r.y + 1, r.width.saturating_sub(4), r.height.saturating_sub(2));

    // ---- setup / login screen
    if app.browser.setup {
        put(buf, r.left() + 2, r.top(), " connect Spotify ", st(theme.accent).add_modifier(Modifier::BOLD), w);
        let uri = app.config.redirect_uri();
        let lines: [(&str, bool); 8] = [
            ("Search and playlists use Spotify's Web API, which needs a (free) developer app on your account.", false),
            ("", false),
            ("1. Open https://developer.spotify.com/dashboard  (ctrl+o opens it) and create an app. Any name.", false),
            ("2. Set the Redirect URI to exactly:", false),
            (uri.as_str(), true),
            ("3. Copy the app's Client ID, paste it below and press enter.", false),
            ("   Your browser opens once to approve. Tokens stay on this Mac (~/.config/aura).", false),
            ("", false),
        ];
        let mut y = inner.y + 1;
        for (text, highlight) in lines {
            if text.is_empty() {
                y += 1;
                continue;
            }
            if highlight {
                put(buf, inner.x + 4, y, text, st(theme.accent).add_modifier(Modifier::BOLD), inner.width.saturating_sub(4));
                y += 1;
                continue;
            }
            for wl in wrap(text, inner.width as usize) {
                put(buf, inner.x, y, &wl, st(theme.text), inner.width);
                y += 1;
            }
        }
        let label = "client id ▸ ";
        put(buf, inner.x, y, label, st(theme.muted), inner.width);
        let field_x = inner.x + label.width() as u16;
        let field_w = inner.width.saturating_sub(label.width() as u16);
        fill_rect(buf, Rect::new(field_x, y, field_w, 1), Style::default().bg(theme.track.color()).fg(theme.text.color()));
        let shown = fit(&app.browser.setup_input, field_w.saturating_sub(1) as usize);
        put(buf, field_x, y, &shown, Style::default().bg(theme.track.color()).fg(theme.text.color()), field_w);
        let cx = field_x + shown.width() as u16;
        if cx < field_x + field_w {
            put(buf, cx, y, "▏", Style::default().bg(theme.track.color()).fg(theme.accent.color()), 1);
        }
        y += 2;
        for s in app.browser.setup_status.iter().rev().take(6).collect::<Vec<_>>().into_iter().rev() {
            for wl in wrap(s, inner.width as usize) {
                put(buf, inner.x, y, &wl, st(theme.muted), inner.width);
                y += 1;
            }
        }
        put(buf, inner.x, r.bottom() - 2, "esc closes · ctrl+u clears", st(theme.dim), inner.width);
        return;
    }

    // ---- tabs
    let mut x = r.left() + 2;
    for t in Tab::ALL {
        let label = format!(" {} ", t.title());
        let style = if t == app.browser.tab {
            Style::default().bg(theme.accent.color()).fg(theme.bg.color()).add_modifier(Modifier::BOLD)
        } else {
            st(theme.muted)
        };
        let wlab = put(buf, x, r.top(), &label, style, w);
        app.hit.buttons.push((Rect::new(x, r.top(), wlab, 1), Action::OpenBrowser(Some(t))));
        x += wlab + 1;
    }
    if let Some(name) = &app.me_name {
        let s = format!(" {name} ");
        put(buf, r.right().saturating_sub(2 + s.width() as u16), r.top(), &s, st(theme.dim), w);
    }

    let mut y = inner.y;
    // ---- search input
    if app.browser.tab == Tab::Search {
        let focused = app.browser.focus == Focus::Input;
        let field = Rect::new(inner.x, y, inner.width, 1);
        app.hit.browser_input = Some(field);
        let bg = if focused { theme.track } else { theme.bg };
        fill_rect(buf, field, Style::default().bg(bg.color()));
        let prompt = "🔍 ";
        let prompt = if prompt.width() > 2 { "/ " } else { prompt };
        put(buf, field.x, y, prompt, Style::default().bg(bg.color()).fg(theme.accent.color()), 3);
        let qx = field.x + 2;
        let qw = field.width.saturating_sub(2);
        if app.browser.query.is_empty() && !focused {
            put(buf, qx, y, "type to search songs, artists, albums, playlists", Style::default().bg(bg.color()).fg(theme.dim.color()), qw);
        } else {
            let shown = fit(&app.browser.query, qw.saturating_sub(1) as usize);
            put(buf, qx, y, &shown, Style::default().bg(bg.color()).fg(theme.text.color()), qw);
            if focused {
                let cx = qx + shown.width() as u16;
                put(buf, cx, y, "▏", Style::default().bg(bg.color()).fg(theme.accent.color()), 1);
            }
        }
        y += 2;
    }

    // ---- breadcrumb / title
    if let Some(title) = app.browser.view.title() {
        let crumb = if app.browser.stack.is_empty() { title.to_string() } else { format!("← {title}") };
        put(buf, inner.x, y, &fit(&crumb, inner.width as usize), st(theme.accent).add_modifier(Modifier::BOLD), inner.width);
        y += 1;
    } else if app.browser.tab == Tab::Search && !app.browser.searched.is_empty() && matches!(app.browser.view, crate::browser::View::Search(_)) {
        put(buf, inner.x, y, &fit(&format!("results for “{}”", app.browser.searched), inner.width as usize), st(theme.muted), inner.width);
        y += 1;
    }

    // ---- list
    let footer_y = r.bottom() - 2;
    let list = Rect::new(inner.x, y, inner.width, footer_y.saturating_sub(y));
    app.hit.browser_list = Some(list);
    if let Some(l) = &app.browser.loading {
        let dots = ((app.started.elapsed().as_millis() / 300) % 4) as usize;
        put(buf, list.x, list.y, &format!("{l}{}", ".".repeat(dots)), st(theme.muted), list.width);
    } else if let Some(e) = &app.browser.error {
        for (i, wl) in wrap(&format!("⚠ {e}"), list.width as usize).iter().take(4).enumerate() {
            put(buf, list.x, list.y + i as u16, wl, st(theme.muted), list.width);
        }
    } else {
        let rows = app.browser.rows();
        app.browser.ensure_visible(list.height as usize);
        let sel_style = Style::default().bg(theme.accent.color()).fg(theme.bg.color()).add_modifier(Modifier::BOLD);
        let list_focused = app.browser.focus == Focus::List;
        for (i, row) in rows.iter().skip(app.browser.scroll).take(list.height as usize).enumerate() {
            let idx = app.browser.scroll + i;
            let yy = list.y + i as u16;
            let selected = idx == app.browser.selected && list_focused;
            let base = if selected { sel_style } else { st(theme.text) };
            let dim = if selected { sel_style } else { st(theme.muted) };
            if selected {
                fill_rect(buf, Rect::new(list.x, yy, list.width, 1), sel_style);
            }
            match row {
                Row::Header(h) => {
                    put(buf, list.x, yy, &h.to_uppercase(), st(theme.dim).add_modifier(Modifier::BOLD), list.width);
                }
                Row::Note(n) => {
                    put(buf, list.x, yy, n, st(theme.dim), list.width);
                }
                Row::Track(t, _) => {
                    let dur = fmt_time(t.duration_ms);
                    let right_w = dur.len() as u16 + 1;
                    let left_w = list.width.saturating_sub(right_w + 1);
                    let name_w = (left_w as usize * 45 / 100).max(10);
                    let name = fit(&t.name, name_w);
                    let mut xx = list.x;
                    xx += put(buf, xx, yy, &name, base, left_w);
                    let rest = left_w.saturating_sub(xx - list.x);
                    let meta = if t.album.is_empty() || t.album == t.name {
                        format!("  {}", t.artists)
                    } else {
                        format!("  {}  ·  {}", t.artists, t.album)
                    };
                    put(buf, xx, yy, &fit(&meta, rest as usize), dim, rest);
                    put(buf, list.right() - right_w, yy, &dur, dim, right_w);
                }
                Row::Album(a) => {
                    let name_w = (list.width as usize * 45 / 100).max(10);
                    let mut xx = list.x;
                    xx += put(buf, xx, yy, &fit(&a.name, name_w), base, list.width);
                    let rest = list.width.saturating_sub(xx - list.x);
                    let meta = format!("  {}  ·  {}  ·  {} songs", a.artists, a.year, a.total_tracks);
                    put(buf, xx, yy, &fit(&meta, rest as usize), dim, rest);
                }
                Row::Artist(a) => {
                    let mut xx = list.x;
                    xx += put(buf, xx, yy, &fit(&a.name, list.width as usize), base, list.width);
                    let rest = list.width.saturating_sub(xx - list.x);
                    put(buf, xx, yy, "  artist", dim, rest);
                }
                Row::Playlist(p) => {
                    let name_w = (list.width as usize * 50 / 100).max(10);
                    let mut xx = list.x;
                    xx += put(buf, xx, yy, &fit(&p.name, name_w), base, list.width);
                    let rest = list.width.saturating_sub(xx - list.x);
                    let meta = if p.owner.is_empty() {
                        format!("  {} songs", p.total)
                    } else {
                        format!("  {}  ·  {} songs", p.owner, p.total)
                    };
                    put(buf, xx, yy, &fit(&meta, rest as usize), dim, rest);
                }
            }
        }
        if rows.len() > list.height as usize && list.height > 0 {
            // scrollbar
            let track_h = list.height as usize;
            let thumb = ((track_h * track_h) / rows.len()).max(1);
            let pos = (app.browser.scroll * track_h) / rows.len();
            for i in 0..track_h {
                let ch = if i >= pos && i < pos + thumb { "┃" } else { "│" };
                let style = if i >= pos && i < pos + thumb { st(theme.accent) } else { st(theme.track) };
                put(buf, r.right() - 2, list.y + i as u16, ch, style, 1);
            }
        }
    }

    // ---- footer
    let hint = match app.browser.tab {
        Tab::Search if app.browser.focus == Focus::Input => "enter search · ↓ results · tab switch · esc close",
        _ => "enter play · a add to queue · p play all · ← back · tab switch · esc close",
    };
    put(buf, inner.x, footer_y, &fit(hint, inner.width as usize), st(theme.dim), inner.width);
}
