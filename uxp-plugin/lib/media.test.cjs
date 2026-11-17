const assert = require("node:assert/strict");
const { test } = require("node:test");
const { mediaTypeOfFileName, isAudioFileName } = require("./media.js");

test("classifies file names and paths by extension", () => {
  for (const [name, expected] of [
    ["Interview_A.mp4", "video"],
    ["C:\Footage\A001.MOV", "video"],
    ["clip.braw", "video"],
    ["Logo_Lockup.png", "image"],
    ["/Volumes/Art/cover.JPEG", "image"],
    ["layers.psd", "image"],
    ["Theme_Song.wav", "audio"],
    ["vo.MP3", "audio"],
    ["Bars and Tone", null],
    ["Color Matte", null],
    ["", null],
    [null, null],
    ["Sequence 01.prproj backup", null]
  ]) {
    assert.equal(mediaTypeOfFileName(name), expected, String(name));
  }
  assert.equal(isAudioFileName("song.flac"), true);
  assert.equal(isAudioFileName("song.png"), false);
});
