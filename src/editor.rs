//! UTF-8 terminal editing. The command reader never buffers input past the
//! accepted line, so shell commands can read the same underlying stdin.
use crate::{
    plugin_editor::{
        EditorEffects, EditorEvent, EditorFrame, EditorPlugin, EditorRender, EditorStyle,
    },
    session::{Session, TerminalColors},
};
use std::io::{self, Write};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub enum Read {
    Text(Vec<u8>),
    Interrupt,
    Eof,
}

pub struct Editor {
    draft: State,
    basic: Vec<u8>,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            draft: State::default(),
            basic: Vec::new(),
        }
    }

    pub fn read_line(
        &mut self,
        prompt: &str,
        session: &Session,
        plugins: Option<&mut dyn EditorPlugin>,
    ) -> io::Result<Read> {
        let mut output = io::stderr().lock();
        // SAFETY: isatty reads these descriptor numbers without pointers.
        let terminal = unsafe {
            libc::isatty(libc::STDIN_FILENO) == 1 && libc::isatty(libc::STDERR_FILENO) == 1
        } && std::env::var("TERM")
            .is_ok_and(|term| term != "dumb" && !term.is_empty());
        if !terminal {
            output.write_all(prompt.as_bytes())?;
            output.flush()?;
            return basic_line(&mut self.basic);
        }
        let options = Terminal::new(session)?;
        let plugin_history = plugins.as_ref().and_then(|plugin| plugin.history());
        let history = plugin_history.as_deref().or_else(|| {
            (session.enabled("history") && session.flag("history.enabled"))
                .then(|| session.history())
        });
        let settings = Settings {
            session,
            history,
            limit: session.number("editor.max_bytes"),
            escape_timeout: session.number("editor.escape_timeout_ms") as i32,
            paste: options.paste,
        };
        let mut state = std::mem::take(&mut self.draft);
        let mut display = Display::new(prompt);
        let result = edit(&settings, &mut state, &mut display, &mut output, plugins);
        if result
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::Interrupted)
        {
            self.draft = state;
            let _ = output.write_all(b"\r\n");
            let _ = output.flush();
        }
        drop(output);
        drop(options);
        result
    }
}

struct Terminal {
    original: libc::termios,
    paste: bool,
    cursor: bool,
}

impl Terminal {
    fn new(session: &Session) -> io::Result<Self> {
        // SAFETY: tcgetattr initializes the struct on success. The local copy
        // passed to cfmakeraw and tcsetattr remains live and properly aligned.
        let original = unsafe {
            let mut original = std::mem::MaybeUninit::uninit();
            if libc::tcgetattr(libc::STDIN_FILENO, original.as_mut_ptr()) == -1 {
                return Err(io::Error::last_os_error());
            }
            let original = original.assume_init();
            let mut raw = original;
            libc::cfmakeraw(&mut raw);
            raw.c_oflag = original.c_oflag;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) == -1 {
                return Err(io::Error::last_os_error());
            }
            original
        };
        let paste = session.flag("editor.bracketed_paste");
        let style = session.get("cursor.style");
        let cursor = style != "default";
        let terminal = Self {
            original,
            paste,
            cursor,
        };
        let mut output = io::stderr().lock();
        if paste {
            output.write_all(b"\x1b[?2004h")?;
        }
        if cursor {
            let (blinking, steady) = match style {
                "block" => (1, 2),
                "underline" => (3, 4),
                "bar" => (5, 6),
                _ => (0, 0),
            };
            let shape = if session.flag("cursor.blink") {
                blinking
            } else {
                steady
            };
            write!(output, "\x1b[{shape} q")?;
        }
        output.flush()?;
        Ok(terminal)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: self owns the valid attributes copied from fd 0.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original) };
        let mut output = io::stderr().lock();
        if self.paste {
            let _ = output.write_all(b"\x1b[?2004l");
        }
        if self.cursor {
            let _ = output.write_all(b"\x1b[0 q");
        }
        let _ = output.flush();
    }
}

fn basic_line(bytes: &mut Vec<u8>) -> io::Result<Read> {
    loop {
        if !ready(100)? {
            continue;
        }
        match read_byte()? {
            Some(b'\n') => return Ok(Read::Text(std::mem::take(bytes))),
            Some(byte) => bytes.push(byte),
            None if bytes.is_empty() => return Ok(Read::Eof),
            None => return Ok(Read::Text(std::mem::take(bytes))),
        }
    }
}

