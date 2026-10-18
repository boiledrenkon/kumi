local T = ...
local sha256 = T.load_payload_module("sha256")
local test, eq, ok = T.test, T.eq, T.ok

local function hex_bytes(h)
  return (h:gsub("%x%x", function(x) return string.char(tonumber(x, 16)) end))
end

test("sha256: FIPS 180-2 vectors", function()
  eq(sha256.hex(""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
  eq(sha256.hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
  eq(sha256.hex("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1")
  eq(sha256.hex(string.rep("a", 1000000)), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0")
end)

test("sha256: padding edges", function()
  -- 55, 56, 63, 64 and 65 bytes cross the one-block and two-block padding cases.
  local expected = {
    [55] = "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318",
    [56] = "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a",
    [63] = "7d3e74a05d7db15bce4ad9ec0658ea98e3f06eeecf16b4c6fff2da457ddc2f34",
    [64] = "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb",
    [65] = "635361c48bb9eab14198e76ea8ab7f1a41685d6ad62aa9146d301d4f17eb0ae0",
  }
  for n, digest in pairs(expected) do
    eq(sha256.hex(string.rep("a", n)), digest, n .. " bytes")
  end
end)

test("sha256: streaming equals one shot", function()
  local text = string.rep("0123456789abcdef", 1000) .. "tail"
  local h = sha256.new()
  local i = 1
  for _, size in ipairs({ 1, 63, 64, 65, 1000, 3, 50000 }) do
    h:update(text:sub(i, i + size - 1))
    i = i + size
  end
  h:update(text:sub(i))
  eq(sha256.tohex(h:digest()), sha256.hex(text))
end)

test("hmac-sha256: RFC 4231 vectors", function()
  local cases = {
    { string.rep("\11", 20), "Hi There", "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7" },
    { "Jefe", "what do ya want for nothing?", "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843" },
    { string.rep("\170", 20), string.rep("\221", 50), "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe" },
    { hex_bytes("0102030405060708090a0b0c0d0e0f10111213141516171819"), string.rep("\205", 50), "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b" },
    { string.rep("\170", 131), "Test Using Larger Than Block-Size Key - Hash Key First", "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54" },
    { string.rep("\170", 131), "This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.", "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2" },
  }
  for i, case in ipairs(cases) do
    eq(sha256.hmac_hex(case[1], case[2]), case[3], "case " .. i)
    eq(sha256.key(case[1]):mac_hex(case[2]), case[3], "case " .. i .. " with a reused key")
  end
  -- Case 5 truncates to 128 bits.
  eq(sha256.hmac_hex(string.rep("\12", 20), "Test With Truncation"):sub(1, 32), "a3b6167473100ee06e0c796c2955552b")
end)

test("hmac-sha256: a key reused for many messages", function()
  local key = sha256.key("k")
  eq(key:mac_hex("one"), sha256.hmac_hex("k", "one"))
  eq(key:mac_hex("two"), sha256.hmac_hex("k", "two"))
end)

test("sha256: equal compares in full", function()
  ok(sha256.equal("abc", "abc"))
  ok(not sha256.equal("abc", "abd"))
  ok(not sha256.equal("abc", "ab"))
  ok(not sha256.equal("abc", nil))
end)
