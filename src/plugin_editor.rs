//! Editor-facing plugin API. Offsets are zero-based UTF-8 byte offsets.

pub struct EditorFrame<'a> {
    pub line: &'a str,
    pub cursor: usize,
    pub history: &'a [Vec<u8>],
}

pub enum EditorEvent<'a> {
    Change,
    Key { key: &'a str, action: &'a str },
    Tick,
}

pub trait EditorPlugin {
    fn on_editor(&mut self, event: EditorEvent<'_>, frame: EditorFrame<'_>) -> EditorEffects;

    fn history(&self) -> Option<Vec<Vec<u8>>> {
        None
    }

    fn has_pending_editor_work(&self) -> bool {
        false
    }
}

#[derive(Default)]
pub struct EditorEffects {
    pub consume: bool,
    pub replacement: Option<EditorReplacement>,
    pub cursor: Option<usize>,
    pub render: Option<EditorRender>,
}

pub struct EditorReplacement {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct EditorRender {
    pub ghost: Option<String>,
    pub ghost_style: Option<EditorStyle>,
    pub virtual_text: Vec<EditorVirtualText>,
    pub highlights: Option<Vec<EditorHighlight>>,
    pub menu: Option<EditorMenu>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct EditorVirtualText {
    pub offset: usize,
    pub text: String,
    pub color: Option<String>,
    pub style: Option<EditorStyle>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct EditorHighlight {
    pub start: usize,
    pub end: usize,
    pub color: String,
    pub style: Option<EditorStyle>,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct EditorMenu {
    pub items: Vec<EditorMenuItem>,
    pub selected: Option<usize>,
    pub style: Option<EditorStyle>,
    pub selected_style: Option<EditorStyle>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct EditorMenuItem {
    pub text: String,
    pub style: Option<EditorStyle>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorColor {
    Rgb([u8; 3]),
    BrightBlack,
}

impl EditorColor {
    pub fn hex(value: &str) -> Option<Self> {
        let digits = value.strip_prefix('#')?;
        if digits.len() != 6 || !digits.bytes().all(|ch| ch.is_ascii_hexdigit()) {
            return None;
        }
        Some(Self::Rgb([
            u8::from_str_radix(&digits[..2], 16).ok()?,
            u8::from_str_radix(&digits[2..4], 16).ok()?,
            u8::from_str_radix(&digits[4..], 16).ok()?,
        ]))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditorStyle {
    pub fg: Option<EditorColor>,
    pub bg: Option<EditorColor>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl EditorStyle {
    pub fn ghost() -> Self {
        Self {
            fg: Some(EditorColor::BrightBlack),
            dim: true,
            ..Self::default()
        }
    }
}
