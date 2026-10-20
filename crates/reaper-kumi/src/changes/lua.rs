//! A Lua chunk run in REAPER as a ReaScript, as a change: the chunk runs once, on the main thread, inside one undo block
//! named by the host, so REAPER's undo takes back whatever it did. What it did is REAPER's to know, not Kumi's, so Kumi's
//! own undo is refused (`daw-only`).
//!
//! The chunk is wrapped (see `wrapper`): it is compiled first, run with a time limit where REAPER's Lua has a debug hook,
//! `print` and the console are kept, and what it returns comes back as JSON (a track, item or take as its ref). The
//! wrapper leaves its answer for the extension in an ext state that it reads and deletes.

use serde_json::{json, Value};

use super::{Applied, Ctx, Done, Preview, Restore, Restored};
use crate::read::{bad, failed};

const NOTE: &str = "Only REAPER's own undo can take back what it does (Edit > Undo).";
/// Where the wrapper leaves its answer.
const SECTION: &str = "kumi_run";
/// How long a chunk may run when the request names no limit, and the most it may name: REAPER's main thread waits.
const TIMEOUT_MS: u64 = 2_000;
const MOST_TIMEOUT_MS: u64 = 10_000;
/// The most of a chunk's answer kept (its result and what it printed).
const MOST_ANSWER: usize = 4 * 1024 * 1024;

fn code_of(input: &Value) -> Done<&str> {
    input
        .get("code")
        .and_then(Value::as_str)
        .filter(|code| !code.trim().is_empty())
        .ok_or_else(|| bad("code: the Lua to run (ReaScript: the reaper table and Lua's own libraries)"))
}

fn timeout_of(input: &Value) -> Done<u64> {
    match input.get("timeoutMs") {
        None | Some(Value::Null) => Ok(TIMEOUT_MS),
        Some(v) => v
            .as_u64()
            .filter(|ms| (1..=MOST_TIMEOUT_MS).contains(ms))
            .ok_or_else(|| bad(format!("timeoutMs must be a whole number of milliseconds from 1 to {MOST_TIMEOUT_MS}"))),
    }
}

/// "Ran Lua: " and the chunk's first line that says something.
fn title(code: &str) -> String {
    let line = code.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("--")).unwrap_or_else(|| code.trim());
    let clipped: String = line.chars().take(60).collect();
    let more = if line.chars().count() > 60 { "…" } else { "" };
    format!("Ran Lua: {clipped}{more}")
}

/// A long bracket that the code doesn't close: `[==[` … `]==]` with enough `=`.
fn long_string(code: &str) -> String {
    let mut level = 0;
    while code.contains(&format!("]{}]", "=".repeat(level))) {
        level += 1;
    }
    let eq = "=".repeat(level);
    format!("[{eq}[\n{code}]{eq}]")
}

