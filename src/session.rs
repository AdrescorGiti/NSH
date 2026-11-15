//! In-memory configuration, trusted startup discovery and per-user state.
#[path = "session_files.rs"]
pub(crate) mod files;

use self::files::{Dir, base, safe_ids, uid};
use std::borrow::Cow;
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SOURCE_LIMIT: u64 = 2 * 1024 * 1024;
const HISTORY_LIMIT: u64 = 32 * 1024 * 1024;
const PROFILES: &[(&str, &[u8])] = &[
    ("default", include_bytes!("../examples/profiles/default.sh")),
    ("vanilla", include_bytes!("../examples/profiles/vanilla.sh")),
    (
        "developer",
        include_bytes!("../examples/profiles/developer.sh"),
    ),
];
const EVENTS: &[&str] = &[
    "startup",
    "pre-command",
    "post-command",
    "directory-changed",
    "before-prompt",
    "exit",
];
const ACTIONS: &[&str] = &[
    "none",
    "accept-line",
    "interrupt",
    "eof",
    "move-left",
    "move-right",
    "move-home",
    "move-end",
    "delete-left",
    "delete-right",
    "delete-word",
    "clear-line",
    "clear-screen",
    "history-previous",
    "history-next",
    "history-search",
    "move-word-left",
    "move-word-right",
    "insert-newline",
];

#[derive(Clone, Copy)]
enum Type {
    Bool,
    Range(usize, usize),
    Text,
    Template,
    Color,
    Choice(&'static [&'static str]),
}

impl Type {
    fn description(self) -> String {
        match self {
            Self::Bool => "true|false".into(),
            Self::Range(min, max) => format!("integer {min}..{max}"),
            Self::Text => "printable string".into(),
            Self::Template => "template: {cwd} {git} {symbol} {status} {uid} {user} {host}".into(),
            Self::Color => "#RRGGBB (empty disables)".into(),
            Self::Choice(options) => options.join("|"),
        }
    }

    fn validate(self, value: &str) -> bool {
        match self {
            Self::Bool => ["true", "false"].contains(&value),
            Self::Range(min, max) => value
                .parse()
                .is_ok_and(|number| (min..=max).contains(&number)),
            Self::Text => value.chars().count() <= 1024 && !value.chars().any(char::is_control),
            Self::Template => {
                value.chars().count() <= 1024
                    && !value.chars().any(|ch| ch != '\n' && ch.is_control())
                    && template(value, |_| Some(String::new())).is_some()
            }
            Self::Color => value.is_empty() || rgb(value).is_some(),
            Self::Choice(options) => options.contains(&value),
        }
    }
}

const SETTINGS: &[(&str, Type, &str)] = &[
    (
        "color.mode",
        Type::Choice(&["auto", "always", "never"]),
        "auto",
    ),
    ("color.fallback", Type::Choice(&["256", "none"]), "256"),
    (
        "prompt.style",
        Type::Choice(&["two-line", "minimal"]),
        "two-line",
    ),
    ("prompt.template", Type::Template, ""),
    ("prompt.symbol", Type::Text, ""),
    ("prompt.symbol.success", Type::Text, ""),
    ("prompt.symbol.error", Type::Text, ""),
    ("prompt.symbol_color", Type::Color, ""),
    ("prompt.symbol.success_color", Type::Color, "#79DEA4"),
    ("prompt.symbol.error_color", Type::Color, "#F87183"),
    ("prompt.user", Type::Bool, "true"),
    ("prompt.user_color", Type::Color, "#46DBEA"),
    ("prompt.host", Type::Bool, "true"),
    ("prompt.host_color", Type::Color, "#62B8F4"),
    ("prompt.path", Type::Bool, "true"),
    ("prompt.path_color", Type::Color, "#69B7FF"),
    ("prompt.path_home", Type::Bool, "true"),
    ("prompt.path_max", Type::Range(0, 4096), "0"),
    ("prompt.git", Type::Bool, "true"),
    ("prompt.git_color", Type::Color, "#89A6C2"),
    ("prompt.git_prefix", Type::Text, " ["),
    ("prompt.git_suffix", Type::Text, "]"),
    ("prompt.git_max", Type::Range(0, 4096), "0"),
    ("prompt.status_color", Type::Color, ""),
    (
        "cursor.style",
        Type::Choice(&["default", "block", "underline", "bar"]),
        "default",
    ),
    ("cursor.blink", Type::Bool, "true"),
    ("editor.bracketed_paste", Type::Bool, "true"),
    ("editor.max_bytes", Type::Range(64, 1024 * 1024), "65536"),
    ("editor.escape_timeout_ms", Type::Range(5, 500), "35"),
    ("editor.beep", Type::Bool, "false"),
    ("editor.control_echo", Type::Bool, "true"),
    ("editor.search_prompt", Type::Text, "history search: "),
    ("editor.ambiguous_width", Type::Choice(&["1", "2"]), "1"),
    ("history.enabled", Type::Bool, "true"),
    ("history.persistent", Type::Bool, "true"),
    ("history.max_size", Type::Range(0, 50000), "2000"),
    ("history.ignore_duplicates", Type::Bool, "false"),
    ("history.ignore_space", Type::Bool, "false"),
    ("history.search_ignore_case", Type::Bool, "false"),
    ("highlighting.command", Type::Color, "#8DBEFF"),
    ("highlighting.argument", Type::Color, ""),
    ("highlighting.single_quote", Type::Color, "#98D293"),
    ("highlighting.double_quote", Type::Color, "#98D293"),
    ("highlighting.variable", Type::Color, "#E9B980"),
    ("highlighting.operator", Type::Color, "#BBA2EE"),
    ("highlighting.redirect", Type::Color, "#FFAB8C"),
    ("highlighting.number", Type::Color, "#DFCFA4"),
    ("highlighting.assignment", Type::Color, "#ADC7CF"),
    ("highlighting.comment", Type::Color, "#8A96A0"),
    ("highlighting.escape", Type::Color, "#BBA2EE"),
    ("highlighting.glob", Type::Color, "#E9B980"),
];

