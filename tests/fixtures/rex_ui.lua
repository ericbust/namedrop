-- Managed by Namedrop. `namedrop install` rewrites this file, so edits here are lost.
-- A key bound later in init.lua wins over the same key below.
local NAMEDROP = "/fixture/runtime/namedrop"
local STATE = "/fixture/state"

local function endpoint()
	local status, err = rex.call("server.status")
	if status == nil then error("Namedrop could not ask Rex for its server: " .. tostring(err), 0) end
	for _, listener in ipairs(status.listeners or {}) do
		if listener.primary and listener.transport == "unix" then return "rex:" .. listener.addresses[1] end
	end
	error("Namedrop needs a Rex server that listens on a Unix socket", 0)
end

local actions = {
	{ "activate", "Activate Namedrop" },
	{ "name", "Name this pane", "alt+shift+cmd+i" },
	{ "manage", "Manage Namedrop names and agents", "alt+shift+cmd+m" },
	{ "doctor", "Namedrop Doctor", "alt+shift+cmd+h" },
	{ "status", "Agent status", "alt+shift+cmd+s" },
	{ "tour", "Namedrop tour", "alt+shift+cmd+o" },
	{ "channels", "Open Namedrop Channels" },
	{ "pause", "Pause Namedrop" },
	{ "resume", "Resume Namedrop" },
	{ "deactivate", "Deactivate Namedrop" },
}

for _, a in ipairs(actions) do
	local name, title, key = a[1], a[2], a[3]
	rex.action{ name = "namedrop_" .. name, title = title, category = "Namedrop", keywords = "namedrop mention agent",
		run = function(ctx)
			if ctx.session_id == nil or ctx.block_id == nil then error("Run this from a Rex pane", 0) end
			local argv = { NAMEDROP, "rex", "action", name, "--endpoint", endpoint(),
				"--space", ctx.session_id, "--pane", ctx.block_id }
			local result, err = rex.exec(argv, { env = { NAMEDROP_STATE_DIR = STATE }, timeout = 25 })
			if result == nil then error(tostring(err), 0) end
			if not result.ok then error(result.stderr, 0) end
		end }
	if key ~= nil and #rex.bindings{ key = key } == 0 then rex.bind(key, "namedrop_" .. name) end
end

rex.action{ name = "namedrop_about", title = "About Namedrop", category = "Namedrop",
	description = "Which Namedrop install this Rex configuration loaded",
	run = function()
		local env = {}
		for _, k in ipairs({ "HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR", "REX_SERVER" }) do
			env[k] = os.getenv(k)
		end
		return { module = debug.getinfo(1, "S").source, launcher = NAMEDROP, state = STATE, env = env }
	end }

rex.exec.start({ NAMEDROP, "rex", "ensure", "--from-server" },
	{ env = { NAMEDROP_STATE_DIR = STATE }, detach = true, stdout = false, stderr = false })
