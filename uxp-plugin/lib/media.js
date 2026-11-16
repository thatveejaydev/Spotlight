"use strict";

// File extensions Spotlight treats as audio-only sources when Premiere can't
// say whether a clip has video. Keep in sync with AUDIO_FILE_EXTENSIONS in
// helper-app/src/audio_preview.rs (a Rust test compares the two lists).
const AUDIO_FILE_EXTENSIONS = [
  "wav", "wave", "bwf", "aif", "aiff", "aifc", "flac", "mp3", "aac", "m4a", "alac"
];

const AUDIO_FILE_PATTERN = new RegExp(`\\.(?:${AUDIO_FILE_EXTENSIONS.join("|")})$`, "i");

function isAudioFileName(name) {
  return AUDIO_FILE_PATTERN.test(name || "");
}

module.exports = { AUDIO_FILE_EXTENSIONS, isAudioFileName };
