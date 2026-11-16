"use strict";

// Scans Premiere's effects and transitions and applies them to the timeline selection.

const { nowMs } = require("./util.js");

const HIDDEN_VIDEO_MATCH_NAMES = new Set([
  "AE.ADBE Motion",
  "AE.ADBE Opacity",
  "AE.ADBE Time Remapping"
]);

module.exports = function createEffects(ppro) {
  const {
    TICKS_PER_SECOND,
    DEFAULT_TICKS_PER_FRAME_30FPS,
    isVideoClip,
    isAudioClip
  } = require("./timeline.js")(ppro);

  function formatTransitionDisplayName(rawMatchName) {
    let name = String(rawMatchName || "").trim();
    // 1. Strip repeated engine prefixes: "AE.", "PR.", "ADBE ", "AE_"
    name = name.replace(/^(?:AE\.|PR\.|ADBE[\s_]+|AE[\s_]+)+/gi, "");
    // 2. Replace underscores with spaces (e.g. "Impact_Dip_To_Black" -> "Impact Dip To Black")
    name = name.replace(/_/g, " ");
    // 3. Strip internal " New" suffix used by Premiere (e.g. "Cross Dissolve New" -> "Cross Dissolve")
    name = name.replace(/\s+New$/i, "");
    // 4. Insert space in CamelCase words (e.g. "MorphCut" -> "Morph Cut", "CheckerBoard" -> "Checker Board")
    name = name.replace(/([a-z])([A-Z])/g, "$1 $2");
    // 5. Collapse multiple spaces
    name = name.replace(/\s{2,}/g, " ").trim();
    return name || String(rawMatchName || "").trim();
  }

  // Returns the deduplicated effect list and how long the scan took.
  async function scanEffectCatalog() {
    const startedAt = nowMs();
    const [videoItems, audioItems, transitionItems] = await Promise.all([
      (async () => {
        const list = [];
        try {
          if (ppro.VideoFilterFactory) {
            const [matchNames, displayNames] = await Promise.all([
              Promise.resolve(ppro.VideoFilterFactory.getMatchNames?.() || []),
              Promise.resolve(ppro.VideoFilterFactory.getDisplayNames?.() || [])
            ]);
            const count = Math.min(matchNames.length, displayNames.length);
            for (let i = 0; i < count; i++) {
              const name = String(displayNames[i] || "").trim();
              const matchName = String(matchNames[i] || "").trim();
              if (!name || !matchName || HIDDEN_VIDEO_MATCH_NAMES.has(matchName)) continue;
              list.push({
                name,
                matchName,
                effectType: "videoEffect",
                folder: "Video Effect"
              });
            }
          }
        } catch (err) {
          console.warn("[Spotlight] VideoFilterFactory scan error:", err);
        }
        return list;
      })(),
      (async () => {
        const list = [];
        try {
          if (ppro.AudioFilterFactory) {
            const displayNames = (await Promise.resolve(ppro.AudioFilterFactory.getDisplayNames?.())) || [];
            for (const rawName of displayNames) {
              const name = String(rawName || "").trim();
              if (!name) continue;
              list.push({
                name,
                matchName: name,
                effectType: "audioEffect",
                folder: "Audio Effect"
              });
            }
          }
        } catch (err) {
          console.warn("[Spotlight] AudioFilterFactory scan error:", err);
        }
        return list;
      })(),
      (async () => {
        const list = [];
        try {
          if (ppro.TransitionFactory) {
            const vTrans = (await Promise.resolve(ppro.TransitionFactory.getVideoTransitionMatchNames?.())) || [];
            for (const m of vTrans) {
              const clean = String(m || "").trim();
              if (!clean) continue;
              list.push({
                name: formatTransitionDisplayName(clean),
                matchName: clean,
                effectType: "videoTransition",
                folder: "Video Transition"
              });
            }
          }
        } catch (err) {
          console.warn("[Spotlight] TransitionFactory scan error:", err);
        }
        return list;
      })()
    ]);

    const items = [];
    const seen = new Set();
    for (const entry of [...videoItems, ...audioItems, ...transitionItems]) {
      const key = `${entry.effectType}:${entry.matchName}`;
      if (seen.has(key)) continue;
      seen.add(key);
      items.push(entry);
    }

    const elapsedMs = Math.max(0, Math.round(nowMs() - startedAt));
    return { items, elapsedMs };
  }

  // `catalog` is the last scanned effect list.
  function resolveEffectEntry(payload, catalog) {
    const reqName = String(payload.name || "").trim();
    const reqMatch = String(payload.matchName || "").trim();
    const reqType = String(payload.effectType || "videoEffect").trim();

    if (catalog.length > 0) {
      const exactMatch = catalog.find(
        (item) => item.effectType === reqType && (item.matchName === reqMatch || item.name === reqName)
      );
      if (exactMatch) return exactMatch;

      const lowerName = reqName.toLowerCase();
      const lowerMatch = reqMatch.toLowerCase();
      const ciMatch = catalog.find(
        (item) =>
          item.effectType === reqType &&
          (item.name.toLowerCase() === lowerName || item.matchName.toLowerCase() === lowerMatch)
      );
      if (ciMatch) return ciMatch;
    }

    return {
      name: reqName || reqMatch,
      matchName: reqMatch || reqName,
      effectType: reqType
    };
  }

  function createTickTimeFromTicks(ticksBigInt) {
    const safeTicks = ticksBigInt > 0n ? ticksBigInt : DEFAULT_TICKS_PER_FRAME_30FPS;
    try {
      if (typeof ppro.TickTime?.createWithTicks === "function") {
        return ppro.TickTime.createWithTicks(String(safeTicks));
      }
    } catch (_) {}
    try {
      const seconds = Number(safeTicks) / Number(TICKS_PER_SECOND);
      if (typeof ppro.TickTime?.createWithSeconds === "function") {
        return ppro.TickTime.createWithSeconds(seconds);
      }
      if (typeof ppro.TickTime?.fromSeconds === "function") {
        return ppro.TickTime.fromSeconds(seconds);
      }
    } catch (_) {}
    return undefined;
  }

  async function getSequenceTicksPerFrame(sequence) {
    // 1. Check sequence.getTimebase() (in Premiere, timebase is ticks per frame as a string/number)
    try {
      const tb = await Promise.resolve(sequence.getTimebase?.());
      if (tb !== undefined && tb !== null) {
        const num = Number(tb);
        if (Number.isFinite(num) && num > 0) {
          if (num > 100000) {
            return BigInt(Math.round(num));
          }
          // If returned as FPS (e.g., 23.976, 24, 29.97, 30, 60)
          return BigInt(Math.round(Number(TICKS_PER_SECOND) / num));
        }
      }
    } catch (_) {}

    // 2. Check sequence.getSettings() -> getVideoFrameRate()
    try {
      const settings = await Promise.resolve(sequence.getSettings?.());
      const fr = await Promise.resolve(settings?.getVideoFrameRate?.());
      if (fr?.ticks !== undefined) {
        const t = BigInt(fr.ticks);
        if (t > 0n) return t;
      }
      if (fr?.seconds !== undefined && Number(fr.seconds) > 0) {
        return BigInt(Math.round(Number(fr.seconds) * Number(TICKS_PER_SECOND)));
      }
    } catch (_) {}

    return DEFAULT_TICKS_PER_FRAME_30FPS;
  }

  async function computeTransitionDurationInfo(sequence, prefDuration, fallbackFrames) {
    const ticksPerFrame = await getSequenceTicksPerFrame(sequence);
    let durationTicks = BigInt(fallbackFrames) * ticksPerFrame;
    let label = `${fallbackFrames}f`;

    if (prefDuration && typeof prefDuration.value === "number" && prefDuration.value > 0) {
      const val = prefDuration.value;
      const unit = String(prefDuration.unit || "frames").toLowerCase();
      if (unit === "seconds") {
        durationTicks = BigInt(Math.round(val * Number(TICKS_PER_SECOND)));
        label = `${val}s`;
      } else {
        const frames = Math.max(1, Math.round(val));
        durationTicks = BigInt(frames) * ticksPerFrame;
        label = `${frames}f`;
      }
    }

    return {
      durationTicks,
      durationTickTime: createTickTimeFromTicks(durationTicks),
      ticksPerFrame,
      label
    };
  }

  function createTransitionOptions({
    applyToStart,
    forceSingleSided = false,
    alignment = 0.5,
    durationTickTime
  }) {
    if (typeof ppro.AddTransitionOptions !== "function") {
      return undefined;
    }
    let opts;
    try {
      opts = new ppro.AddTransitionOptions();
    } catch (_) {
      opts = ppro.AddTransitionOptions();
    }
    if (!opts) return undefined;
    try {
      opts.setApplyToStart?.(Boolean(applyToStart));
    } catch (_) {}
    try {
      opts.setForceSingleSided?.(Boolean(forceSingleSided));
    } catch (_) {}
    if (typeof alignment === "number") {
      try {
        opts.setTransitionAlignment?.(alignment);
      } catch (_) {}
    }
    if (durationTickTime) {
      try {
        opts.setDuration?.(durationTickTime);
      } catch (_) {}
    }
    return opts;
  }

  async function planTrackTransitionTargets(sequence, trackClipsList) {
    const metas = await Promise.all(
      trackClipsList.map(async (clip) => {
        const [startTime, endTime, inPoint, outPoint, trackIndex] = await Promise.all([
          Promise.resolve(clip.getStartTime?.()),
          Promise.resolve(clip.getEndTime?.()),
          Promise.resolve(clip.getInPoint?.()),
          Promise.resolve(clip.getOutPoint?.()),
          Promise.resolve(clip.getTrackIndex?.())
        ]);
        const startTicks = startTime?.ticks !== undefined ? BigInt(startTime.ticks) : 0n;
        const endTicks = endTime?.ticks !== undefined ? BigInt(endTime.ticks) : 0n;
        const inTicks = inPoint?.ticks !== undefined ? BigInt(inPoint.ticks) : 0n;
        const clipSpan = endTicks > startTicks ? endTicks - startTicks : 0n;
        const outTicks =
          outPoint?.ticks !== undefined ? BigInt(outPoint.ticks) : inTicks + clipSpan;
        const track = typeof trackIndex === "number" ? trackIndex : 0;
        return { clip, startTicks, endTicks, inTicks, outTicks, track };
      })
    );

    // Group selected clips by track index
    const byTrack = new Map();
    for (const item of metas) {
      if (!byTrack.has(item.track)) {
        byTrack.set(item.track, []);
      }
      byTrack.get(item.track).push(item);
    }

    let playheadTicks = null;
    try {
      const pos = await sequence.getPlayerPosition?.();
      if (pos?.ticks !== undefined) {
        playheadTicks = BigInt(pos.ticks);
      }
    } catch (_) {}

    const ONE_FRAME_TOLERANCE = 12000000000n; // ~1 frame in Premiere ticks
    const targets = [];

    for (const trackItems of byTrack.values()) {
      trackItems.sort((a, b) => (a.startTicks < b.startTicks ? -1 : a.startTicks > b.startTicks ? 1 : 0));

      if (trackItems.length >= 2) {
        // Multi-clip selection on this track: apply transitions ONLY at the inner cuts/boundaries between consecutive clips
        for (let i = 0; i < trackItems.length - 1; i++) {
          const current = trackItems[i];
          const next = trackItems[i + 1];
          const gap = next.startTicks - current.endTicks;

          if (gap >= -ONE_FRAME_TOLERANCE && gap <= ONE_FRAME_TOLERANCE) {
            // Adjacent cut: place transition across the cut between current and next
            targets.push({
              kind: "cut",
              clip: current.clip,
              clipMeta: current,
              applyToStart: false,
              forceSingleSided: false,
              alignment: 0.5,
              fallbackClip: next.clip,
              fallbackMeta: next
            });
          } else {
            // Non-adjacent clips with a gap: apply to the two inner facing edges (current End + next Start)
            targets.push({
              kind: "edge",
              clip: current.clip,
              clipMeta: current,
              applyToStart: false,
              forceSingleSided: true,
              alignment: 1.0,
              fallbackClip: null,
              fallbackMeta: null
            });
            targets.push({
              kind: "edge",
              clip: next.clip,
              clipMeta: next,
              applyToStart: true,
              forceSingleSided: true,
              alignment: 0.0,
              fallbackClip: null,
              fallbackMeta: null
            });
          }
        }
      } else if (trackItems.length === 1) {
        // Single-clip selection on this track: pick start vs end based on Playhead proximity
        const single = trackItems[0];
        let applyToStart = false;
        if (playheadTicks !== null) {
          const distStart =
            playheadTicks >= single.startTicks
              ? playheadTicks - single.startTicks
              : single.startTicks - playheadTicks;
          const distEnd =
            playheadTicks >= single.endTicks
              ? playheadTicks - single.endTicks
              : single.endTicks - playheadTicks;
          applyToStart = distStart <= distEnd;
        }
        targets.push({
          kind: "single",
          clip: single.clip,
          clipMeta: single,
          applyToStart,
          forceSingleSided: false,
          alignment: 0.5,
          fallbackClip: null,
          fallbackMeta: null
        });
      }
    }

    return targets;
  }

  // `catalog` is the scanned effect list used to resolve the requested effect.
  async function applyEffectToSelection(payload, catalog) {
    const resolved = resolveEffectEntry(payload, catalog);
    const { name, matchName, effectType } = resolved;

    const project = await ppro.Project.getActiveProject();
    if (!project) {
      throw new Error("Open a Premiere Pro project first.");
    }

    const sequence = await project.getActiveSequence();
    if (!sequence) {
      throw new Error("Open a sequence in the Timeline first.");
    }

    const selection = await sequence.getSelection();
    if (!selection) {
      throw new Error("Select a clip in the Timeline first.");
    }

    const clips = (await selection.getTrackItems()) || [];
    if (clips.length === 0) {
      throw new Error("Select at least one timeline clip first.");
    }

    if (effectType === "videoEffect") {
      const videoClips = clips.filter((clip) => isVideoClip(clip));
      if (videoClips.length === 0) {
        throw new Error(`Select a video clip before applying "${name}".`);
      }

      const prepared = await Promise.all(
        videoClips.map(async (clip) => {
          const [chain, component] = await Promise.all([
            clip.getComponentChain(),
            ppro.VideoFilterFactory.createComponent(matchName)
          ]);
          if (!chain || !component) {
            throw new Error(`Premiere could not create video effect "${name}" (${matchName}).`);
          }
          return { chain, component };
        })
      );

      let ok = false;
      project.lockedAccess(() => {
        ok = project.executeTransaction((tx) => {
          for (const { chain, component } of prepared) {
            tx.addAction(chain.createAppendComponentAction(component));
          }
        }, `Apply ${name}`);
      });

      if (!ok) {
        throw new Error(`Premiere rejected applying "${name}".`);
      }
      return `Applied "${name}" to ${prepared.length} video clip${prepared.length === 1 ? "" : "s"}.`;
    }

    if (effectType === "audioEffect") {
      const audioClips = clips.filter((clip) => isAudioClip(clip));
      if (audioClips.length === 0) {
        throw new Error(`Select an audio clip before applying "${name}".`);
      }

      const prepared = await Promise.all(
        audioClips.map(async (clip) => {
          const chain = await clip.getComponentChain();
          let component = null;
          if (typeof ppro.AudioFilterFactory.createComponentByDisplayName === "function") {
            component = await ppro.AudioFilterFactory.createComponentByDisplayName(name, clip);
          } else if (typeof ppro.AudioFilterFactory.createComponent === "function") {
            component = await ppro.AudioFilterFactory.createComponent(matchName || name);
          }
          if (!chain || !component) {
            throw new Error(`Premiere could not create audio effect "${name}".`);
          }
          return { chain, component };
        })
      );

      let ok = false;
      project.lockedAccess(() => {
        ok = project.executeTransaction((tx) => {
          for (const { chain, component } of prepared) {
            tx.addAction(chain.createAppendComponentAction(component));
          }
        }, `Apply ${name}`);
      });

      if (!ok) {
        throw new Error(`Premiere rejected applying "${name}".`);
      }
      return `Applied "${name}" to ${prepared.length} audio clip${prepared.length === 1 ? "" : "s"}.`;
    }

    if (effectType === "videoTransition" && ppro.TransitionFactory) {
      const videoClips = clips.filter((clip) => isVideoClip(clip));
      if (videoClips.length === 0) {
        throw new Error(`Select a video clip before applying "${name}".`);
      }

      const [targets, durationInfo] = await Promise.all([
        planTrackTransitionTargets(sequence, videoClips),
        computeTransitionDurationInfo(sequence, payload.videoTransitionDuration, 30)
      ]);

      if (targets.length === 0) {
        throw new Error(`No valid transition cut points found for "${name}".`);
      }

      const prepared = await Promise.all(
        targets.map(async (target) => {
          const transition = await ppro.TransitionFactory.createVideoTransition(matchName);
          const fallbackTransition = target.fallbackClip
            ? await ppro.TransitionFactory.createVideoTransition(matchName)
            : null;
          return {
            ...target,
            transition,
            fallbackTransition
          };
        })
      );

      let applied = 0;
      project.lockedAccess(() => {
        for (const item of prepared) {
          if (!item.transition || typeof item.clip.createAddVideoTransitionAction !== "function") {
            continue;
          }
          const opts = createTransitionOptions({
            applyToStart: item.applyToStart,
            forceSingleSided: item.forceSingleSided,
            alignment: item.alignment,
            durationTickTime: durationInfo.durationTickTime
          });

          let committed = project.executeTransaction((tx) => {
            const action = opts
              ? item.clip.createAddVideoTransitionAction(item.transition, opts)
              : item.clip.createAddVideoTransitionAction(item.transition);
            if (action) {
              tx.addAction(action);
            }
          }, `Apply ${name}`);

          // If applying to outgoing clip end was rejected (e.g. insufficient tail handle), try incoming clip start at the same cut
          if (!committed && item.fallbackClip && item.fallbackTransition) {
            const fallbackOpts = createTransitionOptions({
              applyToStart: true,
              forceSingleSided: false,
              alignment: 0.5,
              durationTickTime: durationInfo.durationTickTime
            });
            committed = project.executeTransaction((tx) => {
              const action = fallbackOpts
                ? item.fallbackClip.createAddVideoTransitionAction(item.fallbackTransition, fallbackOpts)
                : item.fallbackClip.createAddVideoTransitionAction(item.fallbackTransition);
              if (action) {
                tx.addAction(action);
              }
            }, `Apply ${name}`);
          }

          if (committed) {
            applied++;
          }
        }
      });

      if (applied === 0) {
        throw new Error(`Could not apply transition "${name}".`);
      }
      return `Applied "${name}" (${durationInfo.label}) to ${applied} cut${applied === 1 ? "" : "s"}.`;
    }

    throw new Error(`Unsupported effect type: ${effectType}`);
  }

  return { scanEffectCatalog, applyEffectToSelection };
};
