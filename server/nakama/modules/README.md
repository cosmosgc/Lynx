# Nakama runtime modules

Nakama scans this directory for Lua, JavaScript, and Go plugin modules at
startup. Lynx currently keeps authentication, social, and chat in Nakama's
built-in APIs, so the directory is intentionally empty.

Add a module here when a feature needs Nakama's runtime (matchmaking hooks,
server-to-server RPCs, or chat channels). Register the file in
[`local.yml`](../local.yml) only if the runtime path changes.