fn read_byte() -> io::Result<Option<u8>> {
    let mut byte: u8 = 0;
    // SAFETY: libc::read receives a writable one-byte buffer.
    match unsafe { libc::read(libc::STDIN_FILENO, (&mut byte as *mut u8).cast(), 1) } {
        0 => Ok(None),
        1 => Ok(Some(byte)),
        _ => Err(io::Error::last_os_error()),
    }
}

fn ready(timeout: i32) -> io::Result<bool> {
    if crate::sys::signal_waiting() {
        return Err(io::Error::from(io::ErrorKind::Interrupted));
    }
    let mut input = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: input points to one initialized descriptor slot.
    let result = unsafe { libc::poll(&mut input, 1, timeout) };
    if result >= 0 {
        return Ok(result > 0);
    }
    Err(io::Error::last_os_error())
}

#[derive(Default)]
struct State {
    input: String,
    cursor: usize,
    history_at: Option<usize>,
    draft: Option<(String, usize)>,
    search: Option<Search>,
}

struct Search {
    query: String,
    next: usize,
    original: (String, usize),
}

impl State {
    fn insert(&mut self, value: &str, limit: usize) -> bool {
        if let Some(search) = &mut self.search {
            if search.query.len() + value.len() > limit {
                return false;
            }
            search.query.push_str(value);
            return true;
        }
        if self.input.len() + value.len() > limit {
            return false;
        }
        self.input.insert_str(self.cursor, value);
        self.cursor += value.len();
        if let Some((span, _)) = self
            .input
            .grapheme_indices(true)
            .find(|(span, _)| *span >= self.cursor)
        {
            self.cursor = span;
        } else {
            self.cursor = self.input.len();
        }
        true
    }

    /// Change only ranges whose endpoints agree with the visual editor.
    fn apply_editor(&mut self, effects: EditorEffects, limit: usize) -> EditorChange {
        let mut changed = false;
        if let Some(value) = effects.replacement {
            if value.start <= value.end
                && self.boundary(value.start)
                && self.boundary(value.end)
                && self.input.len() - (value.end - value.start) + value.text.len() <= limit
            {
                changed = self.input[value.start..value.end] != value.text;
                let cursor = value.start + value.text.len();
                self.input
                    .replace_range(value.start..value.end, &value.text);
                let aligned = self
                    .input
                    .grapheme_indices(true)
                    .find_map(|(position, _)| (position >= cursor).then_some(position))
                    .unwrap_or(self.input.len());
                changed |= self.cursor != aligned;
                self.cursor = aligned;
            }
        }
        if let Some(cursor) = effects.cursor.filter(|cursor| self.boundary(*cursor)) {
            changed |= self.cursor != cursor;
            self.cursor = cursor;
        }
        EditorChange {
            consumed: effects.consume,
            changed,
            render: effects.render,
        }
    }

    fn boundary(&self, position: usize) -> bool {
        position == self.input.len()
            || (self.input.is_char_boundary(position)
                && self
                    .input
                    .grapheme_indices(true)
                    .any(|(index, _)| index == position))
    }

    fn history_move(&mut self, history: Option<&[Vec<u8>]>, direction: i8) -> bool {
        let Some(history) = history else {
            return false;
        };
        if direction < 0 {
            let Some(previous) = self
                .history_at
                .unwrap_or(history.len())
                .min(history.len())
                .checked_sub(1)
            else {
                return false;
            };
            if self.history_at.is_none() {
                self.draft = Some((self.input.clone(), self.cursor));
            }
            self.history_at = Some(previous);
            self.input = String::from_utf8_lossy(&history[previous]).into_owned();
            self.cursor = self.input.len();
            return true;
        }
        let Some(next) = self.history_at.map(|at| at + 1) else {
            return false;
        };
        if let Some(line) = history.get(next) {
            self.history_at = Some(next);
            self.input = String::from_utf8_lossy(line).into_owned();
            self.cursor = self.input.len();
        } else {
            self.history_at = None;
            (self.input, self.cursor) = self.draft.take().unwrap_or_default();
        }
        true
    }

    fn history_search(&mut self, history: Option<&[Vec<u8>]>, fold: bool, reset: bool) -> bool {
        let Some(history) = history else {
            return false;
        };
        let search = self.search.get_or_insert_with(|| Search {
            query: String::new(),
            next: history.len(),
            original: (self.input.clone(), self.cursor),
        });
        let query = if fold {
            search.query.to_lowercase()
        } else {
            search.query.clone()
        };
        if reset {
            search.next = history.len();
        }
        for (index, line) in history[..search.next.min(history.len())]
            .iter()
            .enumerate()
            .rev()
        {
            let line = String::from_utf8_lossy(line);
            let found = if fold {
                line.to_lowercase().contains(&query)
            } else {
                line.contains(&query)
            };
            if found {
                self.input = line.into_owned();
                self.cursor = self.input.len();
                search.next = index;
                return true;
            }
        }
        if reset {
            (self.input, self.cursor) = search.original.clone();
        }
        false
    }
}