const KEYS: &[(&str, &str)] = &[
    ("Up", "history-previous"),
    ("Down", "history-next"),
    ("Left", "move-left"),
    ("Right", "move-right"),
    ("Home", "move-home"),
    ("End", "move-end"),
    ("Backspace", "delete-left"),
    ("Delete", "delete-right"),
    ("Enter", "accept-line"),
    ("Tab", "none"),
    ("Escape", "none"),
    ("Ctrl-Left", "move-word-left"),
    ("Ctrl-Right", "move-word-right"),
    ("Ctrl-Up", "none"),
    ("Ctrl-Down", "none"),
    ("Shift-Left", "none"),
    ("Shift-Right", "none"),
    ("Shift-Up", "none"),
    ("Shift-Down", "none"),
    ("Alt-Left", "move-word-left"),
    ("Alt-Right", "move-word-right"),
    ("Alt-Up", "none"),
    ("Alt-Down", "none"),
    ("PageUp", "none"),
    ("PageDown", "none"),
    ("Insert", "none"),
    ("F1", "none"),
    ("F2", "none"),
    ("F3", "none"),
    ("F4", "none"),
    ("F5", "none"),
    ("F6", "none"),
    ("F7", "none"),
    ("F8", "none"),
    ("F9", "none"),
    ("F10", "none"),
    ("F11", "none"),
    ("F12", "none"),
];
const CTRL: &[(&str, &str)] = &[
    ("A", "move-home"),
    ("B", "move-left"),
    ("C", "interrupt"),
    ("D", "eof"),
    ("E", "move-end"),
    ("F", "move-right"),
    ("G", "none"),
    ("J", "accept-line"),
    ("K", "none"),
    ("L", "clear-screen"),
    ("N", "history-next"),
    ("O", "none"),
    ("P", "history-previous"),
    ("Q", "none"),
    ("R", "history-search"),
    ("S", "none"),
    ("T", "none"),
    ("U", "clear-line"),
    ("V", "none"),
    ("W", "delete-word"),
    ("X", "none"),
    ("Y", "none"),
    ("Z", "none"),
];

fn default_binding(name: &str) -> Option<&'static str> {
    if let Some(letter) = name.strip_prefix("Ctrl-") {
        if let Some((_, action)) = CTRL.iter().find(|(key, _)| *key == letter) {
            return Some(action);
        }
    }
    if let Some([letter]) = name.strip_prefix("Alt-").map(str::as_bytes) {
        if letter.is_ascii_lowercase() {
            return Some(match letter {
                b'b' => "move-word-left",
                b'f' => "move-word-right",
                _ => "none",
            });
        }
    }
    KEYS.iter()
        .find(|(key, _)| *key == name)
        .map(|(_, action)| *action)
}

fn bindings() -> impl Iterator<Item = String> {
    KEYS.iter()
        .map(|(key, _)| (*key).into())
        .chain(CTRL.iter().map(|(letter, _)| format!("Ctrl-{letter}")))
        .chain((b'a'..=b'z').map(|letter| format!("Alt-{}", char::from(letter))))
}

fn spec(key: &str) -> Option<(Type, &'static str)> {
    if let Some(key) = key.strip_prefix("key.") {
        return default_binding(key).map(|default| (Type::Choice(ACTIONS), default));
    }
    SETTINGS
        .iter()
        .find(|(name, _, _)| *name == key)
        .map(|(_, ty, default)| (*ty, *default))
}

#[derive(Debug)]
pub enum Action {
    Done,
    Print(String),
    Source { path: PathBuf, source: Vec<u8> },
    SyntaxCheck(PathBuf),
}

pub type StartupScript = (PathBuf, Vec<u8>);

pub struct StartupScripts {
    pub profiles: Result<Vec<StartupScript>, String>,
    pub config: Result<Option<StartupScript>, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Hook {
    function: String,
    module: Option<String>,
}

#[derive(Clone)]
pub struct Session {
    persistent: bool,
    values: BTreeMap<String, String>,
    enabled: BTreeSet<String>,
    loaded: BTreeSet<String>,
    failed: BTreeSet<String>,
    loading: Vec<String>,
    hooks: BTreeMap<String, Vec<Hook>>,
    history: Vec<Vec<u8>>,
    prompt_identity: OnceCell<PromptIdentity>,
}

impl Session {
    pub fn new(persist: bool) -> Self {
        Self {
            persistent: persist && safe_ids(),
            values: BTreeMap::new(),
            enabled: BTreeSet::from(["history".into()]),
            loaded: BTreeSet::new(),
            failed: BTreeSet::new(),
            loading: Vec::new(),
            hooks: BTreeMap::new(),
            history: Vec::new(),
            prompt_identity: OnceCell::new(),
        }
    }

