//! Trusted-file discovery and transactional activation of Lua plugins.
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;

use semver::{Version, VersionReq};
use serde::Deserialize;

use crate::plugin_lua::{CallContext, CommandMode, LuaPlugin, PluginFiles};
use crate::session::files::{self, Dir};
use crate::shell::diagnostic;

pub const API_VERSION: u32 = 1;
const CAPABILITIES: &[&str] = &[
    "shell-state",
    "process",
    "fs",
    "stdio",
    "exec",
    "override",
    "special-builtins",
];

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Manifest {
    name: String,
    version: String,
    api_version: u32,
    entry: PathBuf,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    priority: i32,
    #[serde(default = "any_version")]
    nsh_version: String,
    #[serde(default)]
    options: PluginOptions,
}

fn any_version() -> String {
    "*".into()
}

#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
struct PluginOptions(toml::Table);

impl PartialEq for PluginOptions {
    fn eq(&self, other: &Self) -> bool {
        options_equal(&self.0, &other.0)
    }
}

impl Eq for PluginOptions {}

fn options_equal(left: &toml::Table, right: &toml::Table) -> bool {
    left.len() == right.len()
        && left.iter().all(|(key, value)| {
            right
                .get(key)
                .is_some_and(|other| option_equal(value, other))
        })
}

fn option_equal(left: &toml::Value, right: &toml::Value) -> bool {
    use toml::Value;
    match (left, right) {
        // NaN must compare equal to the same configuration on the next read,
        // and Lua distinguishes negative zero from positive zero.
        (Value::Float(left), Value::Float(right)) => left.to_bits() == right.to_bits(),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len() && left.iter().zip(right).all(|(a, b)| option_equal(a, b))
        }
        (Value::Table(left), Value::Table(right)) => options_equal(left, right),
        _ => left == right,
    }
}

struct Source {
    manifest: Manifest,
    path: PathBuf,
    data: Vec<u8>,
    directory_identity: (u64, u64),
}

struct Root {
    path: PathBuf,
    directory: Dir,
}