struct Settings<'a> {
    session: &'a Session,
    history: Option<&'a [Vec<u8>]>,
    limit: usize,
    escape_timeout: i32,
    paste: bool,
}

enum Key {
    Named(String),
    Text(String),
    Ignore,
}

fn keys(byte: u8, settings: &Settings<'_>) -> io::Result<Key> {
    let name = match byte {
        b'\r' => "Enter".into(),
        b'\t' => "Tab".into(),
        8 | 127 => "Backspace".into(),
        27 => return escaped(settings),
        1..=26 => format!("Ctrl-{}", char::from(b'A' + byte - 1)),
        b' '..=b'~' => return Ok(Key::Text(char::from(byte).to_string())),
        0xc2..=0xf4 => {
            let count = if byte <= 0xdf {
                2
            } else if byte <= 0xef {
                3
            } else {
                4
            };
            let mut bytes = vec![byte];
            for _ in 1..count {
                if !ready(settings.escape_timeout)? {
                    break;
                }
                if let Some(byte) = read_byte()? {
                    bytes.push(byte);
                }
            }
            return Ok(String::from_utf8(bytes).map_or(Key::Ignore, Key::Text));
        }
        _ => return Ok(Key::Ignore),
    };
    Ok(Key::Named(name))
}

fn escaped(settings: &Settings<'_>) -> io::Result<Key> {
    if !ready(settings.escape_timeout)? {
        return Ok(Key::Named("Escape".into()));
    }
    let Some(first) = read_byte()? else {
        return Ok(Key::Ignore);
    };
    if first.is_ascii_lowercase() {
        return Ok(Key::Named(format!("Alt-{}", char::from(first))));
    }
    if !matches!(first, b'[' | b'O') {
        return Ok(Key::Ignore);
    }
    let mut seq = vec![first];
    for _ in 0..12 {
        if !ready(settings.escape_timeout)? {
            break;
        }
        let Some(byte) = read_byte()? else { break };
        seq.push(byte);
        if (b'@'..=b'~').contains(&byte) {
            break;
        }
    }
    let name = match seq.as_slice() {
        b"[A" | b"OA" => "Up",
        b"[B" | b"OB" => "Down",
        b"[C" | b"OC" => "Right",
        b"[D" | b"OD" => "Left",
        b"[H" | b"OH" | b"[1~" | b"[7~" => "Home",
        b"[F" | b"OF" | b"[4~" | b"[8~" => "End",
        b"[2~" => "Insert",
        b"[3~" => "Delete",
        b"[5~" => "PageUp",
        b"[6~" => "PageDown",
        b"OP" | b"[11~" => "F1",
        b"OQ" | b"[12~" => "F2",
        b"OR" | b"[13~" => "F3",
        b"OS" | b"[14~" => "F4",
        b"[15~" => "F5",
        b"[17~" => "F6",
        b"[18~" => "F7",
        b"[19~" => "F8",
        b"[20~" => "F9",
        b"[21~" => "F10",
        b"[23~" => "F11",
        b"[24~" => "F12",
        [
            b'[',
            b'1',
            b';',
            modifier @ (b'2' | b'3' | b'5'),
            direction @ (b'A' | b'B' | b'C' | b'D'),
        ] => {
            let prefix = match modifier {
                b'2' => "Shift",
                b'3' => "Alt",
                _ => "Ctrl",
            };
            let direction = match direction {
                b'A' => "Up",
                b'B' => "Down",
                b'C' => "Right",
                _ => "Left",
            };
            return Ok(Key::Named(format!("{prefix}-{direction}")));
        }
        b"[200~" if settings.paste => return paste(settings.limit).map(Key::Text),
        _ => return Ok(Key::Ignore),
    };
    Ok(Key::Named(name.into()))
}

