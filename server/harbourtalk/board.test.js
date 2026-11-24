import { test } from "node:test";
import assert from "node:assert/strict";
import { MAX_TEXT, cleanName, parseRooms, cleanText, isDevice, keyOk, limitOf, originOk, wait } from "./board.js";

test("texts and names are cleaned", () => {
  assert.equal(cleanText(" a\nb\t "), "a b");
  assert.equal(Array.from(cleanText("あ".repeat(300))).length, MAX_TEXT);
  assert.equal(cleanText(42), "");
  assert.equal(cleanName("  kuro#x\n "), "kurox");
  assert.equal(Array.from(cleanName("😀".repeat(20))).length, 16);
});

test("devices, keys, origins, limits", () => {
  assert.ok(isDevice("3fa29c01"));
  assert.ok(!isDevice("3FA29C01") && !isDevice("3fa2") && !isDevice(undefined));
  assert.ok(keyOk("harbourtalk-1", "old, harbourtalk-1"));
  assert.ok(!keyOk("x", "harbourtalk-1") && !keyOk("", "") && !keyOk("a", undefined));
  assert.ok(originOk(null, ""), "curl sends no Origin");
  assert.ok(originOk("https://demo.example", "https://demo.example"));
  assert.ok(!originOk("https://evil.example", "https://demo.example"));
  assert.equal(limitOf("500"), 100);
  assert.equal(limitOf("0"), 1);
  assert.equal(limitOf(null), 50);
});

test("rooms come from the ROOMS setting", () => {
  assert.deepEqual(parseRooms("lobby=ロビー, music=Music=1,Bad=x,lobby=again,empty="), [
    { id: "lobby", name: "ロビー" },
    { id: "music", name: "Music=1" },
  ]);
  assert.deepEqual(parseRooms(undefined), [{ id: "lobby", name: "Lobby" }]);
});

test("one post every 30 seconds", () => {
  assert.equal(wait(0, 100), 0);
  assert.equal(wait(1000, 1000), 30);
  assert.equal(wait(1000, 1012), 18);
  assert.equal(wait(1000, 1030), 0);
  assert.equal(wait(1000, 1004, 10), 6, "the address gap is shorter");
});