impl Root {
    fn open(create: bool) -> Result<Option<Self>, String> {
        let Some(path) = files::base("XDG_CONFIG_HOME", ".config").map(|path| path.join("plugins"))
        else {
            return Ok(None);
        };
        match Dir::open(&path, create, false) {
            Ok(directory) => Ok(Some(Self { path, directory })),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    fn required() -> Result<Self, String> {
        Self::open(false)?.ok_or_else(|| "plugin directory does not exist".into())
    }

    fn plugin_dir(&self, name: &str) -> Result<Dir, String> {
        self.directory
            .child(OsStr::new(name), false, false)
            .map_err(|cause| format!("plugin {name}: {}: {cause}", self.path.join(name).display()))
    }

    fn read(&self, name: &str) -> Result<Source, String> {
        valid_name(name)?;
        let base = self.path.join(name);
        let manifest_path = base.join("plugin.toml");
        let dir = self.plugin_dir(name)?;
        let directory_identity = dir
            .identity()
            .map_err(|cause| format!("plugin {name}: {}: {cause}", base.display()))?;
        let read_manifest = || -> Result<Manifest, String> {
            let bytes = dir
                .read(OsStr::new("plugin.toml"), 65536, false)
                .map_err(|e| e.to_string())?;
            let contents = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
            let manifest: Manifest = toml::from_str(contents).map_err(|e| e.to_string())?;
            if manifest.name != name {
                return Err("manifest name must match its directory".into());
            }
            if manifest.api_version != API_VERSION {
                return Err(format!(
                    "API {} is incompatible with API {API_VERSION}",
                    manifest.api_version
                ));
            }
            let _ = Version::parse(&manifest.version).map_err(|e| format!("version: {e}"))?;
            let required = VersionReq::parse(&manifest.nsh_version)
                .map_err(|e| format!("nsh_version: {e}"))?;
            let current = Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo package version");
            if !required.matches(&current) {
                return Err(format!("requires NSH {required}; running {current}"));
            }
            for capability in &manifest.capabilities {
                if !CAPABILITIES.contains(&capability.as_str()) {
                    return Err(format!("unknown capability {capability:?}"));
                }
            }
            if manifest
                .capabilities
                .iter()
                .any(|c| c == "special-builtins")
                && !manifest.capabilities.iter().any(|c| c == "override")
            {
                return Err("special-builtins also requires override".into());
            }
            Ok(manifest)
        };
        let manifest = read_manifest()
            .map_err(|e| format!("plugin {name}: {}: {e}", manifest_path.display()))?;
        let path = base.join(&manifest.entry);
        let files = PluginFiles::new(base, dir);
        let data = files
            .read(&manifest.entry)
            .map_err(|e| format!("plugin {name}: {}: {e}", path.display()))?;
        Ok(Source {
            manifest,
            path,
            data,
            directory_identity,
        })
    }

    fn enabled(&self) -> Result<BTreeSet<String>, String> {
        let bytes = match self.directory.read(OsStr::new("enabled"), 65536, true) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
            Err(error) => return Err(format!("{}: {error}", self.path.join("enabled").display())),
        };
        let content = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
        content
            .lines()
            .filter(|line| !line.is_empty())
            .map(|name| {
                valid_name(name)?;
                Ok(name.to_owned())
            })
            .collect()
    }

    fn list(&self) -> Result<Vec<String>, String> {
        let mut names = self.directory.list().map_err(|e| e.to_string())?;
        names.retain(|name| name != "enabled" && valid_name(name).is_ok());
        names.sort();
        Ok(names)
    }

    fn order(&self, requested: &BTreeSet<String>) -> Result<Vec<Source>, String> {
        let mut sources = BTreeMap::new();
        for name in requested {
            self.dependency(name, &mut sources, &mut BTreeSet::new())?;
        }
        let mut ordered = Vec::new();
        let mut ready = BTreeSet::new();
        while !sources.is_empty() {
            let next = sources
                .iter()
                .filter(|(_, source)| {
                    source
                        .manifest
                        .dependencies
                        .iter()
                        .all(|dep| ready.contains(dependency_name(dep)))
                })
                .min_by_key(|(name, source)| (source.manifest.priority, *name))
                .map(|(name, _)| name.clone())
                .ok_or("plugin dependency cycle")?;
            let source = sources.remove(&next).expect("selected manifest");
            ready.insert(next);
            ordered.push(source);
        }
        Ok(ordered)
    }

    fn dependency(
        &self,
        name: &str,
        sources: &mut BTreeMap<String, Source>,
        pending: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        if sources.contains_key(name) {
            return Ok(());
        }
        if !pending.insert(name.to_owned()) {
            return Err(format!("plugin dependency cycle at {name}"));
        }
        if pending.len() > 64 || sources.len() > 256 {
            return Err("plugin dependency limit exceeded".into());
        }
        let source = self.read(name)?;
        for requirement in &source.manifest.dependencies {
            let dependency = dependency_name(requirement);
            self.dependency(dependency, sources, pending)?;
            if let Some((_, raw)) = requirement.split_once('@') {
                let req = VersionReq::parse(raw)
                    .map_err(|e| format!("plugin {name}: dependency {requirement}: {e}"))?;
                let v = Version::parse(&sources[dependency].manifest.version)
                    .expect("validated version");
                if !req.matches(&v) {
                    return Err(format!("plugin {name}: {requirement} required; found {v}"));
                }
            }
        }
        pending.remove(name);
        sources.insert(name.to_owned(), source);
        Ok(())
    }
}

fn dependency_name(dependency: &str) -> &str {
    dependency
        .split_once('@')
        .map_or(dependency, |(name, _)| name)
}

fn valid_name(name: &str) -> Result<(), String> {
    if (1..=80).contains(&name.len())
        && name
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    {
        Ok(())
    } else {
        Err(format!("invalid plugin name {name:?}"))
    }
}

pub fn saved() -> Result<BTreeSet<String>, String> {
    Root::open(false)?.map_or_else(|| Ok(BTreeSet::new()), |root| root.enabled())
}

pub fn validate(enabled: &BTreeSet<String>) -> Result<(), String> {
    if !enabled.is_empty() {
        Root::required()?.order(enabled)?;
    }
    Ok(())
}

pub fn save(name: &str, enabled: bool) -> Result<(), String> {
    valid_name(name)?;
    let root = Root::required()?;
    let _lock = root.directory.lock().map_err(|error| error.to_string())?;
    let mut wanted = root.enabled()?;
    if enabled {
        wanted.insert(name.into());
        root.order(&wanted)?;
    } else {
        wanted.remove(name);
    }
    let content = wanted
        .into_iter()
        .map(|name| name + "\n")
        .collect::<String>();
    root.directory
        .replace(OsStr::new("enabled"), content.as_bytes())
        .map_err(|error| error.to_string())
}

pub fn inspect(args: &[Vec<u8>], active: Option<&BTreeSet<String>>) -> Result<String, String> {
    let root = Root::open(false)?;
    let mut enabled = match active {
        Some(set) => set.clone(),
        None => root
            .as_ref()
            .map_or_else(|| Ok(BTreeSet::new()), Root::enabled)?,
    };
    let mut out = String::new();
    match args {
        [cmd] if cmd == b"list" => {
            if let Some(root) = root {
                if let Ok(sources) = root.order(&enabled) {
                    enabled.extend(sources.into_iter().map(|source| source.manifest.name));
                }
                for name in root.list()? {
                    let state = if enabled.contains(&name) {
                        "enabled"
                    } else {
                        "available"
                    };
                    out.push_str(&format!("{name}\t{state}\n"));
                }
            }
        }
        [cmd, name] if cmd == b"info" => {
            let name = std::str::from_utf8(name).map_err(|_| "plugin name must be UTF-8")?;
            let source = root.ok_or("plugin directory does not exist")?.read(name)?;
            let meta = source.manifest;
            out = format!(
                "{} {} (API {})\n{}\nAuthor: {}\nEntry: {}\nDependencies: {}\nCapabilities: {}\nPriority: {}\nNSH: {}\n",
                meta.name,
                meta.version,
                meta.api_version,
                meta.description,
                meta.author,
                source.path.display(),
                meta.dependencies.join(", "),
                meta.capabilities.join(", "),
                meta.priority,
                meta.nsh_version
            );
        }
        [cmd] if cmd == b"doctor" => {
            if let Some(root) = root {
                let mut errors = Vec::new();
                for name in root.list()? {
                    if let Err(error) = root.order(&BTreeSet::from([name])) {
                        errors.push(error);
                    }
                }
                if !errors.is_empty() {
                    return Err(errors.join("\n"));
                }
            }
            out = format!("Plugin API {API_VERSION}: manifest checks passed\n");
        }
        _ => {
            return Err(
                "usage: nsh plugin list|info NAME|doctor|enable NAME|disable NAME|reload NAME"
                    .into(),
            );
        }
    }
    Ok(out)
}

struct Loaded {
    manifest: Manifest,
    path: PathBuf,
    lua: LuaPlugin,
}

impl Loaded {
    fn label(&self, error: &str) -> String {
        format!(
            "plugin {}: {}: {error}",
            self.manifest.name,
            self.path.display()
        )
    }
    fn lifecycle(
        &self,
        name: &str,
        args: &[Vec<u8>],
        context: CallContext,
    ) -> Result<CallContext, String> {
        self.lua
            .event(name, args, context.clone())
            .map_err(|error| {
                if name != "error" {
                    let _ = self
                        .lua
                        .event("error", &[error.as_bytes().to_vec()], context);
                }
                self.label(&error)
            })
    }
    fn capability(&self, key: &str) -> bool {
        self.manifest
            .capabilities
            .iter()
            .any(|capability| capability == key)
    }
    fn accepts(&self, name: &[u8], mode: CommandMode) -> bool {
        name != b"nsh"
            && (!crate::builtins::special(name)
                || mode == CommandMode::Replace && self.capability("special-builtins"))
            && (mode == CommandMode::Register || self.capability("override"))
    }
}

#[derive(Default)]
pub struct Plugins {
    pub requested: BTreeSet<String>,
    active: Vec<Loaded>,
    provider_order: Vec<usize>,
}

pub struct Stage {
    wanted: BTreeSet<String>,
    order: Vec<String>,
    replacements: BTreeMap<String, Loaded>,
    pub context: CallContext,
}

impl Plugins {
    pub fn contains(&self, name: &str) -> bool {
        self.active.iter().any(|p| p.manifest.name == name)
    }