fn paste(limit: usize) -> io::Result<String> {
    const END: &[u8] = b"\x1b[201~";
    let mut out = Vec::new();
    let mut possible = 0;
    while let Some(byte) = read_byte()? {
        if byte == END[possible] {
            possible += 1;
            if possible == END.len() {
                break;
            }
            continue;
        }
        if out.len() < limit + END.len() {
            out.extend_from_slice(&END[..possible]);
            if byte != END[0] {
                out.push(byte);
            }
        }
        possible = usize::from(byte == END[0]);
    }
    if out.len() > limit {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&out)
        .replace("\r\n", "\n")
        .replace('\r', "\n"))
}

fn edit(
    settings: &Settings<'_>,
    state: &mut State,
    display: &mut Display<'_>,
    output: &mut impl Write,
    mut plugins: Option<&mut dyn EditorPlugin>,
) -> io::Result<Read> {
    let mut render = EditorRender::default();
    refresh(&mut plugins, settings, state, &mut render);
    display.draw(settings.session, state, &render, output)?;
    loop {
        let pending = plugins
            .as_ref()
            .is_some_and(|host| host.has_pending_editor_work());
        if !ready(if pending { 30 } else { 100 })? {
            if crate::plugin_fs::has_retired() {
                crate::plugin_fs::reap_retired();
            }
            let mut redraw = terminal_size() != display.size;
            if pending && state.search.is_none() {
                let update = notify(&mut plugins, EditorEvent::Tick, settings, state);
                redraw |= update.changed || replace_render(&mut render, update.render);
                if update.changed {
                    refresh(&mut plugins, settings, state, &mut render);
                }
            }
            if redraw {
                display.draw(settings.session, state, &render, output)?;
            }
            continue;
        }
        let Some(byte) = read_byte()? else {
            return Ok(Read::Eof);
        };
        match keys(byte, settings)? {
            Key::Text(text) => {
                if !state.insert(&text, settings.limit) && settings.session.flag("editor.beep") {
                    output.write_all(b"\x07")?;
                }
                if state.search.is_some() {
                    state.history_search(
                        settings.history,
                        settings.session.flag("history.search_ignore_case"),
                        true,
                    );
                }
            }
            Key::Named(key) => {
                let action = settings.session.get(&format!("key.{key}"));
                if state.search.is_none() {
                    let update = notify(
                        &mut plugins,
                        EditorEvent::Key { key: &key, action },
                        settings,
                        state,
                    );
                    replace_render(&mut render, update.render);
                    if update.consumed {
                        if update.changed {
                            refresh(&mut plugins, settings, state, &mut render);
                        }
                        display.draw(settings.session, state, &render, output)?;
                        continue;
                    }
                }
                match action {
                    "accept-line" if state.search.is_some() => state.search = None,
                    "accept-line" => {
                        state.cursor = state.input.len();
                        display.draw(settings.session, state, &EditorRender::default(), output)?;
                        output.write_all(b"\r\n")?;
                        output.flush()?;
                        return Ok(Read::Text(state.input.as_bytes().to_vec()));
                    }
                    "interrupt" => {
                        state.input.clear();
                        state.cursor = 0;
                        state.search = None;
                        display.draw(settings.session, state, &EditorRender::default(), output)?;
                        if settings.session.flag("editor.control_echo") {
                            output.write_all(b"^C")?;
                        }
                        output.write_all(b"\r\n")?;
                        output.flush()?;
                        return Ok(Read::Interrupt);
                    }
                    "eof" if state.input.is_empty() && state.search.is_none() => {
                        output.write_all(b"\r\n")?;
                        output.flush()?;
                        return Ok(Read::Eof);
                    }
                    "history-search" => {
                        if !state.history_search(
                            settings.history,
                            settings.session.flag("history.search_ignore_case"),
                            false,
                        ) && settings.session.flag("editor.beep")
                        {
                            output.write_all(b"\x07")?;
                        }
                    }
                    "delete-left" if state.search.is_some() => {
                        if let Some((index, _)) = state
                            .search
                            .as_ref()
                            .unwrap()
                            .query
                            .grapheme_indices(true)
                            .next_back()
                        {
                            state.search.as_mut().unwrap().query.truncate(index);
                            state.history_search(
                                settings.history,
                                settings.session.flag("history.search_ignore_case"),
                                true,
                            );
                        }
                    }
                    _ if key == "Escape" && state.search.is_some() => {
                        (state.input, state.cursor) = state.search.take().unwrap().original;
                    }
                    _ if state.search.is_some() => (),
                    "history-previous" | "history-next" => {
                        let direction = if action == "history-previous" { -1 } else { 1 };
                        if !state.history_move(settings.history, direction)
                            && settings.session.flag("editor.beep")
                        {
                            output.write_all(b"\x07")?;
                        }
                    }
                    "clear-screen" => {
                        output.write_all(b"\x1b[H\x1b[2J")?;
                        display.previous_row = 0;
                    }
                    other => state.edit(other, settings.limit),
                }
            }
            Key::Ignore => continue,
        }
        refresh(&mut plugins, settings, state, &mut render);
        display.draw(settings.session, state, &render, output)?;
    }
}

