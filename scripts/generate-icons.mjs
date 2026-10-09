// Generates every icon derivative from the master artwork, AI-Gateway.png.
//
//   node scripts/generate-icons.mjs        (from the repository root)
//
// The master has a transparent margin around the rounded square, which would
// make the icon tiny at favicon sizes, so it is cropped to the square first.
// `sharp` is not a direct dependency; it ships with Next.js, so it is resolved
// through the frontend's `next` install (run `pnpm install` in frontend/ first).
//
// Outputs (commit them; they are what the app actually serves/embeds):
//   frontend/src/app/icon.png, apple-icon.png, favicon.ico   Next.js file-convention icons
//   frontend/public/icons/*.png                              manifest icons + sidebar logo
//   assets/icon.ico                                          Windows exe icon (embedded by build.rs)
//   assets/favicon.ico, assets/icon.png                      served by the gateway at /favicon.ico and /icon.png

import { createRequire } from "node:module";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const requireFromNext = createRequire(
  createRequire(resolve(root, "frontend/package.json")).resolve("next/package.json"),
);
const sharp = requireFromNext("sharp");

// Opaque area of the master (alpha > 128) is x 159..1094, y 170..1065 of
// 1254x1254; a 936px square centered on it keeps the whole shape.
const CROP = { left: 159, top: 150, width: 936, height: 936 };
// The artwork's own background, for icons that must be opaque (Apple).
const BACKGROUND = { r: 31, g: 34, b: 38, alpha: 1 };

const source = resolve(root, "AI-Gateway.png");
const square = await sharp(source).extract(CROP).png().toBuffer();

const png = (size, { flatten = false } = {}) => {
  let img = sharp(square).resize(size, size, { kernel: "lanczos3" });
  if (flatten) img = img.flatten({ background: BACKGROUND });
  return img.png({ compressionLevel: 9 }).toBuffer();
};

function ico(entries) {
  // ICO with PNG-compressed images (supported since Windows Vista).
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(entries.length, 4);
  const directory = Buffer.alloc(16 * entries.length);
  let offset = header.length + directory.length;
  entries.forEach(({ size, data }, i) => {
    const at = i * 16;
    directory.writeUInt8(size >= 256 ? 0 : size, at); // width (0 = 256)
    directory.writeUInt8(size >= 256 ? 0 : size, at + 1); // height
    directory.writeUInt8(0, at + 2); // palette colors
    directory.writeUInt8(0, at + 3);
    directory.writeUInt16LE(1, at + 4); // planes
    directory.writeUInt16LE(32, at + 6); // bits per pixel
    directory.writeUInt32LE(data.length, at + 8);
    directory.writeUInt32LE(offset, at + 12);
    offset += data.length;
  });
  return Buffer.concat([header, directory, ...entries.map((e) => e.data)]);
}

const write = (relative, data) => {
  const path = resolve(root, relative);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, data);
  console.log(`${relative.padEnd(46)} ${(data.length / 1024).toFixed(1)} KB`);
};

// Frontend: Next.js picks these up by file name and adds the <link> tags.
write("frontend/src/app/icon.png", await png(512));
write("frontend/src/app/apple-icon.png", await png(180, { flatten: true }));
write(
  "frontend/src/app/favicon.ico",
  ico(await Promise.all([16, 32, 48].map(async (size) => ({ size, data: await png(size) })))),
);
// Frontend: web app manifest icons and the sidebar logo (drawn at 32px CSS, so
// 128px covers 4x displays).
write("frontend/public/icons/icon-192.png", await png(192));
write("frontend/public/icons/icon-512.png", await png(512));
write("frontend/public/icons/ai-gateway-128.png", await png(128));

// Backend: exe icon (embedded by build.rs) and the images the gateway serves
// at /favicon.ico and /icon.png.
write(
  "assets/icon.ico",
  ico(await Promise.all([16, 24, 32, 48, 64, 128, 256].map(async (size) => ({ size, data: await png(size) })))),
);
write("assets/icon.png", await png(256));
write(
  "assets/favicon.ico",
  ico(await Promise.all([16, 32, 48].map(async (size) => ({ size, data: await png(size) })))),
);