    pub fn command_names(&self) -> impl Iterator<Item = Vec<u8>> + '_ {
        self.active.iter().flat_map(|p| {
            p.lua
                .commands()
                .into_iter()
                .filter(|(name, mode)| p.accepts(name, *mode))
                .map(|(name, _)| name)
        })
    }

    pub fn stage(
        &self,
        wanted: BTreeSet<String>,
        reload: Option<&str>,
        mut context: CallContext,
        reserved: impl Fn(&[u8]) -> bool,
    ) -> Result<Stage, String> {
        let root = (!wanted.is_empty()).then(Root::required).transpose()?;
        let sources = root
            .as_ref()
            .map(|root| root.order(&wanted))
            .transpose()?
            .unwrap_or_default();
        let mut order = Vec::new();
        let mut replacements = BTreeMap::new();
        for Source {
            manifest,
            path,
            data,
            directory_identity,
        } in sources
        {
            let key = manifest.name.clone();
            order.push(key.clone());
            if reload != Some(key.as_str())
                && self.active.iter().any(|old| old.manifest == manifest)
            {
                continue;
            }
            let root = root
                .as_ref()
                .expect("plugin sources require a root directory");
            // Hold only the current plugin directory open while executing Lua.
            // Recheck its identity so the entry and required modules use the
            // directory that passed preflight.
            let directory = root.plugin_dir(&key)?;
            let current = directory
                .identity()
                .map_err(|cause| format!("plugin {key}: {}: {cause}", path.display()))?;
            if current != directory_identity {
                return Err(format!(
                    "plugin {key}: {}: plugin directory changed during activation",
                    path.display()
                ));
            }
            let lua = LuaPlugin::load(
                &path,
                &data,
                &manifest.capabilities,
                Some(PluginFiles::new(root.path.join(&key), directory)),
                manifest.options.0.clone(),
            )
            .map_err(|e| format!("plugin {key}: {}: {e}", path.display()))?;
            let plugin = Loaded {
                manifest,
                path,
                lua,
            };
            context = plugin.lifecycle("load", &[], context)?;
            context = plugin.lifecycle("enable", &[], context)?;
            if reload == Some(key.as_str()) {
                context = plugin.lifecycle("reload", &[], context)?;
            }
            replacements.insert(key, plugin);
        }
        let mut registered = BTreeSet::new();
        for name in &order {
            let plugin = replacements
                .get(name)
                .or_else(|| self.active.iter().find(|old| old.manifest.name == *name))
                .expect("ordered plugin");
            for (command, mode) in plugin.lua.commands() {
                let duplicated = !registered.insert(command.clone());
                if !plugin.accepts(&command, mode)
                    || mode == CommandMode::Register && (duplicated || reserved(&command))
                {
                    return Err(plugin.label(&format!(
                        "command {:?} conflicts or lacks override permission",
                        String::from_utf8_lossy(&command)
                    )));
                }
            }
        }
        Ok(Stage {
            wanted,
            order,
            replacements,
            context,
        })
    }

