local backtick = string.char(96)
local last_line, last_cursor, last_suffix
local ghost_style

if nsh.config then
    local style = nsh.config.get("ghost")
    if style then
        nsh.style.define("suggestion", style)
        ghost_style = "suggestion"
    end
end

-- Decode the last shell word. Keep the active quote mode so the suffix we
-- insert has the same meaning as the quoted or escaped path already typed.
local function last_word(line)
    local bytes = {}
    local quote, escaped, dynamic = nil, false, false
    local unquoted_tilde = false
    local started = false

    for i = 1, #line do
        local ch = line:sub(i, i)
        if escaped then
            if quote == '"' and not ('\\"$' .. backtick):find(ch, 1, true) then
                bytes[#bytes + 1] = "\\"
            end
            if ch ~= "\n" then bytes[#bytes + 1] = ch end
            escaped = false
        elseif ch == "\\" and quote ~= "'" then
            escaped, started = true, true
        elseif ch == quote then
            quote = nil
        elseif not quote and (ch == "'" or ch == '"') then
            quote, started = ch, true
        elseif not quote and ch:find("[%s;|&<>()]") then
            bytes, unquoted_tilde, dynamic, started = {}, false, false, false
        elseif quote ~= "'" and (ch == "$" or ch == backtick) then
            dynamic = true
        else
            if not started and not quote and ch == "~" then unquoted_tilde = true end
            if not quote and ch:find("[*?%[]") then dynamic = true end
            bytes[#bytes + 1] = ch
            started = true
        end
    end
    if escaped or dynamic then return nil end
    return table.concat(bytes), quote, unquoted_tilde
end

local function quote_suffix(text, quote)
    if quote == "'" then return (text:gsub("'", "'\\''")) end
    local escaped = {}
    local special = quote == '"' and ('\\"$' .. backtick)
        or (" \t\\'\"$*?[](){}|&;<>!#~" .. backtick)
    for i = 1, #text do
        local ch = text:sub(i, i)
        if special:find(ch, 1, true) then escaped[#escaped + 1] = "\\" end
        escaped[#escaped + 1] = ch
    end
    return table.concat(escaped)
end

local function find_suffix(line, cursor)
    if cursor ~= #line or #line > 16384 then return nil end
    local word, quote, tilde = last_word(line)
    if not word or word == "" then return nil end
    local directory, prefix = word:match("^(.*[/])([^/]*)$")
    if not directory then directory, prefix = ".", word end
    if tilde then
        local home = nsh.env.get("HOME")
        if not home or home == "" then return nil end
        if word == "~" then
            return nsh.fs.read_dir(home, ".nsh-directory-check") and "/" or nil
        end
        if directory:sub(1, 2) ~= "~/" then return nil end
        directory = home .. directory:sub(2)
    end
    local entries = nsh.fs.read_dir(directory, prefix)
    if not entries then return nil end

    local best
    for _, entry in ipairs(entries) do
        local name = entry.name
        if name:sub(1, #prefix) == prefix and not name:find("%c") and utf8.len(name)
            and (name:sub(1, 1) ~= "." or prefix:sub(1, 1) == ".") then
            local safe = true
            for _, point in utf8.codes(name) do
                if (point >= 0x202a and point <= 0x202e) or (point >= 0x2066 and point <= 0x2069) then
                    safe = false
                    break
                end
            end
            local suffix = name:sub(#prefix + 1) .. (entry.is_dir and "/" or "")
            if safe and (not best or #suffix < #best or (#suffix == #best and suffix < best)) then
                best = suffix
            end
        end
    end
    if not best or best == "" then return nil end
    return quote_suffix(best, quote)
end

local function update(line, cursor)
    last_line, last_cursor, last_suffix = line, cursor, find_suffix(line, cursor)
    nsh.editor.set_ghost_text(last_suffix or "", ghost_style)
end

nsh.editor.on_change(update)
nsh.editor.on_tick(update)
nsh.keys.bind("Right", function()
    local line, cursor = nsh.editor.line(), nsh.editor.cursor()
    if last_suffix and last_line == line and last_cursor == cursor then
        nsh.editor.replace(cursor, cursor, last_suffix)
        nsh.editor.set_cursor(cursor + #last_suffix)
        last_suffix = nil
        return true
    end
    return false
end)