    pub fn load_scripts(profile: Option<&str>) -> Result<StartupScripts, String> {
        if !safe_ids() {
            return Ok(StartupScripts {
                profiles: Ok(Vec::new()),
                config: Ok(None),
            });
        }
        let root = base("XDG_CONFIG_HOME", ".config");
        let config = root
            .as_deref()
            .map(|path| absent(Dir::open(path, false, false)))
            .transpose()
            .map_err(display_error)?
            .flatten();
        let requested = match profile {
            Some(name) => Ok(name.to_owned()),
            None => selection(config.as_ref())
                .map(|selection| selection.unwrap_or_else(|| "default".into())),
        };
        let profiles =
            requested.and_then(|name| resolve_profiles(root.as_deref(), config.as_ref(), name));
        let config = if let (Some(root), Some(dir)) = (root, config) {
            let file = root.join("conf.sh");
            absent(dir.read(OsStr::new("conf.sh"), SOURCE_LIMIT, false))
                .map_err(|e| format!("{}: {e}", file.display()))
                .map(|source| source.map(|source| (file, source)))
        } else {
            Ok(None)
        };
        Ok(StartupScripts { profiles, config })
    }

    pub fn command(&mut self, args: &[Vec<u8>]) -> Result<Action, String> {
        let usage = "usage: nsh set|get|unset|config|profile|module|hook|bind|unbind|bindings ...";
        if args.len() == 3 && args[0] == b"config" && args[1] == b"check" {
            return Ok(Action::SyntaxCheck(
                OsString::from_vec(args[2].clone()).into(),
            ));
        }
        let words = args
            .iter()
            .map(|arg| std::str::from_utf8(arg))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "NSH settings require UTF-8".to_owned())?;
        match words.as_slice() {
            ["set", key, value] => {
                self.set(key, value)?;
                Ok(Action::Done)
            }
            ["get", key] => {
                require_key(key)?;
                Ok(Action::Print(format!("{}\n", self.get(key))))
            }
            ["unset", key] => {
                require_key(key)?;
                self.values.remove(*key);
                Ok(Action::Done)
            }
            ["config", "list"] => {
                let mut keys = SETTINGS
                    .iter()
                    .map(|(key, _, _)| (*key).to_owned())
                    .collect::<Vec<_>>();
                keys.extend(bindings().map(|key| format!("key.{key}")));
                keys.sort();
                Ok(Action::Print(keys.join("\n") + "\n"))
            }
            ["config", "describe", key] => {
                let (ty, default) = require_key(key)?;
                Ok(Action::Print(format!(
                    "{key}\t{}\tdefault {default:?}\n",
                    ty.description()
                )))
            }
            ["bind", key, action] => {
                self.set(&format!("key.{key}"), action)?;
                Ok(Action::Done)
            }
            ["unbind", key] => {
                self.set(&format!("key.{key}"), "none")?;
                Ok(Action::Done)
            }
            ["bindings", "list"] => {
                let output = bindings()
                    .filter_map(|key| {
                        let action = self.get(&format!("key.{key}"));
                        (action != "none").then(|| format!("{key}\t{action}\n"))
                    })
                    .collect();
                Ok(Action::Print(output))
            }
            ["profile", rest @ ..] => self.profile(rest),
            ["module", rest @ ..] => self.module(rest),
            ["hook", rest @ ..] => self.hook(rest),
            _ => Err(usage.into()),
        }
    }

    pub fn get(&self, key: &str) -> &str {
        self.values.get(key).map_or_else(
            || spec(key).map_or("", |(_, default)| default),
            String::as_str,
        )
    }

    fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let (ty, _) = require_key(key)?;
        if !ty.validate(value) {
            return Err(format!("{key} expects {}", ty.description()));
        }
        self.values.insert(key.to_owned(), value.to_owned());
        if key == "history.max_size" {
            self.trim_history();
        }
        Ok(())
    }

    pub fn flag(&self, key: &str) -> bool {
        self.get(key) == "true"
    }

    pub fn number(&self, key: &str) -> usize {
        self.get(key).parse().unwrap_or(0)
    }

    pub fn enabled(&self, module: &str) -> bool {
        self.enabled.contains(module)
    }

    fn profile(&self, args: &[&str]) -> Result<Action, String> {
        let root = base("XDG_CONFIG_HOME", ".config");
        match args {
            ["list"] => {
                let mut names = PROFILES
                    .iter()
                    .map(|(name, _)| (*name).to_owned())
                    .collect::<BTreeSet<_>>();
                if let Some(root) = root {
                    names.extend(user_names(&root, "profiles")?);
                }
                Ok(Action::Print(
                    names.into_iter().map(|name| name + "\n").collect(),
                ))
            }
            [verb @ ("create" | "use" | "delete"), name] => {
                if !self.persistent {
                    return Err("profile changes are unavailable in this mode".into());
                }
                name_ok(name)?;
                let root = root.ok_or_else(|| "no absolute XDG_CONFIG_HOME or HOME".to_owned())?;
                let config = Dir::open(&root, *verb != "delete", false).map_err(display_error)?;
                let _guard = config.lock().map_err(display_error)?;
                let filename = format!("{name}.sh");
                match *verb {
                    "use" => {
                        resolve_profiles(Some(&root), Some(&config), (*name).to_owned())?;
                        selection(Some(&config))?;
                        config
                            .replace(OsStr::new("profile"), format!("{name}\n").as_bytes())
                            .map_err(display_error)?;
                    }
                    "create" => {
                        if builtin_profile(name).is_some() {
                            return Err(format!("profile {name} is built in"));
                        }
                        config
                            .child(OsStr::new("profiles"), true, false)
                            .and_then(|dir| dir.create(OsStr::new(&filename), b"# NSH profile\n"))
                            .map_err(display_error)?;
                    }
                    "delete" => {
                        if builtin_profile(name).is_some() {
                            return Err(format!("profile {name} is built in"));
                        }
                        if let Some(selected) = selection(Some(&config))? {
                            let active = resolve_profiles(Some(&root), Some(&config), selected)?;
                            if active
                                .iter()
                                .any(|(path, _)| path == &root.join("profiles").join(&filename))
                            {
                                return Err(format!(
                                    "profile {name} belongs to the saved profile chain"
                                ));
                            }
                        }
                        let dir = config
                            .child(OsStr::new("profiles"), false, false)
                            .map_err(display_error)?;
                        dir.read(OsStr::new(&filename), SOURCE_LIMIT, false)
                            .map_err(display_error)?;
                        dir.remove(OsStr::new(&filename)).map_err(display_error)?;
                    }
                    _ => unreachable!(),
                }
                Ok(Action::Done)
            }
            _ => Err("usage: nsh profile list|create <name>|use <name>|delete <name>".into()),
        }
    }

    fn module(&mut self, args: &[&str]) -> Result<Action, String> {
        const BUILTINS: &[&str] = &["history", "highlighting", "git-prompt"];
        match args {
            ["list"] => {
                let mut names = BUILTINS
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<BTreeSet<_>>();
                if self.persistent {
                    if let Some(root) = base("XDG_CONFIG_HOME", ".config") {
                        names.extend(user_names(&root, "modules")?);
                    }
                }
                Ok(Action::Print(
                    names
                        .into_iter()
                        .map(|name| {
                            format!(
                                "{name}\t{}\n",
                                if self.enabled(&name) {
                                    "enabled"
                                } else {
                                    "disabled"
                                }
                            )
                        })
                        .collect(),
                ))
            }
            ["enable", name] => {
                name_ok(name)?;
                if self.enabled(name) || BUILTINS.contains(name) {
                    self.enabled.insert((*name).into());
                    return Ok(Action::Done);
                }
                if self.loaded.contains(*name) {
                    if self.failed.contains(*name) {
                        return Err(format!(
                            "module {name} failed while loading in this session"
                        ));
                    }
                    self.enabled.insert((*name).into());
                    return Ok(Action::Done);
                }
                if !self.persistent {
                    return Err("user modules are unavailable in this mode".into());
                }
                let root = base("XDG_CONFIG_HOME", ".config")
                    .ok_or_else(|| "no absolute XDG_CONFIG_HOME or HOME".to_owned())?;
                let filename = format!("{name}.sh");
                let directory = root.join("modules");
                let path = directory.join(&filename);
                let source = Dir::open(&directory, false, false)
                    .and_then(|dir| dir.read(OsStr::new(&filename), SOURCE_LIMIT, false))
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                self.loaded.insert((*name).into());
                self.enabled.insert((*name).into());
                self.loading.push((*name).into());
                Ok(Action::Source { path, source })
            }
            ["disable", name] => {
                name_ok(name)?;
                if !BUILTINS.contains(name) && !self.loaded.contains(*name) {
                    return Err(format!("unknown module {name}"));
                }
                self.enabled.remove(*name);
                Ok(Action::Done)
            }
            _ => Err("usage: nsh module list|enable <name>|disable <name>".into()),
        }
    }

    pub fn finish_module(&mut self, succeeded: bool) {
        if let Some(name) = self.loading.pop() {
            if !succeeded {
                self.enabled.remove(&name);
                self.failed.insert(name);
            }
        }
    }

    fn hook(&mut self, args: &[&str]) -> Result<Action, String> {
        match args {
            ["list"] => Ok(Action::Print(
                self.hooks
                    .iter()
                    .flat_map(|(event, hooks)| {
                        hooks
                            .iter()
                            .filter(|hook| self.hook_enabled(hook))
                            .map(move |hook| format!("{event}\t{}\n", hook.function))
                    })
                    .collect(),
            )),
            [verb @ ("add" | "remove"), event, function] => {
                if !EVENTS.contains(event) {
                    return Err(format!("unknown hook event {event}"));
                }
                let mut chars = function.bytes();
                if !chars
                    .next()
                    .is_some_and(|ch| ch == b'_' || ch.is_ascii_alphabetic())
                    || !chars.all(|ch| ch == b'_' || ch.is_ascii_alphanumeric())
                {
                    return Err("hook function must be a shell identifier".into());
                }
                let handlers = self.hooks.entry((*event).into()).or_default();
                if *verb == "add" {
                    let hook = Hook {
                        function: (*function).into(),
                        module: self.loading.last().cloned(),
                    };
                    if !handlers.contains(&hook) {
                        handlers.push(hook);
                    }
                } else {
                    handlers.retain(|hook| hook.function != *function);
                }
                Ok(Action::Done)
            }
            _ => {
                Err("usage: nsh hook list|add <event> <function>|remove <event> <function>".into())
            }
        }
    }

    fn hook_enabled(&self, hook: &Hook) -> bool {
        hook.module
            .as_deref()
            .is_none_or(|module| self.enabled(module))
    }

    pub fn hooks(&self, event: &str) -> Vec<Vec<u8>> {
        self.hooks
            .get(event)
            .into_iter()
            .flatten()
            .filter(|hook| self.hook_enabled(hook))
            .map(|hook| hook.function.as_bytes().to_vec())
            .collect()
    }

    pub fn history(&self) -> &[Vec<u8>] {
        &self.history
    }

    fn trim_history(&mut self) {
        let extra = self
            .history
            .len()
            .saturating_sub(self.number("history.max_size"));
        self.history.drain(..extra);
    }

    fn disk_history(&self) -> bool {
        self.persistent
            && self.enabled("history")
            && self.flag("history.enabled")
            && self.flag("history.persistent")
    }

    pub fn load_history(&mut self) -> Result<(), String> {
        if !self.disk_history() {
            return Ok(());
        }
        if let Some(root) = base("XDG_STATE_HOME", ".local/state") {
            if let Some(dir) = absent(Dir::open(&root, false, true)).map_err(display_error)? {
                let _guard = dir.lock().map_err(display_error)?;
                self.history = read_history(&dir)?;
                self.trim_history();
            }
        }
        Ok(())
    }

    pub fn record(&mut self, line: &[u8]) -> Result<(), String> {
        if line.iter().all(u8::is_ascii_whitespace)
            || line.len() > HISTORY_LIMIT as usize / 2 - 1
            || self.number("history.max_size") == 0
            || !self.enabled("history")
            || !self.flag("history.enabled")
            || (self.flag("history.ignore_space") && line.starts_with(b" "))
        {
            return Ok(());
        }
        let no_duplicates = self.flag("history.ignore_duplicates");
        if !no_duplicates || self.history.last().is_none_or(|previous| previous != line) {
            self.history.push(line.to_vec());
            self.trim_history();
        }
        if !self.disk_history() {
            return Ok(());
        }
        if let Some(root) = base("XDG_STATE_HOME", ".local/state") {
            let dir = Dir::open(&root, true, true).map_err(display_error)?;
            let _guard = dir.lock().map_err(display_error)?;
            let mut stored = read_history(&dir)?;
            if !no_duplicates || stored.last().is_none_or(|previous| previous != line) {
                stored.push(line.to_vec());
            }
            let mut size = 0;
            let keep = stored
                .iter()
                .rev()
                .take(self.number("history.max_size"))
                .take_while(|entry| {
                    size += entry.len() * 2 + 1;
                    size <= HISTORY_LIMIT as usize
                })
                .count();
            let mut data = Vec::with_capacity(size.min(HISTORY_LIMIT as usize));
            for entry in &stored[stored.len() - keep..] {
                for byte in entry {
                    const DIGITS: &[u8] = b"0123456789abcdef";
                    data.extend([DIGITS[(byte >> 4) as usize], DIGITS[(byte & 15) as usize]]);
                }
                data.push(b'\n');
            }
            dir.replace(OsStr::new("history"), &data)
                .map_err(display_error)?;
        }
        Ok(())
    }

    pub fn prompt(&self, ps1: &str, status: i32) -> String {
        let (symbol_key, status_color) = if status == 0 {
            ("prompt.symbol.success", "prompt.symbol.success_color")
        } else {
            ("prompt.symbol.error", "prompt.symbol.error_color")
        };
        let pattern = self.get("prompt.template");
        let two_lines = self.get("prompt.style") == "two-line";
        let configured_symbol = [self.get(symbol_key), self.get("prompt.symbol")]
            .into_iter()
            .find(|symbol| !symbol.is_empty());
        if pattern.is_empty() && !two_lines && configured_symbol.is_none() {
            return ps1.to_owned();
        }
        let symbol = configured_symbol.unwrap_or(if two_lines { "❯" } else { ps1.trim_end() });
        let symbol_color = if self.values.contains_key(status_color)
            || pattern.is_empty() && two_lines && !self.values.contains_key("prompt.symbol_color")
        {
            status_color
        } else {
            "prompt.symbol_color"
        };
        let colors = self.terminal_colors();
        let identity = || self.prompt_identity.get_or_init(PromptIdentity::read);
        let user = || self.paint(colors, "prompt.user_color", &identity().user);
        let host = || self.paint(colors, "prompt.host_color", &identity().host);
        let git_enabled = self.flag("prompt.git") && self.enabled("git-prompt");
        if !pattern.is_empty() {
            return template(pattern, |field| {
                Some(match field {
                    "symbol" => self.paint(colors, symbol_color, symbol),
                    "status" => self.paint(colors, "prompt.status_color", &status.to_string()),
                    "uid" => uid().to_string(),
                    "user" if self.flag("prompt.user") => user(),
                    "host" if self.flag("prompt.host") => host(),
                    "git" if git_enabled => self.git_prompt(colors, true),
                    "cwd" if self.flag("prompt.path") => self.cwd_prompt(colors),
                    "cwd" | "git" | "user" | "host" => String::new(),
                    _ => return None,
                })
            })
            .unwrap_or_else(|| ps1.to_owned());
        }

        let mut output = String::new();
        if two_lines {
            if self.flag("prompt.user") {
                output.push_str(&user());
            }
            if self.flag("prompt.host") {
                let host = host();
                if !host.is_empty() {
                    if !output.is_empty() {
                        output.push('@');
                    }
                    output.push_str(&host);
                }
            }
            if self.flag("prompt.path") {
                push_prompt_segment(&mut output, &self.cwd_prompt(colors));
            }
            if git_enabled {
                push_prompt_segment(&mut output, &self.git_prompt(colors, false));
            }
            if !output.is_empty() {
                output.push('\n');
            }
        }
        output.push_str(&self.paint(colors, symbol_color, symbol));
        output.push(' ');
        output
    }

    fn cwd_prompt(&self, colors: TerminalColors) -> String {
        let Ok(cwd) = env::current_dir() else {
            return String::new();
        };
        let home = env::var_os("HOME").map(PathBuf::from);
        let relative = home
            .as_deref()
            .filter(|_| self.flag("prompt.path_home"))
            .and_then(|home| cwd.strip_prefix(home).ok());
        let path = match relative {
            Some(path) if path.as_os_str().is_empty() => Cow::Borrowed("~"),
            Some(path) => Cow::Owned(format!("~/{}", path.display())),
            None => cwd.to_string_lossy(),
        };
        self.paint(
            colors,
            "prompt.path_color",
            &clean(&path, self.number("prompt.path_max")),
        )
    }

    fn git_prompt(&self, colors: TerminalColors, inline: bool) -> String {
        let Ok(output) = Command::new("git")
            .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            return String::new();
        };
        if !output.status.success() {
            return String::new();
        }
        let name = String::from_utf8_lossy(&output.stdout);
        let name = clean(name.trim_end_matches('\n'), self.number("prompt.git_max"));
        if name.is_empty() {
            return String::new();
        }
        let decoration = |key| {
            if inline {
                self.get(key)
            } else {
                self.values.get(key).map_or("", String::as_str)
            }
        };
        format!(
            "{}{}{}",
            decoration("prompt.git_prefix"),
            self.paint(colors, "prompt.git_color", &name),
            decoration("prompt.git_suffix")
        )
    }

    fn paint(&self, colors: TerminalColors, key: &str, value: &str) -> String {
        if !value.is_empty() {
            if let Some(color) = self.color(colors, key) {
                return format!("{color}{value}\x1b[0m");
            }
        }
        value.to_owned()
    }

    pub fn colors(&self, keys: &[&str]) -> Vec<Option<String>> {
        let colors = self.terminal_colors();
        keys.iter().map(|key| self.color(colors, key)).collect()
    }

    fn color(&self, colors: TerminalColors, key: &str) -> Option<String> {
        colors.style(self.get(key))
    }

    pub(crate) fn terminal_colors(&self) -> TerminalColors {
        let mode = self.get("color.mode");
        let term = env::var("TERM").unwrap_or_default();
        if mode == "never"
            || term.is_empty()
            || term == "dumb"
            || env::var_os("NO_COLOR").is_some()
            || !crate::sys::tty(libc::STDIN_FILENO)
            || !crate::sys::tty(libc::STDERR_FILENO)
        {
            return TerminalColors::Plain;
        }
        let truecolor = || {
            term.contains("direct")
                || env::var("COLORTERM")
                    .is_ok_and(|value| matches!(value.as_str(), "truecolor" | "24bit"))
        };
        if mode == "always" || truecolor() {
            TerminalColors::Rgb
        } else if term.contains("256color") && self.get("color.fallback") == "256" {
            TerminalColors::Indexed
        } else {
            TerminalColors::Plain
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum TerminalColors {
    Plain,
    Rgb,
    Indexed,
}

impl TerminalColors {
    pub(crate) fn structured(self, spec: &crate::plugin_editor::EditorStyle) -> Option<String> {
        use crate::plugin_editor::EditorColor;

        if matches!(self, Self::Plain) {
            return None;
        }
        let mut values = Vec::with_capacity(7);
        for (active, ansi) in [
            (spec.bold, "1"),
            (spec.dim, "2"),
            (spec.italic, "3"),
            (spec.underline, "4"),
            (spec.inverse, "7"),
        ] {
            if active {
                values.push(std::borrow::Cow::Borrowed(ansi));
            }
        }
        for (color, background) in [(spec.fg, false), (spec.bg, true)] {
            let Some(color) = color else {
                continue;
            };
            values.push(match color {
                EditorColor::BrightBlack => {
                    std::borrow::Cow::Borrowed(if background { "100" } else { "90" })
                }
                EditorColor::Rgb([r, g, b]) => {
                    let plane = if background { 48 } else { 38 };
                    let parameter = match self {
                        Self::Rgb => format!("{plane};2;{r};{g};{b}"),
                        Self::Indexed => {
                            let cube = |v: u8| (u16::from(v) * 5 + 127) / 255;
                            format!("{plane};5;{}", 16 + 36 * cube(r) + 6 * cube(g) + cube(b))
                        }
                        Self::Plain => unreachable!("plain terminal is handled above"),
                    };
                    std::borrow::Cow::Owned(parameter)
                }
            });
        }
        (!values.is_empty()).then(|| format!("\x1b[{}m", values.join(";")))
    }

    pub(crate) fn style(self, spec: &str) -> Option<String> {
        if matches!(self, Self::Plain) {
            return None;
        }
        if spec == "dim" {
            return Some("\x1b[2;90m".to_owned());
        }
        let [r, g, b] = rgb(spec)?;
        match self {
            Self::Plain => None,
            Self::Rgb => Some(format!("\x1b[38;2;{r};{g};{b}m")),
            Self::Indexed => {
                let cube = |v: u8| (u16::from(v) * 5 + 127) / 255;
                Some(format!(
                    "\x1b[38;5;{}m",
                    16 + 36 * cube(r) + 6 * cube(g) + cube(b)
                ))
            }
        }
    }
}

#[derive(Clone)]
struct PromptIdentity {
    user: String,
    host: String,
}

impl PromptIdentity {
    fn read() -> Self {
        let user = env::var("USER")
            .or_else(|_| env::var("LOGNAME"))
            .ok()
            .filter(|name| !name.is_empty())
            .or_else(local_username)
            .unwrap_or_else(|| uid().to_string());
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .ok()
            .or_else(|| env::var("HOSTNAME").ok())
            .unwrap_or_default();
        Self {
            user: clean(&user, 128).into_owned(),
            host: clean(host.trim_end_matches('\n'), 128).into_owned(),
        }
    }
}

fn local_username() -> Option<String> {
    let id = uid().to_string();
    let data = std::fs::read("/etc/passwd").ok()?;
    let name = data.split(|&byte| byte == b'\n').find_map(|line| {
        let mut fields = line.split(|&byte| byte == b':');
        let name = fields.next()?;
        fields.next()?;
        (fields.next()? == id.as_bytes() && !name.is_empty()).then_some(name)
    })?;
    Some(String::from_utf8_lossy(name).into_owned())
}

fn push_prompt_segment(output: &mut String, segment: &str) {
    if !segment.is_empty() {
        if !output.is_empty() {
            output.push_str("  ");
        }
        output.push_str(segment);
    }
}

fn display_error(error: io::Error) -> String {
    error.to_string()
}

fn absent<T>(value: io::Result<T>) -> io::Result<Option<T>> {
    match value {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        result => result.map(Some),
    }
}

fn name_ok(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("name must be 1–64 ASCII letters, digits, underscores or hyphens".into());
    }
    Ok(())
}

fn user_names(root: &Path, sub: &str) -> Result<Vec<String>, String> {
    let Some(dir) = absent(Dir::open(&root.join(sub), false, false)).map_err(display_error)? else {
        return Ok(Vec::new());
    };
    Ok(dir
        .list()
        .map_err(display_error)?
        .into_iter()
        .filter_map(|file| {
            let name = file.strip_suffix(".sh")?;
            name_ok(name).ok()?;
            dir.read(OsStr::new(&file), SOURCE_LIMIT, false).ok()?;
            Some(name.to_owned())
        })
        .collect())
}

fn selection(dir: Option<&Dir>) -> Result<Option<String>, String> {
    let Some(dir) = dir else { return Ok(None) };
    let Some(value) = absent(dir.read(OsStr::new("profile"), 128, false)).map_err(display_error)?
    else {
        return Ok(None);
    };
    let value = std::str::from_utf8(&value)
        .map_err(|_| "profile selection is not UTF-8".to_owned())?
        .trim_end_matches('\n');
    name_ok(value)?;
    Ok(Some(value.to_owned()))
}

fn builtin_profile(name: &str) -> Option<&'static [u8]> {
    PROFILES
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, source)| *source)
}

