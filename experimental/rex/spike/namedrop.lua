local M = {}
local tool = "__TOOL__"
local env = { RD_SERVER = "__RD_SERVER__", RD_STATE = "__RD_STATE__" }

rex.action{
  name = "namedrop_name", title = "Namedrop: Name this pane", category = "Namedrop",
  args = { alias = "string" },
  run = function(ctx, args)
    local r, err = rex.exec({ "/usr/bin/python3", tool, "name", args.alias, "--session", ctx.session_id, "--block", ctx.block_id }, { env = env, timeout = 20 })
    if r == nil then error(err, 0) end
    if not r.ok then error(r.stderr, 0) end
    return { origin = ctx.origin, block = ctx.block_id, output = r.stdout }
  end,
}

rex.action{
  name = "namedrop_list", title = "Namedrop: Show names", category = "Namedrop",
  run = function(ctx, args)
    local w = rex.session.view()
    local res, err = rex.call("session.new_layer", {
      session_id = ctx.session_id, window_id = w.active_window_id, bounds = { 0.2, 0.2, 0.6, 0.5 }, focus = true,
      layout = { block = { flavor = "com.superlogical.terminal.shell", label = "Namedrop",
        options = { command = { "/bin/sh", "-c", "RD_SERVER=__RD_SERVER__ RD_STATE=__RD_STATE__ /usr/bin/python3 " .. tool .. " ls; sleep 30" } } } },
    })
    if res == nil then error(err, 0) end
    return res
  end,
}
return M