    pub fn commit(&mut self, mut stage: Stage, context: &CallContext) {
        for plugin in self.active.iter().rev().filter(|plugin| {
            !stage.order.contains(&plugin.manifest.name)
                || stage.replacements.contains_key(&plugin.manifest.name)
        }) {
            self.stop(plugin, context, false);
        }
        let mut existing = std::mem::take(&mut self.active)
            .into_iter()
            .map(|plugin| (plugin.manifest.name.clone(), plugin))
            .collect::<BTreeMap<_, _>>();
        for name in stage.order {
            let active = stage
                .replacements
                .remove(&name)
                .or_else(|| existing.remove(&name))
                .expect("staged plugin");
            self.active.push(active);
        }
        self.provider_order = (0..self.active.len()).collect();
        self.provider_order.sort_by_key(|&index| {
            let manifest = &self.active[index].manifest;
            (std::cmp::Reverse(manifest.priority), manifest.name.as_str())
        });
        self.requested = stage.wanted;
    }

    fn stop(&self, plugin: &Loaded, context: &CallContext, shutdown: bool) {
        let events = if shutdown {
            &["shutdown", "disable", "unload"][..]
        } else {
            &["disable", "unload"]
        };
        for event in events {
            if let Err(error) = plugin.lifecycle(event, &[], context.clone()) {
                diagnostic(&error);
            }
        }
    }