fn resolve_profiles(
    root: Option<&Path>,
    dir: Option<&Dir>,
    mut name: String,
) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    let mut seen = BTreeSet::new();
    let mut sources = Vec::new();
    loop {
        name_ok(&name)?;
        if !seen.insert(name.clone()) {
            return Err(format!("profile inheritance cycle at {name}"));
        }
        if seen.len() > 64 {
            return Err("profile inheritance exceeds 64 levels".into());
        }
        let (path, mut source) = if let Some(source) = builtin_profile(&name) {
            (PathBuf::from(format!("<profile:{name}>")), source.to_vec())
        } else {
            let Some(root) = root else {
                return Err(format!("profile {name} does not exist"));
            };
            let Some(dir) = dir else {
                return Err(format!("profile {name} does not exist"));
            };
            let file = format!("{name}.sh");
            let path = root.join("profiles").join(&file);
            let source = dir
                .child(OsStr::new("profiles"), false, false)
                .and_then(|profiles| profiles.read(OsStr::new(&file), SOURCE_LIMIT, false))
                .map_err(|e| format!("{}: {e}", path.display()))?;
            (path, source)
        };
        let parent = take_parent(&mut source).map_err(|e| format!("{}: {e}", path.display()))?;
        sources.push((path, source));
        let Some(parent) = parent else { break };
        name = parent;
    }
    sources.reverse();
    Ok(sources)
}

