local T = ...
local net = T.load_payload_module("net")
local test, eq, ok = T.test, T.eq, T.ok

local function feed_all(framer, chunks)
  local lines = {}
  for _, chunk in ipairs(chunks) do
    local got, err = net.feed(framer, chunk)
    for _, line in ipairs(got) do lines[#lines + 1] = line end
    if err then return lines, err end
  end
  return lines
end

test("framing: lines split across chunks, several per chunk", function()
  local lines = feed_all(net.framer(100), { '{"a"', ':1}\n{"b":2}\n{', '"c":3', "}\n" })
  eq(#lines, 3)
  eq(lines[1], '{"a":1}')
  eq(lines[2], '{"b":2}')
  eq(lines[3], '{"c":3}')
end)

test("framing: CRLF and a leading BOM are not part of a record", function()
  local lines = feed_all(net.framer(100), { "\239\187\191{}\r\n", "x\r", "\n" })
  eq(lines[1], "{}")
  eq(lines[2], "x")
end)

test("framing: one byte at a time", function()
  local framer = net.framer(100)
  local text = '{"k":"v"}\n[1]\n'
  local lines = {}
  for i = 1, #text do
    for _, line in ipairs((net.feed(framer, text:sub(i, i)))) do lines[#lines + 1] = line end
  end
  eq(#lines, 2)
  eq(lines[2], "[1]")
end)

test("framing: a line past the limit is refused, with or without its newline", function()
  local _, err = feed_all(net.framer(10), { string.rep("x", 11) })
  eq(err, "oversized")
  _, err = feed_all(net.framer(10), { string.rep("x", 6), string.rep("x", 6) })
  eq(err, "oversized")
  _, err = feed_all(net.framer(10), { string.rep("x", 11) .. "\n" })
  eq(err, "oversized")
  local lines
  lines, err = feed_all(net.framer(10), { string.rep("x", 10) .. "\n" })
  eq(err, nil)
  eq(#lines[1], 10)
end)

test("nonces: 32 hex characters, never repeated", function()
  local seen = {}
  for _ = 1, 1000 do
    local n = net.nonce("secret")
    ok(n:match("^%x+$") and #n == 32)
    ok(not seen[n], "repeated nonce")
    seen[n] = true
  end
end)

test("proofs: client and server proofs differ and bind both nonces", function()
  local s = string.rep("k", 32)
  local a, b = string.rep("a", 32), string.rep("b", 32)
  ok(net.client_proof(s, a, b) ~= net.server_proof(s, b, a))
  ok(net.client_proof(s, a, b) ~= net.client_proof(s, b, a))
  eq(#net.client_proof(s, a, b), 64)
end)