    pub fn shutdown(&mut self, context: &CallContext) {
        for plugin in self.active.iter().rev() {
            self.stop(plugin, context, true);
        }
        self.active.clear();
        self.provider_order.clear();
        self.requested.clear();
    }

    pub fn command(&self, name: &[u8]) -> Option<CommandMode> {
        self.command_plugin(name).map(|(_, mode)| mode)
    }

    fn command_plugin(&self, name: &[u8]) -> Option<(&Loaded, CommandMode)> {
        self.priority().find_map(|plugin| {
            let mode = plugin.lua.command_mode(name)?;
            plugin.accepts(name, mode).then_some((plugin, mode))
        })
    }

    fn priority(&self) -> impl Iterator<Item = &Loaded> {
        self.provider_order.iter().map(|&index| &self.active[index])
    }

    pub fn invoke(
        &self,
        name: &[u8],
        args: &[Vec<u8>],
        context: CallContext,
        wrapped: Option<i32>,
    ) -> Result<(i32, CallContext), String> {
        let (plugin, _) = self
            .command_plugin(name)
            .ok_or("plugin command disappeared")?;
        plugin
            .lua
            .call_command(name, args, context.clone(), wrapped)
            .map_err(|error| {
                let _ = plugin.lifecycle("error", &[error.as_bytes().to_vec()], context);
                plugin.label(&error)
            })
    }

    pub fn event(&self, event: &str, args: &[Vec<u8>], mut context: CallContext) -> CallContext {
        for plugin in self.priority() {
            match plugin.lifecycle(event, args, context.clone()) {
                Ok(next) => context = next,
                Err(error) => diagnostic(&error),
            }
        }
        context
    }
}

use crate::plugin_editor::{EditorEffects, EditorEvent, EditorFrame, EditorPlugin};
use crate::plugin_lua::Provider;

impl Plugins {
    pub fn has_event(&self, event: &str) -> bool {
        self.active.iter().any(|plugin| plugin.lua.has_event(event))
    }

    pub fn has_editor(&self) -> bool {
        self.active
            .iter()
            .any(|p| p.lua.has_editor() || p.lua.has_provider(Provider::History))
    }

    pub fn has_prompt(&self) -> bool {
        self.active
            .iter()
            .any(|p| p.lua.has_provider(Provider::Prompt))
    }

    pub fn prompt(
        &self,
        default: &str,
        context: CallContext,
        session: &crate::session::Session,
    ) -> Option<String> {
        let colors = session.terminal_colors();
        for plugin in self
            .priority()
            .filter(|p| p.lua.has_provider(Provider::Prompt))
        {
            match plugin.lua.prompt(default, context.clone(), colors) {
                Ok(Some(prompt)) => {
                    return Some(safe_prompt(&prompt, session.get("color.mode") != "never"));
                }
                Ok(None) => {}
                Err(error) => diagnostic(&plugin.label(&error)),
            }
        }
        None
    }