fn take_parent(source: &mut [u8]) -> Result<Option<String>, String> {
    for (index, line) in source.split_mut(|byte| *byte == b'\n').enumerate() {
        let first = line
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
            .unwrap_or(line.len());
        let trimmed = &line[first..];
        if trimmed.is_empty() || trimmed.starts_with(b"#") {
            continue;
        }
        let parts = trimmed
            .split(u8::is_ascii_whitespace)
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>();
        if parts.starts_with(&[b"nsh", b"profile", b"inherit"]) {
            if parts.len() != 4 {
                return Err(format!(
                    "line {}: expected nsh profile inherit <name>",
                    index + 1
                ));
            }
            let name = std::str::from_utf8(parts[3])
                .map_err(|_| "profile parent is not UTF-8".to_owned())?
                .to_owned();
            name_ok(&name)?;
            line.fill(b' ');
            return Ok(Some(name));
        }
        break;
    }
    Ok(None)
}

fn require_key(key: &str) -> Result<(Type, &'static str), String> {
    spec(key).ok_or_else(|| format!("unknown NSH configuration key {key:?}"))
}

fn read_history(dir: &Dir) -> Result<Vec<Vec<u8>>, String> {
    let Some(bytes) =
        absent(dir.read(OsStr::new("history"), HISTORY_LIMIT, true)).map_err(display_error)?
    else {
        return Ok(Vec::new());
    };
    let complete = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(&[][..], |end| &bytes[..end]);
    Ok(complete
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            if line.is_empty() || line.len() % 2 != 0 {
                return None;
            }
            line.chunks_exact(2)
                .map(|pair| Some(hex(pair[0])? * 16 + hex(pair[1])?))
                .collect::<Option<Vec<_>>>()
        })
        .collect())
}

