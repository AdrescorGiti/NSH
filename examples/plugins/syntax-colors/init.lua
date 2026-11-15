local defaults = {
    command = "#7CD28A", unknown = "#F07178", option = "#82AAFF",
    string = "#F5DA84", path = "#87D1D3", variable = "#C59FEA",
    operator = "#FFA77A", comment = "#798399",
}
local cache = {}
local backtick = string.char(96)
local operators = "><|&;()"
local colors = nsh.config and nsh.config.get("colors", {}) or {}
if type(colors) ~= "table" then error("options.colors must be a table") end

local function color(kind)
    local override = nsh.env.get("NSH_SYNTAX_" .. kind:upper())
    if override and override:match("^#%x%x%x%x%x%x$") then return override end
    local configured = colors[kind]
    if type(configured) == "string" and configured:match("^#%x%x%x%x%x%x$") then return configured end
    return defaults[kind]
end

-- Memoize the last command per frame too, so argument edits can reuse its
-- resolution without traversing PATH. The session API tracks PATH and cwd.
local function command_kind(word)
    local known = cache[word]
    if known == nil then
        known = nsh.command.exists(word)
        cache[word] = known
    end
    return known and "command" or "unknown"
end

local previous_program, previous_directory, previous_path
nsh.highlight.register(function(line)
    if #line > 16384 then return {} end
    local ranges, i, command = {}, 1, true
    local program = line:match("^%s*([^%s;|&<>()]+)")
    local path, directory = nsh.env.get("PATH"), nsh.shell.cwd()
    if program ~= previous_program or path ~= previous_path or directory ~= previous_directory then
        cache = {}
        previous_program, previous_path, previous_directory = program, path, directory
    end
    while i <= #line do
        local from, kind = i, nil
        local ch = line:sub(i, i)
        if ch:match("%s") then
            if ch == "\n" then command = true end
            i = i + 1
        elseif ch == "#" and (i == 1 or line:sub(i - 1, i - 1):match("%s")) then
            kind = "comment"
            i = line:find("\n", i, true) or (#line + 1)
        elseif operators:find(ch, 1, true) then
            kind = "operator"
            if ch:find("[|&;()]") then command = true end
            i = i + 1
        elseif ch == "'" or ch == '"' then
            kind = "string"
            i = i + 1
            while i <= #line do
                local next_byte = line:sub(i, i)
                i = i + 1
                if next_byte == ch then break end
                if next_byte == "\\" and ch == '"' and i <= #line then i = i + 1 end
            end
            command = false
        elseif ch == "$" then
            kind = "variable"
            local _, end_byte = line:find("^%$%b{}", i)
            if not end_byte then _, end_byte = line:find("^%$[%w_]+", i) end
            i = (end_byte or i) + 1
        else
            repeat
                if line:sub(i, i) == "\\" and i < #line then i = i + 1 end
                i = i + 1
                ch = line:sub(i, i)
            until i > #line or ch:match("%s") or operators:find(ch, 1, true)
                or ('"\'$' .. backtick):find(ch, 1, true)
            local word = line:sub(from, i - 1)
            if command and word:match("^[%a_][%w_]*=") then
                kind = "variable"
            elseif command then
                kind, command = command_kind(word), false
            elseif word:sub(1, 1) == "-" then
                kind = "option"
            elseif word:find("/", 1, true) or word:sub(1, 1) == "~" then
                kind = "path"
            end
        end
        if kind then
            ranges[#ranges + 1] = {start = from - 1, ["end"] = i - 1, color = color(kind)}
        end
    end
    return ranges
end)