#[derive(Default)]
struct EditorChange {
    consumed: bool,
    changed: bool,
    render: Option<EditorRender>,
}

fn notify(
    plugins: &mut Option<&mut dyn EditorPlugin>,
    event: EditorEvent<'_>,
    settings: &Settings<'_>,
    state: &mut State,
) -> EditorChange {
    let Some(plugin) = plugins.as_deref_mut() else {
        return EditorChange::default();
    };
    let effects = plugin.on_editor(
        event,
        EditorFrame {
            line: &state.input,
            cursor: state.cursor,
            history: settings.history.unwrap_or_default(),
        },
    );
    state.apply_editor(effects, settings.limit)
}

fn replace_render(render: &mut EditorRender, update: Option<EditorRender>) -> bool {
    let Some(update) = update else {
        return false;
    };
    if update == *render {
        return false;
    }
    *render = update;
    true
}

fn refresh(
    plugins: &mut Option<&mut dyn EditorPlugin>,
    settings: &Settings<'_>,
    state: &mut State,
    render: &mut EditorRender,
) {
    if plugins.is_some() {
        *render = if state.search.is_some() {
            EditorRender::default()
        } else {
            notify(plugins, EditorEvent::Change, settings, state)
                .render
                .unwrap_or_default()
        };
    }
}

impl State {
    fn edit(&mut self, action: &str, limit: usize) {
        let before = || {
            self.input[..self.cursor]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(at, _)| at)
        };
        let after = || {
            self.cursor
                + self.input[self.cursor..]
                    .graphemes(true)
                    .next()
                    .map_or(0, str::len)
        };
        match action {
            "move-left" => self.cursor = before(),
            "move-right" => self.cursor = after(),
            "move-home" => {
                self.cursor = self.input[..self.cursor]
                    .rfind('\n')
                    .map_or(0, |index| index + 1)
            }
            "move-end" => {
                self.cursor = self.input[self.cursor..]
                    .find('\n')
                    .map_or(self.input.len(), |index| self.cursor + index)
            }
            "move-word-left" => self.cursor = word_left(&self.input, self.cursor),
            "move-word-right" => {
                let later = &self.input[self.cursor..];
                let word_start = later
                    .find(|ch: char| !ch.is_whitespace())
                    .unwrap_or(later.len());
                self.cursor += later[word_start..]
                    .find(char::is_whitespace)
                    .map_or(later.len(), |index| word_start + index);
            }
            "delete-left" => {
                let position = before();
                self.input.drain(position..self.cursor);
                self.cursor = position;
            }
            "delete-right" | "eof" => {
                self.input.drain(self.cursor..after());
            }
            "delete-word" => {
                let position = word_left(&self.input, self.cursor);
                self.input.drain(position..self.cursor);
                self.cursor = position;
            }
            "clear-line" => {
                self.input.clear();
                self.cursor = 0;
            }
            "insert-newline" => {
                self.insert("\n", limit);
            }
            _ => (),
        }
    }
}

fn word_left(input: &str, at: usize) -> usize {
    input[..at]
        .trim_end()
        .rfind(char::is_whitespace)
        .map_or(0, |index| {
            index + input[index..].chars().next().map_or(1, char::len_utf8)
        })
}

fn terminal_size() -> (usize, usize) {
    // SAFETY: ioctl writes at most one winsize to the supplied pointer.
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut size) == 0
            && size.ws_col > 0
            && size.ws_row > 0
        {
            return (size.ws_row as usize, size.ws_col as usize);
        }
    }
    (24, 80)
}

struct Display<'a> {
    size: (usize, usize),
    previous_row: usize,
    prompt: &'a str,
}

impl<'a> Display<'a> {
    fn new(prompt: &'a str) -> Self {
        Self {
            size: terminal_size(),
            previous_row: 0,
            prompt,
        }
    }

