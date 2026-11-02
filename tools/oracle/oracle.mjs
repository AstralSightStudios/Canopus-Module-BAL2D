// Reference pose dumper: drives the official spine-core 4.2 runtime through a scenario and
// writes per-frame world vertices, slot colors, draw order and fired events as JSON.
// The runtime is NOT part of this repository: pass the directory of an installed
// @esotericsoftware/spine-core package (npm install it somewhere outside the repo).
//
//   node tools/oracle/oracle.mjs <spine-core dist dir> <skel> <atlas> <scenario.json> <out.json>
import { readFileSync, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import path from "node:path";

const [distDir, skelPath, atlasPath, scenarioPath, outPath] = process.argv.slice(2);
if (!outPath) {
  console.error("usage: oracle.mjs <spine-core dist> <skel> <atlas> <scenario.json> <out.json>");
  process.exit(2);
}
const spine = await import(pathToFileURL(path.join(distDir, "index.js")).href);
const {
  TextureAtlas, AtlasAttachmentLoader, SkeletonBinary, Skeleton, AnimationStateData,
  AnimationState, Physics, RegionAttachment, MeshAttachment,
} = spine;

const atlas = new TextureAtlas(readFileSync(atlasPath, "utf8"));
const binary = new SkeletonBinary(new AtlasAttachmentLoader(atlas));
const bytes = readFileSync(skelPath);
const data = binary.readSkeletonData(new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength));
const scenario = JSON.parse(readFileSync(scenarioPath, "utf8"));

const skeleton = new Skeleton(data);
skeleton.setToSetupPose();
skeleton.updateWorldTransform(Physics.update);
const stateData = new AnimationStateData(data);
stateData.defaultMix = scenario.defaultMix ?? 0.25;
const state = new AnimationState(stateData);
const fired = [];
state.addListener({
  event: (entry, e) => fired.push({ track: entry.trackIndex, name: e.data.name, time: e.time }),
  complete: (entry) => fired.push({ track: entry.trackIndex, complete: entry.animation.name }),
});

const dt = scenario.dt;
const frames = [];
const poses = []; // active bone overrides: {name, x, y, until}
for (let f = 0; f < scenario.frames; f++) {
  for (const a of scenario.actions.filter((a) => a.frame === f)) {
    switch (a.op) {
      case "set":
        if (data.findAnimation(a.anim)) state.setAnimation(a.track, a.anim, !!a.loop);
        break;
      case "add":
        if (data.findAnimation(a.anim)) state.addAnimation(a.track, a.anim, !!a.loop, a.delay ?? 0);
        break;
      case "setEmpty":
        state.setEmptyAnimation(a.track, a.mix ?? 0);
        break;
      case "addEmpty":
        state.addEmptyAnimation(a.track, a.mix ?? 0, a.delay ?? 0);
        break;
      case "bone":
        poses.push({ name: a.name, dx: a.dx, dy: a.dy, until: a.until });
        break;
    }
  }
  state.update(dt);
  state.apply(skeleton);
  for (const p of poses) {
    if (f >= p.until) continue;
    const b = skeleton.findBone(p.name);
    if (b) { b.x = b.data.x + p.dx; b.y = b.data.y + p.dy; }
  }
  skeleton.update(dt);
  skeleton.updateWorldTransform(Physics.update);
  if (f % scenario.dumpEvery === 0 || f === scenario.frames - 1) {
    const slots = [];
    for (const slot of skeleton.drawOrder) {
      const att = slot.getAttachment();
      if (!att) continue;
      let verts = null;
      if (att instanceof RegionAttachment) {
        verts = new Array(8);
        att.computeWorldVertices(slot, verts, 0, 2);
      } else if (att instanceof MeshAttachment) {
        verts = new Array(att.worldVerticesLength);
        att.computeWorldVertices(slot, 0, att.worldVerticesLength, verts, 0, 2);
      } else continue;
      const c = slot.color;
      slots.push({ slot: slot.data.index, att: att.name, color: [c.r, c.g, c.b, c.a], v: verts.map((x) => Math.round(x * 1000) / 1000) });
    }
    const entry = { frame: f, slots, events: fired.splice(0) };
    if (scenario.dumpBones) {
      entry.bones = skeleton.bones.map((b) => [b.a, b.b, b.c, b.d, b.worldX, b.worldY, b.x, b.y, b.rotation, b.scaleX, b.scaleY, b.shearX, b.shearY]);
      entry.ik = skeleton.ikConstraints.map((c) => [c.mix, c.softness, c.bendDirection, c.compress ? 1 : 0, c.stretch ? 1 : 0, c.active ? 1 : 0]);
      entry.physics = skeleton.physicsConstraints.map((c) => [c.mix, c.inertia, c.strength, c.damping, c.massInverse, c.wind, c.gravity, c.active ? 1 : 0]);
    }
    frames.push(entry);
  }
}
writeFileSync(outPath, JSON.stringify({ frames }));
console.log(`wrote ${frames.length} frames to ${outPath}`);
