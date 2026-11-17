"use strict";

// File extensions Spotlight treats as audio-only sources when Premiere can't
// say whether a clip has video. Keep in sync with AUDIO_FILE_EXTENSIONS in
// helper-app/src/audio_preview.rs (a Rust test compares the two lists).
const AUDIO_FILE_EXTENSIONS = [
  "wav", "wave", "bwf", "aif", "aiff", "aifc", "flac", "mp3", "aac", "m4a", "alac"
];

// Still images and graphics, which get their own icon in Spotlight.
const IMAGE_FILE_EXTENSIONS = [
  "png", "jpg", "jpeg", "jpe", "jfif", "gif", "tif", "tiff", "bmp", "psd", "ai", "eps",
  "webp", "heic", "heif", "exr", "dpx", "tga", "svg", "ico", "avif"
];

function extensionPattern(extensions) {
  return new RegExp(`\\.(?:${extensions.join("|")})$`, "i");
}

const AUDIO_FILE_PATTERN = extensionPattern(AUDIO_FILE_EXTENSIONS);
const IMAGE_FILE_PATTERN = extensionPattern(IMAGE_FILE_EXTENSIONS);
const HAS_EXTENSION = /\.[a-z0-9]{1,5}$/i;

function isAudioFileName(name) {
  return AUDIO_FILE_PATTERN.test(name || "");
}

// "audio", "image" or "video" from a file name or path, or null without an extension.
// Any other extension is a movie format, since Premiere imports little else as media.
function mediaTypeOfFileName(name) {
  const text = String(name || "").trim();
  if (AUDIO_FILE_PATTERN.test(text)) return "audio";
  if (IMAGE_FILE_PATTERN.test(text)) return "image";
  return HAS_EXTENSION.test(text) ? "video" : null;
}

module.exports = { AUDIO_FILE_EXTENSIONS, IMAGE_FILE_EXTENSIONS, isAudioFileName, mediaTypeOfFileName };
