local palette = nsh.config.get()
nsh.style.define("path", {fg = palette.path, bold = true})
nsh.style.define("success", {fg = palette.success, bold = true})
nsh.style.define("failure", {fg = palette.failure, bold = true})
nsh.style.define("shortcut", {fg = palette.menu_text})
nsh.style.define("selected", {
    fg = palette.menu_selected_text, bg = palette.menu_selected_background, bold = true,
})

nsh.prompt.register(function(_, status)
    local path = nsh.shell.cwd()
    if not utf8.len(path) then return nil end
    return {
        {text = path, style = "path"},
        {text = "\n"},
        {text = "❯", style = status == 0 and "success" or "failure"},
        {text = " "},
    }
end)

local shortcuts = {"pwd", "ls", "nsh config list"}
local selected

nsh.on("before-prompt", function() selected = nil end)

local function menu()
    return {items = shortcuts, selected = selected, style = "shortcut", selected_style = "selected"}
end

nsh.completion.register(function(line)
    if line == "" then
        selected = 1
        return menu()
    end
end)

nsh.editor.on_change(function() selected = nil end)

local function show_menu()
    nsh.editor.set_menu(shortcuts, menu())
end

nsh.editor.on_tick(function()
    if selected then show_menu() end
end)

local function move(direction)
    if not selected then return false end
    selected = (selected - 1 + direction) % #shortcuts + 1
    show_menu()
    return true
end

nsh.keys.bind("Up", function() return move(-1) end)
nsh.keys.bind("Down", function() return move(1) end)

nsh.keys.bind("Escape", function()
    if not selected then return false end
    selected = nil
    nsh.editor.set_menu(nil)
    return true
end)

nsh.keys.bind("Enter", function()
    if not selected then return false end
    nsh.editor.set_line(shortcuts[selected])
    selected = nil
    nsh.editor.set_menu(nil)
    return true
end)