/// The ReaScript that runs `code` (or only compiles it, `check`) and leaves its answer in the ext state `token`.
pub fn wrapper(code: &str, token: &str, timeout_ms: u64, check: bool) -> String {
    format!(
        r##"-- Kumi: a Lua chunk run once, then taken away.
local TOKEN, LIMIT_MS, CHECK = "{token}", {timeout_ms}, {check}
local code = {code}
local function quote(s)
  return '"' .. s:gsub('[%c"\\]', function(c)
    if c == '"' then return '\\"' elseif c == '\\' then return '\\\\' elseif c == '\n' then return '\\n'
    elseif c == '\r' then return '\\r' elseif c == '\t' then return '\\t' end
    return string.format('\\u%04x', c:byte())
  end) .. '"'
end
local function ref_of(v)
  local ok, kind = pcall(function()
    if reaper.ValidatePtr2(0, v, "MediaTrack*") then
      if v == reaper.GetMasterTrack(0) then return "r:track:master" end
      return "r:track:" .. reaper.GetTrackGUID(v)
    elseif reaper.ValidatePtr2(0, v, "MediaItem*") then
      local _, guid = reaper.GetSetMediaItemInfo_String(v, "GUID", "", false)
      return "r:item:" .. guid
    elseif reaper.ValidatePtr2(0, v, "MediaItem_Take*") then
      local _, guid = reaper.GetSetMediaItemTakeInfo_String(v, "GUID", "", false)
      return "r:take:" .. guid
    end
  end)
  if ok and kind then return kind end
  return tostring(v)
end
local function json(v, depth)
  depth = depth or 0
  local t = type(v)
  if v == nil then return "null"
  elseif t == "boolean" then return tostring(v)
  elseif t == "number" then
    if v ~= v or v == math.huge or v == -math.huge then return "null" end
    if v == math.floor(v) and math.abs(v) < 2^53 then return string.format("%d", v) end
    return string.format("%.17g", v)
  elseif t == "string" then return quote(v)
  elseif t == "userdata" then return quote(ref_of(v))
  elseif t == "table" then
    if depth > 32 then return quote("(nested too deep)") end
    local n, count = #v, 0
    for _ in pairs(v) do count = count + 1 end
    local out = {{}}
    if count > 0 and count == n then
      for i = 1, n do out[i] = json(v[i], depth + 1) end
      return "[" .. table.concat(out, ",") .. "]"
    end
    if count == 0 then return "[]" end
    for k, value in pairs(v) do out[#out + 1] = quote(tostring(k)) .. ":" .. json(value, depth + 1) end
    return "{{" .. table.concat(out, ",") .. "}}"
  end
  return quote(tostring(v))
end
local function where(message)
  message = tostring(message)
  local line, rest = message:match('^chunk:(%d+): (.*)$')
  if line then return '{{"message":' .. quote(rest) .. ',"line":' .. line .. '}}' end
  return '{{"message":' .. quote(message) .. '}}'
end
local function answer(text) reaper.SetExtState("{section}", TOKEN, text, false) end
local chunk, err = load(code, "=chunk", "t")
if not chunk then answer('{{"compiled":false,"error":' .. where(err) .. '}}') return end
if CHECK then answer('{{"compiled":true}}') return end
local printed = {{}}
local function keep(...)
  local parts = {{}}
  for i = 1, select("#", ...) do parts[#parts + 1] = tostring((select(i, ...))) end
  printed[#printed + 1] = table.concat(parts, "\t")
end
print = keep
reaper.ShowConsoleMsg = function(text) printed[#printed + 1] = tostring(text) end
local started = reaper.time_precise()
if debug and debug.sethook then
  debug.sethook(function()
    if (reaper.time_precise() - started) * 1000 > LIMIT_MS then
      debug.sethook()
      error("stopped after " .. LIMIT_MS .. " ms (the chunk's time limit)", 0)
    end
  end, "", 10000)
end
local results = {{ n = 0 }}
local ok, failure = xpcall(function()
  local function pack(...) return {{ n = select("#", ...), ... }} end
  results = pack(chunk())
end, function(e) return e end)
if debug and debug.sethook then debug.sethook() end
local lines = {{}}
for i, line in ipairs(printed) do lines[i] = quote(line) end
local result = "null"
if results.n == 1 then result = json(results[1]) elseif results.n > 1 then
  local list = {{}}
  for i = 1, results.n do list[i] = json(results[i]) end
  result = "[" .. table.concat(list, ",") .. "]"
end
if ok then
  answer('{{"compiled":true,"ok":true,"result":' .. result .. ',"printed":[' .. table.concat(lines, ",") .. ']}}')
else
  answer('{{"compiled":true,"ok":false,"error":' .. where(failure) .. ',"printed":[' .. table.concat(lines, ",") .. ']}}')
end
"##,
        code = long_string(code),
        section = SECTION,
    )
}

static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn token() -> String {
    let n = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("run{}-{n}", std::process::id())
}

/// Runs the wrapper and reads what it left: None when it left nothing (REAPER didn't run it, or it was cut short).
fn run(ctx: &Ctx, code: &str, timeout_ms: u64, check: bool) -> Done<Option<Value>> {
    let token = token();
    ctx.reaper.run_script(ctx.proj, &wrapper(code, &token, timeout_ms, check)).map_err(failed)?;
    let Some(text) = ctx.reaper.take_ext_state(SECTION, &token) else { return Ok(None) };
    if text.len() > MOST_ANSWER {
        return Ok(Some(
            json!({"compiled": true, "ok": true, "result": format!("(an answer of {} MB, more than Kumi keeps)", text.len() >> 20), "printed": []}),
        ));
    }
    serde_json::from_str(&text).map(Some).map_err(|e| failed(format!("the chunk's answer couldn't be read: {e}")))
}

fn compile_error(error: &Value) -> String {
    match error["line"].as_i64() {
        Some(line) => format!("the chunk doesn't compile: line {line}: {}", error["message"].as_str().unwrap_or("")),
        None => format!("the chunk doesn't compile: {}", error["message"].as_str().unwrap_or("")),
    }
}

pub fn preview(ctx: &Ctx, input: &Value) -> Done<Preview> {
    let code = code_of(input)?;
    timeout_of(input)?;
    let checked = run(ctx, code, MOST_TIMEOUT_MS, true)?.ok_or_else(|| failed("REAPER didn't run the check of the chunk"))?;
    if checked["compiled"] != true {
        return Err(bad(compile_error(&checked["error"])));
    }
    Ok(Preview {
        target: json!({"kind": "lua"}),
        prior: json!({}),
        fence: json!({"chars": code.chars().count()}),
        display: json!({"title": title(code), "note": NOTE}),
    })
}

pub fn apply(ctx: &Ctx, input: &Value, fence: &Value) -> Done<Applied> {
    let code = code_of(input)?;
    let timeout_ms = timeout_of(input)?;
    if fence["chars"] != code.chars().count() {
        return Err(bad("the chunk isn't the one previewed"));
    }
    let ran = run(ctx, code, timeout_ms, false)?;
    let Some(ran) = ran else {
        // (REAPER was asked to run it and nothing came back: whether it ran, and what it did, can't be told)
        return Err((
            "uncertain".into(),
            "REAPER was asked to run the chunk and it gave no answer: whatever it did, REAPER's undo takes back".into(),
        ));
    };
    if ran["compiled"] != true {
        return Err(bad(compile_error(&ran["error"])));
    }
    let printed = ran.get("printed").cloned().unwrap_or(json!([]));
    let mut display = json!({"title": title(code), "note": NOTE});
    // (REAPER counts a block's changes only as it ends, so whether the chunk changed anything isn't known here: the block
    // is always closed as a write, REAPER's undo covers whatever it did, and the answer's undo.entry says whether REAPER
    // made a point of it)
    let applied = if ran["ok"] == true {
        json!({"result": ran["result"], "printed": printed})
    } else {
        let error = &ran["error"];
        let at = error["line"].as_i64().map(|line| format!(" at line {line}")).unwrap_or_default();
        let stays = " Anything it changed before the error stays: REAPER's undo takes it back.";
        display["note"] = format!("It stopped with an error{at}: {}.{stays}", error["message"].as_str().unwrap_or("")).into();
        json!({"failed": error, "printed": printed})
    };
    Ok(Applied { applied, display, ..Default::default() })
}

pub fn restore(_ctx: &Ctx, _restore: &Restore) -> Done<Restored> {
    Err(("daw-only".into(), "Kumi can't take back what a Lua chunk did: REAPER's own undo can (Edit > Undo)".into()))
}
