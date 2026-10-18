-- Measures what the bridge's HMAC costs on LuaJIT: per connection and per frame.
-- Usage: luajit bench.lua (add -joff to measure the interpreter alone)

local tests_dir = arg[0]:match("^(.*[/\\])") or "./"
local loaded = {}
local function import(name)
  if loaded[name] == nil then
    loaded[name] = assert(loadfile(tests_dir .. "../com.kumi.Bridge.xrnx/payload/" .. name .. ".lua"))(import)
  end
  return loaded[name]
end
local sha256, net = import("sha256"), import("net")

local function time(fn, min_seconds)
  local runs, start = 0, os.clock()
  repeat
    fn()
    runs = runs + 1
  until os.clock() - start >= (min_seconds or 0.5)
  return (os.clock() - start) / runs
end

local secret = string.rep("k", 43)
local client_nonce, server_nonce = string.rep("ab", 16), string.rep("cd", 16)
print(string.format("jit: %s", jit and jit.status() and "on" or "off"))
local handshake = time(function()
  local n = net.nonce(secret)
  local proof = net.client_proof(secret, server_nonce, client_nonce)
  assert(sha256.equal(proof, proof) and n)
  net.server_proof(secret, client_nonce, server_nonce)
end)
print(string.format("per connection (nonce + both proofs): %.3f ms", handshake * 1000))

local key = sha256.key(secret)
for _, size in ipairs({ 256, 4096, 65536, 1048576, 16777216 }) do
  local frame = string.rep("x", size)
  local t = time(function() key:mac(frame) end, size > 1e6 and 2 or 0.5)
  print(string.format("per frame, %8d bytes: %9.3f ms  (%.1f MB/s)", size, t * 1000, size / t / 1e6))
end
