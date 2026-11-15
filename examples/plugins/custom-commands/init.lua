nsh.command.register("hello", function(args)
    nsh.print("Hello from Lua, " .. (args[1] or "NSH") .. "!")
    return 0
end)

nsh.command.register("cdev", function()
    local home = nsh.env.get("HOME")
    if not home then
        nsh.print("HOME is unset")
        return 1
    end
    nsh.shell.cd(home .. "/dev")
    return 0
end)
