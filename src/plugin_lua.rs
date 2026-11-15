//! Lua callback ownership and the boundary between the VM and NSH.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{
    ChunkMode, Function, HookTriggers, Lua, LuaOptions, RegistryKey, StdLib, Value, VmState,
};

use crate::plugin_editor::{
    EditorEffects, EditorEvent, EditorFrame, EditorRender, EditorReplacement,
};
use crate::plugin_fs::Fs;
use crate::session::TerminalColors;
use crate::session::files::Dir;

#[path = "plugin_lua_api.rs"]
mod api;

const MEMORY_LIMIT: usize = 8 * 1024 * 1024;
const MAX_REGISTRATIONS: usize = 128;
const MAX_EDITOR_TEXT: usize = 32 * 1024;
const MAX_ITEMS: usize = 512;
const MAX_EFFECTS: usize = 128;
const INSTRUCTION_INTERVAL: u32 = 1_000;
const INSTRUCTION_SAMPLES: u32 = 1_000;
const MAX_MODULES: usize = 64;
const MAX_PLUGIN_SOURCE: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandMode {
    Register,
    Replace,
    Wrap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Provider {
    Prompt,
    History,
    Highlight,
    Completion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    SetVar(Vec<u8>, Option<Vec<u8>>),
    SetEnv(Vec<u8>, Option<Vec<u8>>),
    Cd(PathBuf),
    Exec(Vec<Vec<u8>>),
}

#[derive(Clone, Debug, Default)]
pub struct CallContext {
    pub vars: Rc<BTreeMap<Vec<u8>, Vec<u8>>>,
    pub env: Rc<BTreeMap<Vec<u8>, Vec<u8>>>,
    pub cwd: PathBuf,
    pub effects: Vec<Effect>,
    pub history: Rc<Vec<Vec<u8>>>,
    pub status: i32,
    pub commands: Rc<Vec<Vec<u8>>>,
}

#[derive(Default)]
struct Registrations {
    commands: BTreeMap<Vec<u8>, (CommandMode, RegistryKey)>,
    providers: BTreeMap<Provider, RegistryKey>,
    events: BTreeMap<String, Vec<RegistryKey>>,
    keys: BTreeMap<String, RegistryKey>,
    change: Vec<RegistryKey>,
    tick: Vec<RegistryKey>,
}

impl Registrations {
    fn room(&self) -> mlua::Result<()> {
        let count = self.commands.len()
            + self.providers.len()
            + self.events.values().map(Vec::len).sum::<usize>()
            + self.keys.len()
            + self.change.len()
            + self.tick.len();
        if count >= MAX_REGISTRATIONS {
            Err(error("plugin registration limit exceeded"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
enum Phase {
    Register,
    Command,
    Event,
    ReadOnly,
}

struct Edit {
    line: String,
    original_len: usize,
    cursor: usize,
    changed: bool,
    cursor_changed: bool,
    render: EditorRender,
    render_changed: bool,
}

impl Edit {
    fn new(frame: &EditorFrame<'_>) -> Self {
        Self {
            line: frame.line.to_owned(),
            original_len: frame.line.len(),
            cursor: frame.cursor,
            changed: false,
            cursor_changed: false,
            render: EditorRender::default(),
            render_changed: false,
        }
    }

    fn finish(self, consume: bool, change: bool) -> EditorEffects {
        EditorEffects {
            consume,
            replacement: self.changed.then_some(EditorReplacement {
                start: 0,
                end: self.original_len,
                text: self.line,
            }),
            cursor: (self.cursor_changed || self.changed).then_some(self.cursor),
            render: (self.render_changed || change).then_some(self.render),
        }
    }
}

struct Call {
    shell: CallContext,
    phase: Phase,
    edit: Option<Edit>,
}

impl Call {
    fn effect(&mut self, effect: Effect) -> mlua::Result<()> {
        if !matches!(self.phase, Phase::Command | Phase::Event) {
            return Err(error(
                "shell mutations require a command or lifecycle callback",
            ));
        }
        if self.shell.effects.len() >= MAX_EFFECTS {
            return Err(error("plugin shell effect limit exceeded"));
        }
        if matches!(self.shell.effects.last(), Some(Effect::Exec(_))) {
            return Err(error("exec must be the final shell effect"));
        }
        self.shell.effects.push(effect);
        Ok(())
    }

    fn io(&self) -> mlua::Result<()> {
        if matches!(self.phase, Phase::Command | Phase::Event) {
            Ok(())
        } else {
            Err(error("live I/O requires a command or lifecycle callback"))
        }
    }
}

struct State {
    capabilities: Vec<String>,
    options: toml::Table,
    files: RefCell<Option<PluginFiles>>,
    modules: RefCell<BTreeMap<String, Option<RegistryKey>>>,
    registrations: RefCell<Registrations>,
    call: RefCell<Option<Call>>,
    fs: RefCell<Fs>,
    command_cache: RefCell<api::CommandCache>,
    budget: Cell<u32>,
    deadline: Cell<Option<Instant>>,
}

impl State {
    fn allow(&self, capability: &str) -> mlua::Result<()> {
        if self.capabilities.iter().any(|cap| cap == capability) {
            Ok(())
        } else {
            Err(error(format!(
                "requires '{capability}' capability in plugin.toml"
            )))
        }
    }

    fn call<T>(&self, update: impl FnOnce(&mut Call) -> mlua::Result<T>) -> mlua::Result<T> {
        let mut call = self
            .call
            .try_borrow_mut()
            .map_err(|_| error("reentrant plugin API"))?;
        update(
            call.as_mut()
                .ok_or_else(|| error("plugin API used outside a callback"))?,
        )
    }

    fn edit<T>(&self, update: impl FnOnce(&mut Edit) -> mlua::Result<T>) -> mlua::Result<T> {
        self.call(|call| {
            update(
                call.edit
                    .as_mut()
                    .ok_or_else(|| error("editor API requires an editor callback"))?,
            )
        })
    }

    fn register(
        &self,
        update: impl FnOnce(&mut Registrations) -> mlua::Result<()>,
    ) -> mlua::Result<()> {
        self.call(|call| {
            if !matches!(call.phase, Phase::Register) {
                return Err(error("register handlers during plugin initialization"));
            }
            Ok(())
        })?;
        let mut handlers = self
            .registrations
            .try_borrow_mut()
            .map_err(|_| error("reentrant plugin registration"))?;
        handlers.room()?;
        update(&mut handlers)
    }

    fn require(&self, lua: &Lua, name: String) -> mlua::Result<Value> {
        {
            let modules = self.modules.borrow();
            if let Some(module) = modules.get(&name) {
                return match module {
                    Some(key) => lua.registry_value(key),
                    None => Err(error(format!("circular plugin module dependency: {name}"))),
                };
            }
            if modules.len() >= MAX_MODULES {
                return Err(error("plugin module limit exceeded"));
            }
        }

        let function = {
            let files = self.files.borrow();
            let files = files
                .as_ref()
                .ok_or_else(|| error("module must first be loaded during plugin initialization"))?;
            let relative = module_path(&name)?;
            let path = files.path.join(&relative);
            let source = files
                .read(&relative)
                .map_err(|cause| error(format!("{}: {cause}", path.display())))?;
            lua.load(&source)
                .set_name(format!("@{}", path.display()))
                .set_mode(ChunkMode::Text)
                .into_function()?
        };

        // Release both borrows before execution so modules can import each other.
        self.modules.borrow_mut().insert(name.clone(), None);
        let result = function.call::<Value>(()).and_then(|value| {
            let value = if value.is_nil() {
                Value::Boolean(true)
            } else {
                value
            };
            let key = lua.create_registry_value(value.clone())?;
            self.modules.borrow_mut().insert(name.clone(), Some(key));
            Ok(value)
        });
        if result.is_err() {
            self.modules.borrow_mut().remove(&name);
        }
        result
    }
}

fn module_path(name: &str) -> mlua::Result<PathBuf> {
    if name.len() > 128 {
        return Err(error("invalid plugin module name"));
    }
    let mut path = PathBuf::new();
    for part in name.split('.') {
        if part.is_empty()
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(error("invalid plugin module name"));
        }
        path.push(part);
    }
    path.set_extension("lua");
    Ok(path)
}

pub struct PluginFiles {
    path: PathBuf,
    directory: Dir,
}

impl PluginFiles {
    pub fn new(path: PathBuf, directory: Dir) -> Self {
        Self { path, directory }
    }

    pub fn read(&self, entry: &Path) -> io::Result<Vec<u8>> {
        let mut parts = entry.components().peekable();
        let mut directory = None;
        loop {
            let Some(Component::Normal(part)) = parts.next() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid relative entry",
                ));
            };
            let parent = directory.as_ref().unwrap_or(&self.directory);
            if parts.peek().is_none() {
                return parent.read(part, MAX_PLUGIN_SOURCE, false);
            }
            directory = Some(parent.child(part, false, false)?);
        }
    }
}

/// Each enabled plugin owns a Lua state and all of its callbacks. Dropping
/// this handle removes the complete set together with Lua's heap.
pub struct LuaPlugin {
    file: PathBuf,
    lua: Lua,
    state: Rc<State>,
}

impl LuaPlugin {
    pub fn load(
        file: &Path,
        data: &[u8],
        capabilities: &[String],
        files: Option<PluginFiles>,
        options: toml::Table,
    ) -> Result<Self, String> {
        let lua = Lua::new_with(
            StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH,
            LuaOptions::default(),
        )
        .map_err(|cause| report(file, &cause))?;
        lua.set_memory_limit(MEMORY_LIMIT)
            .map_err(|cause| report(file, &cause))?;
        let state = Rc::new(State {
            capabilities: capabilities.to_vec(),
            options,
            files: RefCell::new(files),
            modules: RefCell::default(),
            registrations: RefCell::new(Registrations::default()),
            call: RefCell::default(),
            fs: RefCell::default(),
            command_cache: RefCell::default(),
            budget: Cell::new(0),
            deadline: Cell::new(None),
        });
        let clock = Rc::clone(&state);
        lua.set_hook(
            HookTriggers::new().every_nth_instruction(INSTRUCTION_INTERVAL),
            move |_, _| {
                let remaining = clock.budget.get();
                if remaining == 0
                    || clock
                        .deadline
                        .get()
                        .is_some_and(|limit| Instant::now() >= limit)
                {
                    return Err(error("Lua callback execution budget exceeded"));
                }
                clock.budget.set(remaining - 1);
                Ok(VmState::Continue)
            },
        )
        .map_err(|cause| report(file, &cause))?;

        let plugin = Self {
            file: file.to_owned(),
            lua,
            state,
        };
        api::install(&plugin.lua, &plugin.state).map_err(|cause| report(file, &cause))?;
        let env = std::env::vars_os()
            .map(|(key, value)| (key.as_bytes().to_vec(), value.as_bytes().to_vec()))
            .collect();
        let shell = CallContext {
            env: Rc::new(env),
            cwd: std::env::current_dir().unwrap_or_default(),
            ..CallContext::default()
        };
        plugin.invoke(Phase::Register, shell, None, || {
            plugin
                .lua
                .load(data)
                .set_name(format!("@{}", file.display()))
                .set_mode(ChunkMode::Text)
                .exec()
        })?;
        plugin.state.files.take();
        Ok(plugin)
    }

    pub fn commands(&self) -> Vec<(Vec<u8>, CommandMode)> {
        self.state
            .registrations
            .borrow()
            .commands
            .iter()
            .map(|(name, (mode, _))| (name.clone(), *mode))
            .collect()
    }

    pub fn command_mode(&self, name: &[u8]) -> Option<CommandMode> {
        self.state
            .registrations
            .borrow()
            .commands
            .get(name)
            .map(|entry| entry.0)
    }

    pub fn call_command(
        &self,
        name: &[u8],
        args: &[Vec<u8>],
        ctx: CallContext,
        wrapped: Option<i32>,
    ) -> Result<(i32, CallContext), String> {
        let callback = {
            let handlers = self.state.registrations.borrow();
            let (_, handle) = handlers.commands.get(name).ok_or_else(|| {
                format!(
                    "{}: Lua command is no longer registered",
                    self.file.display()
                )
            })?;
            self.callback(handle)?
        };
        let (status, call) = self.invoke(Phase::Command, ctx, None, || {
            let args = api::string_table(&self.lua, args)?;
            callback.call::<Option<i32>>((args, wrapped))
        })?;
        Ok((status.or(wrapped).unwrap_or(0) & 255, call.shell))
    }

    pub fn event(
        &self,
        name: &str,
        args: &[Vec<u8>],
        ctx: CallContext,
    ) -> Result<CallContext, String> {
        let callbacks = self.callbacks(self.state.registrations.borrow().events.get(name))?;
        if callbacks.is_empty() {
            return Ok(ctx);
        }
        let ((), call) = self.invoke(Phase::Event, ctx, None, || {
            for callback in callbacks {
                callback.call::<()>(api::string_table(&self.lua, args)?)?;
            }
            Ok(())
        })?;
        Ok(call.shell)
    }

    pub fn has_event(&self, name: &str) -> bool {
        self.state.registrations.borrow().events.contains_key(name)
    }

    pub fn has_provider(&self, provider: Provider) -> bool {
        self.state
            .registrations
            .borrow()
            .providers
            .contains_key(&provider)
    }

    pub fn has_editor(&self) -> bool {
        let callbacks = self.state.registrations.borrow();
        !callbacks.change.is_empty()
            || !callbacks.tick.is_empty()
            || !callbacks.keys.is_empty()
            || callbacks.providers.contains_key(&Provider::Highlight)
            || callbacks.providers.contains_key(&Provider::Completion)
    }

    pub fn pending_editor_work(&self) -> bool {
        self.state.fs.borrow().pending()
    }

    pub fn prompt(
        &self,
        default: &str,
        ctx: CallContext,
        colors: TerminalColors,
    ) -> Result<Option<String>, String> {
        let Some(callback) = self.provider(Provider::Prompt)? else {
            return Ok(None);
        };
        let status = ctx.status;
        let (text, _) = self.invoke(Phase::ReadOnly, ctx, None, || {
            api::prompt_text(&self.lua, callback.call((default, status))?, colors)
        })?;
        Ok(text)
    }

    pub fn history(&self, ctx: CallContext) -> Result<Option<Vec<Vec<u8>>>, String> {
        let Some(callback) = self.provider(Provider::History)? else {
            return Ok(None);
        };
        let defaults = api::string_table(&self.lua, &ctx.history)
            .map_err(|cause| report(&self.file, &cause))?;
        let (entries, _) = self.invoke(Phase::ReadOnly, ctx, None, || {
            callback
                .call::<Option<mlua::Table>>(defaults)?
                .map(api::byte_list)
                .transpose()
        })?;
        Ok(entries)
    }

    pub fn editor(
        &self,
        event: EditorEvent<'_>,
        frame: EditorFrame<'_>,
        ctx: CallContext,
    ) -> Result<EditorEffects, String> {
        if matches!(event, EditorEvent::Tick) {
            self.state.fs.borrow_mut().poll();
        }
        let mut key = None;
        let mut should_render = false;
        let mut completion = None;
        let mut highlighting = None;
        let callbacks = {
            let handlers = self.state.registrations.borrow();
            match event {
                EditorEvent::Change => {
                    should_render = true;
                    highlighting = self.provider(Provider::Highlight)?;
                    self.callbacks(Some(&handlers.change))?
                }
                EditorEvent::Tick => {
                    should_render = true;
                    highlighting = self.provider(Provider::Highlight)?;
                    self.callbacks(Some(if handlers.tick.is_empty() {
                        &handlers.change
                    } else {
                        &handlers.tick
                    }))?
                }
                EditorEvent::Key { key: label, action } => {
                    key = Some((label, action));
                    if label == "Tab" {
                        completion = self.provider(Provider::Completion)?;
                    }
                    handlers
                        .keys
                        .get(label)
                        .map(|callback| self.callback(callback).map(|value| vec![value]))
                        .transpose()?
                        .unwrap_or_default()
                }
            }
        };
        if callbacks.is_empty() && highlighting.is_none() && completion.is_none() {
            return Ok(EditorEffects::default());
        }
        let (consume, call) = self.invoke(Phase::ReadOnly, ctx, Some(Edit::new(&frame)), || {
            let mut consume = false;
            for callback in callbacks {
                if let Some(key) = key {
                    consume |= callback.call::<Option<bool>>(key)?.unwrap_or(false);
                } else {
                    callback.call::<()>((frame.line, frame.cursor))?;
                }
            }
            if let Some(callback) = highlighting {
                if let Some(colors) =
                    callback.call::<Option<mlua::Table>>((frame.line, frame.cursor))?
                {
                    let highlights = api::highlights(&self.lua, colors)?;
                    self.state.edit(|edit| {
                        edit.render.highlights = Some(highlights);
                        edit.render_changed = true;
                        Ok(())
                    })?;
                }
            }
            if !consume {
                if let Some(callback) = completion {
                    if let Some(menu) =
                        callback.call::<Option<mlua::Table>>((frame.line, frame.cursor))?
                    {
                        let menu = api::completion_menu(&self.lua, menu)?;
                        consume = !menu.items.is_empty();
                        self.state.edit(|edit| {
                            edit.render.menu = Some(menu);
                            edit.render_changed = true;
                            Ok(())
                        })?;
                    }
                }
            }
            Ok(consume)
        })?;
        Ok(call.edit.map_or_else(EditorEffects::default, |edit| {
            edit.finish(consume, should_render)
        }))
    }

    fn callback(&self, key: &RegistryKey) -> Result<Function, String> {
        self.lua
            .registry_value(key)
            .map_err(|cause| report(&self.file, &cause))
    }

    fn callbacks(&self, keys: Option<&Vec<RegistryKey>>) -> Result<Vec<Function>, String> {
        keys.into_iter()
            .flatten()
            .map(|key| self.callback(key))
            .collect()
    }

    fn provider(&self, provider: Provider) -> Result<Option<Function>, String> {
        self.state
            .registrations
            .borrow()
            .providers
            .get(&provider)
            .map(|key| self.callback(key))
            .transpose()
    }

    fn invoke<T>(
        &self,
        phase: Phase,
        shell: CallContext,
        edit: Option<Edit>,
        callback: impl FnOnce() -> mlua::Result<T>,
    ) -> Result<(T, Call), String> {
        if self.state.call.borrow().is_some() {
            return Err(format!("{}: reentrant callback", self.file.display()));
        }
        self.state.call.replace(Some(Call { shell, phase, edit }));
        self.state.budget.set(INSTRUCTION_SAMPLES);
        let timeout = if matches!(phase, Phase::ReadOnly) {
            35
        } else {
            300
        };
        self.state
            .deadline
            .set(Some(Instant::now() + Duration::from_millis(timeout)));
        let result = callback();
        self.state.deadline.set(None);
        let call = self.state.call.take().expect("callback context installed");
        result
            .map(|result| (result, call))
            .map_err(|cause| report(&self.file, &cause))
    }
}

fn error(message: impl Into<String>) -> mlua::Error {
    mlua::Error::RuntimeError(message.into())
}

fn report(file: &Path, cause: &mlua::Error) -> String {
    format!("{}: {cause}", file.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(source: &str, caps: &[&str]) -> Result<LuaPlugin, String> {
        LuaPlugin::load(
            Path::new("/fixtures/lua/init.lua"),
            source.as_bytes(),
            &caps.iter().map(|cap| cap.to_string()).collect::<Vec<_>>(),
            None,
            toml::Table::new(),
        )
    }

    #[test]
    fn raw_arguments_mutation_and_status() {
        let lua = load(
            r#"
            nsh.command.register('fixture', function(args)
                nsh.shell.set("RAW", args[1])
                assert(nsh.shell.get("RAW") == args[1])
                return 13
            end)
        "#,
            &["shell-state"],
        )
        .unwrap();
        let value = vec![0xff, 0xfe, b' '];
        let (status, state) = lua
            .call_command(
                b"fixture",
                std::slice::from_ref(&value),
                CallContext::default(),
                None,
            )
            .unwrap();
        assert_eq!(status, 13);
        assert_eq!(
            state.effects,
            [Effect::SetVar(b"RAW".to_vec(), Some(value))]
        );
    }

    #[test]
    fn duplicate_and_override_have_explicit_errors() {
        let duplicate = load(
            r#"
            nsh.command.register('same', function() end)
            nsh.command.register('same', function() end)
        "#,
            &[],
        );
        assert!(duplicate.err().unwrap().contains("duplicate Lua command"));
        let blocked = load("nsh.command.replace('echo', function() end)", &[]);
        assert!(blocked.err().unwrap().contains("'override'"));
        let lua = load(
            "nsh.command.wrap('echo', function(args, status) return status end)",
            &["override"],
        )
        .unwrap();
        assert_eq!(lua.command_mode(b"echo"), Some(CommandMode::Wrap));
        assert_eq!(
            lua.call_command(b"echo", &[], CallContext::default(), Some(11))
                .unwrap()
                .0,
            11
        );
    }

    #[test]
    fn callback_errors_include_file_and_line_and_do_not_leak_actions() {
        let lua = load(
            "nsh.command.register('bad', function()\nnsh.shell.set('VAR', 'bad')\nerror('boom')\nend)\n\
             nsh.command.register('good', function() assert(nsh.shell.get('VAR') == nil) end)",
            &["shell-state"],
        ).unwrap();
        let error = lua
            .call_command(b"bad", &[], CallContext::default(), None)
            .unwrap_err();
        assert!(error.contains("init.lua:3"), "{error}");
        assert!(error.contains("boom"));
        assert!(
            lua.call_command(b"good", &[], CallContext::default(), None)
                .unwrap()
                .1
                .effects
                .is_empty()
        );
    }

    #[test]
    fn lua_instruction_and_heap_limits_apply() {
        let instruction = load("while true do end", &[]).err().unwrap();
        assert!(instruction.contains("budget exceeded"), "{instruction}");
        let heap = load("local value=string.rep('x', 10*1024*1024)", &[])
            .err()
            .unwrap();
        assert!(heap.contains("memory"), "{heap}");
    }

    #[test]
    fn editor_callbacks_keep_raw_ghost_separate_and_apply_utf8_ranges() {
        let lua = load(
            r#"
            nsh.editor.on_change(function(line, cursor)
                assert(line == "λ")
                assert(cursor == 2)
                nsh.editor.set_ghost_text("ú")
            end)
            nsh.keys.bind("Right", function()
                nsh.editor.replace(2, 2, "ú")
                nsh.editor.set_cursor(4)
                return true
            end)
        "#,
            &[],
        )
        .unwrap();
        let frame = || EditorFrame {
            line: "λ",
            cursor: 2,
            history: &[],
        };
        let ghost = lua
            .editor(EditorEvent::Change, frame(), CallContext::default())
            .unwrap();
        assert_eq!(ghost.render.unwrap().ghost.as_deref(), Some("ú"));
        assert!(ghost.replacement.is_none());
        let applied = lua
            .editor(
                EditorEvent::Key {
                    key: "Right",
                    action: "forward-char",
                },
                frame(),
                CallContext::default(),
            )
            .unwrap();
        assert!(applied.consume);
        assert_eq!(applied.replacement.unwrap().text, "λú");
        assert_eq!(applied.cursor, Some(4));
    }

    #[test]
    fn lifecycle_and_provider_delivery() {
        let lua = load(
            r#"
            local active = false
            nsh.on("enable", function(args) active = args[1] == "x" end)
            nsh.prompt.register(function(default, status)
                assert(default == "% ")
                return active and status == 9 and "Lua λ " or nil
            end)
            nsh.history.register(function() return {"saved"} end)
        "#,
            &[],
        )
        .unwrap();
        assert!(lua.has_event("enable") && lua.has_provider(Provider::Prompt));
        let shell = CallContext {
            status: 9,
            ..CallContext::default()
        };
        assert_eq!(
            lua.prompt("% ", shell.clone(), TerminalColors::Plain)
                .unwrap(),
            None
        );
        let shell = lua.event("enable", &[b"x".to_vec()], shell).unwrap();
        assert_eq!(
            lua.prompt("% ", shell, TerminalColors::Plain)
                .unwrap()
                .as_deref(),
            Some("Lua λ ")
        );
        assert_eq!(
            lua.history(CallContext::default()).unwrap(),
            Some(vec![b"saved".to_vec()])
        );
    }
}