    pub fn history(&self, context: &CallContext) -> Option<Vec<Vec<u8>>> {
        for plugin in self
            .priority()
            .filter(|p| p.lua.has_provider(Provider::History))
        {
            match plugin.lua.history(context.clone()) {
                Ok(Some(history)) => return Some(history),
                Ok(None) => {}
                Err(error) => diagnostic(&plugin.label(&error)),
            }
        }
        None
    }
}

pub struct Bridge<'a> {
    plugins: &'a Plugins,
    context: CallContext,
    errors: BTreeSet<String>,
}

impl<'a> Bridge<'a> {
    pub fn new(plugins: &'a Plugins, context: CallContext) -> Self {
        Self {
            plugins,
            context,
            errors: BTreeSet::new(),
        }
    }
}

impl EditorPlugin for Bridge<'_> {
    fn history(&self) -> Option<Vec<Vec<u8>>> {
        self.plugins.history(&self.context)
    }

    fn on_editor(&mut self, event: EditorEvent<'_>, frame: EditorFrame<'_>) -> EditorEffects {
        let mut combined = EditorEffects::default();
        for plugin in self.plugins.priority().filter(|p| p.lua.has_editor()) {
            if self.errors.contains(&plugin.manifest.name) {
                continue;
            }
            let event = match event {
                EditorEvent::Change => EditorEvent::Change,
                EditorEvent::Tick => EditorEvent::Tick,
                EditorEvent::Key { key, action } => EditorEvent::Key { key, action },
            };
            let frame = EditorFrame {
                line: frame.line,
                cursor: frame.cursor,
                history: frame.history,
            };
            match plugin.lua.editor(event, frame, self.context.clone()) {
                Ok(mut next) => {
                    combined.replacement = combined.replacement.or(next.replacement);
                    combined.cursor = combined.cursor.or(next.cursor);
                    if let Some(mut render) = next.render.take() {
                        let view = combined.render.get_or_insert_with(Default::default);
                        if view.ghost.is_none() {
                            view.ghost = render.ghost;
                            view.ghost_style = render.ghost_style;
                        }
                        if view.highlights.is_none() {
                            view.highlights = render.highlights;
                        }
                        if view.menu.is_none() {
                            view.menu = render.menu.filter(|menu| !menu.items.is_empty());
                        }
                        view.virtual_text.append(&mut render.virtual_text);
                    }
                    if next.consume {
                        combined.consume = true;
                        break;
                    }
                }
                Err(error) => {
                    self.errors.insert(plugin.manifest.name.clone());
                    diagnostic(&plugin.label(&error));
                }
            }
        }
        combined
    }

    fn has_pending_editor_work(&self) -> bool {
        self.plugins
            .active
            .iter()
            .any(|p| p.lua.pending_editor_work())
    }
}

fn safe_prompt(prompt: &str, allow_style: bool) -> String {
    let mut output = String::new();
    let mut input = prompt;
    let color = allow_style
        && crate::sys::tty(2)
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").is_ok_and(|term| !term.is_empty() && term != "dumb");
    let mut changed = false;
    while !input.is_empty() {
        if let Some(sequence) = input.strip_prefix("\x1b[") {
            let length = sequence
                .bytes()
                .take_while(|b| b.is_ascii_digit() || b";:".contains(b))
                .count();
            if sequence.as_bytes().get(length) == Some(&b'm') {
                if color && length <= 64 {
                    output.push_str(&input[..length + 3]);
                    changed = true;
                }
                input = &sequence[length + 1..];
                continue;
            }
        }
        let character = input.chars().next().unwrap();
        if !character.is_control() || character == '\n' {
            output.push(character);
        }
        input = &input[character.len_utf8()..];
    }
    if changed {
        output.push_str("\x1b[0m");
    }
    output
}