    fn draw(
        &mut self,
        session: &Session,
        state: &State,
        render: &EditorRender,
        output: &mut impl Write,
    ) -> io::Result<()> {
        self.size = terminal_size();
        let (height, width) = self.size;
        let mut layout = Layout::new(width, session.get("editor.ambiguous_width") == "2");
        if let Some(search) = &state.search {
            layout.prompt(session.get("editor.search_prompt"));
            for grapheme in search.query.graphemes(true) {
                layout.grapheme(grapheme, None);
            }
            layout.newline();
        }
        layout.prompt(self.prompt);
        let colors = (render.highlights.is_some()
            || render.ghost.is_some()
            || !render.virtual_text.is_empty()
            || render.menu.is_some())
        .then(|| session.terminal_colors());
        let ranges = render.highlights.as_ref().map(|spans| {
            spans
                .iter()
                .take(512)
                .filter_map(|span| {
                    if span.start < span.end
                        && span.end <= state.input.len()
                        && state.input.is_char_boundary(span.start)
                        && state.input.is_char_boundary(span.end)
                    {
                        ui_style(colors, span.style.as_ref(), Some(&span.color))
                            .map(|style| (span.start..span.end, style))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        });
        let palette = ranges
            .is_none()
            .then(|| Highlight::new(session, &state.input));
        let virtual_text = render
            .virtual_text
            .iter()
            .take(16)
            .map(|text| {
                (
                    text,
                    ui_style(colors, text.style.as_ref(), text.color.as_deref()),
                )
            })
            .collect::<Vec<_>>();
        let append_virtual = |layout: &mut Layout, offset| {
            for (text, style) in &virtual_text {
                if text.offset == offset {
                    layout.text(limit_bytes(&text.text, 2048), style.as_deref());
                }
            }
        };
        let mut cursor = None;
        for (index, grapheme) in state.input.grapheme_indices(true) {
            if index == state.cursor {
                cursor = Some(layout.position());
            }
            append_virtual(&mut layout, index);
            let color = if let Some(ranges) = &ranges {
                ranges
                    .iter()
                    .rev()
                    .find(|(range, _)| range.contains(&index))
                    .map(|(_, style)| style.as_str())
            } else {
                palette.as_ref().and_then(|palette| palette.color(index))
            };
            layout.grapheme(grapheme, color);
        }
        let (row, col) = cursor.unwrap_or_else(|| layout.position());
        append_virtual(&mut layout, state.input.len());
        if state.cursor == state.input.len() {
            if let Some(ghost) = &render.ghost {
                let spec = render.ghost_style.unwrap_or_else(EditorStyle::ghost);
                let style = colors.and_then(|colors| colors.structured(&spec));
                if let Some(style) = &style {
                    layout.text(limit_bytes(ghost, 4096), Some(style));
                } else if !ghost.is_empty() {
                    layout.text("⟨", None);
                    layout.text(limit_bytes(ghost, 4096), None);
                    layout.text("⟩", None);
                }
            }
        }
        if let Some(menu) = &render.menu {
            let first = menu.selected.map_or(0, |index| index.saturating_sub(7));
            for (index, item) in menu.items.iter().enumerate().skip(first).take(8) {
                let selected = menu.selected == Some(index);
                let spec = selected
                    .then_some(menu.selected_style.as_ref())
                    .flatten()
                    .or(item.style.as_ref())
                    .or(menu.style.as_ref());
                let style = ui_style(colors, spec, None);
                layout.newline();
                if menu.selected.is_some() {
                    layout.text(if selected { "> " } else { "  " }, style.as_deref());
                }
                layout.text(
                    limit_bytes(item.text.lines().next().unwrap_or_default(), 256),
                    style.as_deref(),
                );
            }
        }
        layout.finish_color();
        let start = row.saturating_sub(height - 1);
        let end = layout.lines.len().min(start + height);
        output.write_all(b"\r")?;
        if self.previous_row > 0 {
            write!(output, "\x1b[{}A", self.previous_row)?;
        }
        output.write_all(b"\x1b[J")?;
        for (index, line) in layout.lines[start..end].iter().enumerate() {
            if index > 0 {
                output.write_all(b"\r\n")?;
            }
            output.write_all(line.as_bytes())?;
        }
        output.write_all(b"\r")?;
        if end - 1 > row {
            write!(output, "\x1b[{}A", end - 1 - row)?;
        }
        if col > 0 {
            write!(output, "\x1b[{col}C")?;
        }
        self.previous_row = row - start;
        output.flush()
    }
}

fn ui_style(
    colors: Option<TerminalColors>,
    style: Option<&EditorStyle>,
    legacy: Option<&str>,
) -> Option<String> {
    let colors = colors?;
    match style {
        Some(spec) => colors.structured(spec),
        None => colors.style(legacy?),
    }
}

fn limit_bytes(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

struct Layout {
    lines: Vec<String>,
    col: usize,
    width: usize,
    cjk: bool,
    line_color: String,
}

impl Layout {
    fn new(width: usize, cjk: bool) -> Self {
        Self {
            lines: vec![String::new()],
            col: 0,
            width,
            cjk,
            line_color: String::new(),
        }
    }

    fn position(&self) -> (usize, usize) {
        (self.lines.len() - 1, self.col)
    }

    fn newline(&mut self) {
        self.finish_color();
        self.lines.push(String::new());
        self.col = 0;
    }

    fn text(&mut self, text: &str, color: Option<&str>) {
        for grapheme in text.graphemes(true) {
            self.grapheme(grapheme, color);
        }
    }

    fn prompt(&mut self, mut prompt: &str) {
        let mut color = String::new();
        while !prompt.is_empty() {
            let next = prompt.find('\x1b').unwrap_or(prompt.len());
            for grapheme in prompt[..next].graphemes(true) {
                self.grapheme(grapheme, (!color.is_empty()).then_some(color.as_str()));
            }
            if next == prompt.len() {
                break;
            }
            prompt = &prompt[next + 1..];
            if let Some(csi) = prompt.strip_prefix('[') {
                let end = csi
                    .as_bytes()
                    .iter()
                    .position(|byte| (b'@'..=b'~').contains(byte));
                let Some(end) = end else { break };
                if csi.as_bytes()[end] == b'm'
                    && csi[..end]
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || byte == b';')
                {
                    if matches!(&csi[..end], "" | "0") {
                        color.clear();
                    } else {
                        color.push_str("\x1b[");
                        color.push_str(&csi[..=end]);
                    }
                }
                prompt = &csi[end + 1..];
            }
        }
        self.finish_color();
    }

    fn finish_color(&mut self) {
        if !self.line_color.is_empty() {
            self.lines.last_mut().unwrap().push_str("\x1b[0m");
            self.line_color.clear();
        }
    }

    fn grapheme(&mut self, grapheme: &str, color: Option<&str>) {
        if grapheme == "\n" {
            self.newline();
            return;
        }
        if grapheme.chars().any(|ch| {
            ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        }) {
            for ch in grapheme.escape_default().to_string().graphemes(true) {
                self.grapheme(ch, color);
            }
            return;
        }
        let width = if self.cjk {
            grapheme.width_cjk()
        } else {
            grapheme.width()
        };
        if self.col + width > self.width {
            self.newline();
        }
        let color = color.unwrap_or_default();
        if color != self.line_color {
            self.finish_color();
            self.lines.last_mut().unwrap().push_str(color);
            self.line_color.push_str(color);
        }
        self.lines.last_mut().unwrap().push_str(grapheme);
        self.col += width.min(self.width);
        if self.col == self.width {
            self.newline();
        }
    }
}

const COLOR_KEYS: &[&str] = &[
    "highlighting.command",
    "highlighting.argument",
    "highlighting.single_quote",
    "highlighting.double_quote",
    "highlighting.variable",
    "highlighting.operator",
    "highlighting.redirect",
    "highlighting.number",
    "highlighting.assignment",
    "highlighting.comment",
    "highlighting.escape",
    "highlighting.glob",
];

struct Highlight {
    colors: Vec<Option<String>>,
    indices: Vec<usize>,
}

impl Highlight {
    fn new(session: &Session, input: &str) -> Self {
        if !session.enabled("highlighting") {
            return Self {
                colors: Vec::new(),
                indices: Vec::new(),
            };
        }
        let colors = session.colors(COLOR_KEYS);
        if colors.iter().all(Option::is_none) {
            return Self {
                colors,
                indices: Vec::new(),
            };
        }
        let bytes = input.as_bytes();
        let mut indices = vec![1; bytes.len()];
        let mut command = true;
        let mut at = 0;
        while at < bytes.len() {
            let start = at;
            let byte = bytes[at];
            let kind = match byte {
                b'#' if at == 0 || bytes[at - 1].is_ascii_whitespace() => {
                    at += bytes[at..]
                        .iter()
                        .position(|byte| *byte == b'\n')
                        .unwrap_or(bytes.len() - at);
                    9
                }
                b'\'' | b'"' => {
                    at += 1;
                    while at < bytes.len() {
                        if bytes[at] == byte {
                            at += 1;
                            break;
                        }
                        at += 1 + usize::from(
                            byte == b'"' && bytes[at] == b'\\' && at + 1 < bytes.len(),
                        );
                    }
                    command = false;
                    if byte == b'\'' { 2 } else { 3 }
                }
                b'\\' => {
                    at = (at + 2).min(bytes.len());
                    10
                }
                b'$' => {
                    at += 1;
                    if bytes.get(at) == Some(&b'{') {
                        at += bytes[at..]
                            .iter()
                            .position(|byte| *byte == b'}')
                            .map_or(bytes.len() - at, |end| end + 1);
                    } else {
                        at += bytes[at..]
                            .iter()
                            .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
                            .count();
                        if at == start + 1
                            && bytes.get(at).is_some_and(|byte| b"?*#@!$-".contains(byte))
                        {
                            at += 1;
                        }
                    }
                    4
                }
                b'>' | b'<' => {
                    at += 1;
                    6
                }
                b'|' | b'&' | b';' | b'(' | b')' | b'\n' => {
                    command = true;
                    at += 1;
                    5
                }
                b'*' | b'?' | b'[' | b']' => {
                    at += 1;
                    11
                }
                byte if byte.is_ascii_whitespace() => {
                    at += 1;
                    1
                }
                _ => {
                    at += bytes[at..]
                        .iter()
                        .take_while(|byte| {
                            !byte.is_ascii_whitespace() && !b"'\"\\$><|&;()*?[]".contains(byte)
                        })
                        .count();
                    let word = &bytes[start..at];
                    if command && word.contains(&b'=') {
                        8
                    } else if command {
                        command = false;
                        0
                    } else if word.iter().all(u8::is_ascii_digit) {
                        7
                    } else {
                        1
                    }
                }
            };
            indices[start..at].fill(kind);
        }
        Self { colors, indices }
    }

    fn color(&self, index: usize) -> Option<&str> {
        self.indices
            .get(index)
            .and_then(|kind| self.colors[*kind].as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_respects_grapheme_clusters() {
        let mut state = State::default();
        state.insert("e\u{301}😀", 100);
        state.edit("move-left", 100);
        state.edit("delete-left", 100);
        assert_eq!(state.input, "😀");
        assert_eq!(state.cursor, 0);
        state.edit("delete-right", 100);
        assert!(state.input.is_empty());
    }

    #[test]
    fn plugin_edits_require_grapheme_boundaries_and_preserve_unicode() {
        use crate::plugin_editor::EditorReplacement;

        let mut state = State::default();
        state.insert("e\u{301}🐚", 100);
        let change = state.apply_editor(
            EditorEffects {
                replacement: Some(EditorReplacement {
                    start: 1,
                    end: 3,
                    text: "z".into(),
                }),
                cursor: Some(1),
                ..EditorEffects::default()
            },
            100,
        );
        assert!(!change.changed);
        assert_eq!(state.input, "e\u{301}🐚");
        let change = state.apply_editor(
            EditorEffects {
                replacement: Some(EditorReplacement {
                    start: 0,
                    end: 3,
                    text: "界".into(),
                }),
                cursor: Some(0),
                ..EditorEffects::default()
            },
            100,
        );
        assert!(change.changed);
        assert_eq!(state.input, "界🐚");
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn layout_accounts_for_ansi_combining_and_wide_glyphs() {
        let mut layout = Layout::new(5, false);
        layout.prompt("\x1b[31mλ\x1b[0m ");
        layout.grapheme("e\u{301}", None);
        assert_eq!(layout.position(), (0, 3));
        layout.grapheme("😺", None);
        assert_eq!(layout.position(), (1, 0));
    }

    #[test]
    fn prompt_color_runs_are_contiguous_and_reset_before_wrapping() {
        let mut layout = Layout::new(4, false);
        layout.prompt("\x1b[31mab界d\x1b[0m\nλ ");
        assert_eq!(
            layout.lines,
            ["\x1b[31mab界\x1b[0m", "\x1b[31md\x1b[0m", "λ "]
        );
        assert_eq!(layout.position(), (2, 2));
    }

    #[test]
    fn history_restores_cursor_with_draft() {
        let mut session = Session::new(false);
        session.record(b"one\ntwo").unwrap();
        let mut state = State {
            input: "draft".into(),
            cursor: 2,
            ..State::default()
        };
        assert!(state.history_move(Some(session.history()), -1));
        assert_eq!(state.input, "one\ntwo");
        assert!(state.history_move(Some(session.history()), 1));
        assert_eq!(state.input, "draft");
        assert_eq!(state.cursor, 2);
    }
}