fn hex(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|digit| digit as u8)
}

fn rgb(value: &str) -> Option<[u8; 3]> {
    let [b'#', r1, r2, g1, g2, b1, b2] = value.as_bytes() else {
        return None;
    };
    Some([
        hex(*r1)? * 16 + hex(*r2)?,
        hex(*g1)? * 16 + hex(*g2)?,
        hex(*b1)? * 16 + hex(*b2)?,
    ])
}

fn template(mut input: &str, mut field: impl FnMut(&str) -> Option<String>) -> Option<String> {
    let mut output = String::new();
    while let Some(index) = input.find(['{', '}']) {
        output.push_str(&input[..index]);
        input = &input[index..];
        if input.starts_with("{{") || input.starts_with("}}") {
            output.push(input.as_bytes()[0] as char);
            input = &input[2..];
        } else if let Some((name, rest)) = input.strip_prefix('{')?.split_once('}') {
            if !["cwd", "git", "symbol", "status", "uid", "user", "host"].contains(&name) {
                return None;
            }
            output.push_str(&field(name)?);
            input = rest;
        } else {
            return None;
        }
    }
    output.push_str(input);
    Some(output)
}

fn clean(value: &str, max: usize) -> Cow<'_, str> {
    let unsafe_character = |ch: char| {
        ch.is_control()
            || matches!(
                ch,
                '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            )
    };
    let mut printable = if !value.chars().any(unsafe_character) {
        Cow::Borrowed(value)
    } else {
        let mut sanitized = String::with_capacity(value.len());
        for ch in value.chars() {
            if unsafe_character(ch) {
                sanitized.extend(ch.escape_default());
            } else {
                sanitized.push(ch);
            }
        }
        Cow::Owned(sanitized)
    };
    if max > 0 && printable.chars().count() > max {
        let boundary = printable.char_indices().nth(max - 1).unwrap().0;
        let value = printable.to_mut();
        value.truncate(boundary);
        value.push('…');
    }
    printable
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(session: &mut Session, command: &[&str]) -> Result<Action, String> {
        session.command(
            &command
                .iter()
                .map(|word| word.as_bytes().to_vec())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn settings_validate_and_are_independent() {
        let mut first = Session::new(false);
        let mut second = first.clone();
        assert!(run(&mut first, &["set", "prompt.path_color", "#D0ffaa"]).is_ok());
        assert!(run(&mut first, &["set", "prompt.path_color", "#DXffff"]).is_err());
        assert!(run(&mut first, &["set", "history.max_size", "50001"]).is_err());
        assert!(run(&mut first, &["set", "prompt.template", "{unknown}"]).is_err());
        assert_eq!(first.get("prompt.path_color"), "#D0ffaa");
        assert_eq!(second.get("prompt.path_color"), "#69B7FF");
        assert!(run(&mut second, &["bind", "F8", "move-home"]).is_ok());
        assert_eq!(second.get("key.F8"), "move-home");
        assert_eq!(first.get("key.F8"), "none");
        let Action::Print(keys) = run(&mut first, &["config", "list"]).unwrap() else {
            panic!("expected keys")
        };
        let keys = keys.lines().collect::<BTreeSet<_>>();
        assert_eq!(keys.len(), 138);
        assert!(keys.into_iter().all(|key| spec(key).is_some()));
    }

    #[test]
    fn prompt_escapes_untrusted_data_and_uses_literal_fields() {
        assert_eq!(
            clean("evil\n\x1b[1m\u{202e}", 0),
            "evil\\n\\u{1b}[1m\\u{202e}"
        );
        assert_eq!(
            template("{{{user}}}", |_| Some("{symbol}".into())).as_deref(),
            Some("{{symbol}}")
        );
        let mut settings = Session::new(false);
        run(&mut settings, &["set", "prompt.style", "minimal"]).unwrap();
        run(&mut settings, &["set", "prompt.symbol", "⚡"]).unwrap();
        run(&mut settings, &["set", "prompt.symbol.success", "❯"]).unwrap();
        assert_eq!(settings.prompt("$ ", 0), "❯ ");
        assert_eq!(settings.prompt("$ ", 1), "⚡ ");
    }

    #[test]
    fn profiles_resolve_before_execution() {
        let fixtures = resolve_profiles(None, None, "developer".into()).unwrap();
        assert_eq!(
            fixtures
                .iter()
                .map(|(path, _)| path.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["<profile:vanilla>", "<profile:developer>"]
        );
        assert!(resolve_profiles(None, None, "../../vanilla".into()).is_err());
        assert!(resolve_profiles(None, None, "missing".into()).is_err());
    }

    #[test]
    fn hook_registration_preserves_order_and_disables_module_handlers() {
        let mut session = Session::new(false);
        run(&mut session, &["hook", "add", "startup", "first"]).unwrap();
        run(&mut session, &["hook", "add", "startup", "first"]).unwrap();
        session.enabled.insert("user-module".into());
        session.loading.push("user-module".into());
        run(&mut session, &["hook", "add", "startup", "second"]).unwrap();
        assert_eq!(
            session.hooks("startup"),
            [b"first".to_vec(), b"second".to_vec()]
        );
        session.finish_module(false);
        assert_eq!(session.hooks("startup"), [b"first".to_vec()]);
    }

    #[test]
    fn history_respects_memory_settings() {
        let mut session = Session::new(false);
        run(&mut session, &["set", "history.max_size", "2"]).unwrap();
        run(&mut session, &["set", "history.ignore_duplicates", "true"]).unwrap();
        for entry in [b"first".as_slice(), b"second\nline", b"third", b"third"] {
            session.record(entry).unwrap();
        }
        assert_eq!(
            session.history(),
            [b"second\nline".to_vec(), b"third".to_vec()]
        );
        run(&mut session, &["module", "disable", "history"]).unwrap();
        session.record(b"ignored").unwrap();
        assert_eq!(session.history().len(), 2);
    }
}
