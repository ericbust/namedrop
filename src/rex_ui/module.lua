-- Managed by Namedrop. `namedrop install` rewrites this file, so edits here are lost.
-- A key bound later in init.lua wins over the same key below.
local NAMEDROP = __LAUNCHER__
local STATE = __STATE__

local function endpoint()
	local status, err = rex.call("server.status")
	if status == nil then error("Namedrop could not ask Rex for its server: " .. tostring(err), 0) end
	for _, listener in ipairs(status.listeners or {}) do
		if listener.primary and listener.transport == "unix" then return "rex:" .. listener.addresses[1] end
	end
	error("Namedrop needs a Rex server that listens on a Unix socket", 0)
end

__ACTIONS__

for _, a in ipairs(actions) do
	local name, title, key = a[1], a[2], a[3]
	rex.action{ name = "namedrop_" .. name, title = title, category = "Namedrop", keywords = "namedrop mention agent",
		run = function(ctx)
			if ctx.session_id == nil or ctx.block_id == nil then error("Run this from a Rex pane", 0) end
			local argv = { NAMEDROP, "rex", "action", name, "--endpoint", endpoint(),
				"--space", ctx.session_id, "--pane", ctx.block_id }
			local result, err = rex.exec(argv, { env = { NAMEDROP_STATE_DIR = STATE }, timeout = 25 })
			if result == nil then
				if tostring(err):find("no such file or directory", 1, true) then
					error("Namedrop's launcher " .. NAMEDROP .. " is missing. Run sh scripts/install.sh again.", 0)
				end
				error("Namedrop's launcher " .. NAMEDROP .. " could not start: " .. tostring(err) .. ". Run sh scripts/install.sh again.", 0)
			end
			if result.ok then return end
			if result.timed_out then
				error(title .. " did not finish within 25 s, so Rex stopped it. If Namedrop is updating, try again when the update is done.", 0)
			end
			if result.stderr ~= nil and result.stderr ~= "" then error(result.stderr, 0) end
			if result.signal ~= nil then error(title .. " stopped on " .. tostring(result.signal) .. " before it finished.", 0) end
			error(title .. " exited with status " .. tostring(result.code) .. " and printed no reason.", 0)
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
