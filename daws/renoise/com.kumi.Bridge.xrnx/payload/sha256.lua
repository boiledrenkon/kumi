-- SHA-256 and HMAC-SHA256 in pure Lua on LuaJIT's bit library.

local bit = require("bit")
local band, bor, bxor, bnot = bit.band, bit.bor, bit.bxor, bit.bnot
local rshift, lshift, ror, tobit, tohex = bit.rshift, bit.lshift, bit.ror, bit.tobit, bit.tohex
local byte, char, rep, sub = string.byte, string.char, string.rep, string.sub
local concat, floor = table.concat, math.floor

local K = {
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
}
for i = 1, 64 do K[i] = tobit(K[i]) end

local INITIAL = { 0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19 }
for i = 1, 8 do INITIAL[i] = tobit(INITIAL[i]) end

local W = {}

-- Processes the 64-byte block of `s` that starts after `offset`.
local function compress(H, s, offset)
  for i = 1, 16 do
    local p = offset + i * 4
    local a, b, c, d = byte(s, p - 3, p)
    W[i] = bor(lshift(a, 24), lshift(b, 16), lshift(c, 8), d)
  end
  for i = 17, 64 do
    local w15, w2 = W[i - 15], W[i - 2]
    local s0 = bxor(ror(w15, 7), ror(w15, 18), rshift(w15, 3))
    local s1 = bxor(ror(w2, 17), ror(w2, 19), rshift(w2, 10))
    W[i] = tobit(W[i - 16] + s0 + W[i - 7] + s1)
  end
  local a, b, c, d, e, f, g, h = H[1], H[2], H[3], H[4], H[5], H[6], H[7], H[8]
  for i = 1, 64 do
    local t1 = h + bxor(ror(e, 6), ror(e, 11), ror(e, 25)) + bxor(band(e, f), band(bnot(e), g)) + K[i] + W[i]
    local t2 = bxor(ror(a, 2), ror(a, 13), ror(a, 22)) + bxor(band(a, b), band(a, c), band(b, c))
    h = g; g = f; f = e; e = tobit(d + t1)
    d = c; c = b; b = a; a = tobit(t1 + t2)
  end
  H[1] = tobit(H[1] + a); H[2] = tobit(H[2] + b); H[3] = tobit(H[3] + c); H[4] = tobit(H[4] + d)
  H[5] = tobit(H[5] + e); H[6] = tobit(H[6] + f); H[7] = tobit(H[7] + g); H[8] = tobit(H[8] + h)
end

local Hasher = {}
Hasher.__index = Hasher

local function new_hasher(state)
  local H = {}
  for i = 1, 8 do H[i] = state[i] end
  return setmetatable({ H = H, pending = "", length = 0 }, Hasher)
end

function Hasher:update(s)
  self.length = self.length + #s
  if self.pending ~= "" then
    s = self.pending .. s
  end
  local blocks = floor(#s / 64)
  local H = self.H
  for i = 0, blocks - 1 do
    compress(H, s, i * 64)
  end
  self.pending = sub(s, blocks * 64 + 1)
  return self
end

local function words_to_bytes(H)
  local out = {}
  for i = 1, 8 do
    local v = H[i]
    out[i] = char(band(rshift(v, 24), 255), band(rshift(v, 16), 255), band(rshift(v, 8), 255), band(v, 255))
  end
  return concat(out)
end

-- The 32-byte digest; the hasher can't be updated afterwards.
function Hasher:digest()
  local bits = self.length * 8
  local tail = self.pending .. "\128" .. rep("\0", (55 - #self.pending) % 64)
  local high, low = floor(bits / 4294967296), bits % 4294967296
  tail = tail .. char(
    band(rshift(high, 24), 255), band(rshift(high, 16), 255), band(rshift(high, 8), 255), band(high, 255),
    band(rshift(low, 24), 255), band(rshift(low, 16), 255), band(rshift(low, 8), 255), band(low, 255))
  local H = self.H
  for i = 0, #tail / 64 - 1 do
    compress(H, tail, i * 64)
  end
  self.pending = nil
  return words_to_bytes(H)
end

local M = {}

function M.new()
  return new_hasher(INITIAL)
end

function M.digest(s)
  return new_hasher(INITIAL):update(s):digest()
end

function M.tohex(s)
  local out = {}
  for i = 1, #s do out[i] = tohex(byte(s, i), 2) end
  return concat(out)
end

function M.hex(s)
  return M.tohex(M.digest(s))
end

-- An HMAC key whose padded inner and outer blocks are hashed once, for many messages.
local Key = {}
Key.__index = Key

function M.key(secret)
  if #secret > 64 then secret = M.digest(secret) end
  secret = secret .. rep("\0", 64 - #secret)
  local ipad, opad = {}, {}
  for i = 1, 64 do
    local b = byte(secret, i)
    ipad[i] = char(bxor(b, 0x36))
    opad[i] = char(bxor(b, 0x5c))
  end
  local inner, outer = new_hasher(INITIAL), new_hasher(INITIAL)
  compress(inner.H, concat(ipad), 0)
  compress(outer.H, concat(opad), 0)
  return setmetatable({ inner = inner.H, outer = outer.H }, Key)
end

-- A hasher already fed the inner pad; finish it with Key:finish.
function Key:start()
  local h = new_hasher(self.inner)
  h.length = 64
  return h
end

function Key:finish(inner_hasher)
  local h = new_hasher(self.outer)
  h.length = 64
  return h:update(inner_hasher:digest()):digest()
end

function Key:mac(message)
  return self:finish(self:start():update(message))
end

function Key:mac_hex(message)
  return M.tohex(self:mac(message))
end

function M.hmac(secret, message)
  return M.key(secret):mac(message)
end

function M.hmac_hex(secret, message)
  return M.tohex(M.hmac(secret, message))
end

-- Compares two strings in time that depends only on their lengths.
function M.equal(a, b)
  if type(a) ~= "string" or type(b) ~= "string" or #a ~= #b then return false end
  local diff = 0
  for i = 1, #a do
    diff = bor(diff, bxor(byte(a, i), byte(b, i)))
  end
  return diff == 0
end

return M
