// Rasteriza media/icon-source.svg → media/icon.png (256×256) para la galería de
// Open VSX / VS Marketplace, que exige un PNG (el SVG solo vale para la UI).
// `sharp` no es dependencia del proyecto (el PNG va commiteado); instálalo solo
// al regenerar:  npm i -D sharp && npm run build:icon  (o `npx`).
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import sharp from "sharp";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const svg = readFileSync(join(root, "media/icon-source.svg"));
const png = await sharp(svg, { density: 384 })
  .resize(256, 256, { fit: "contain", background: { r: 0, g: 0, b: 0, alpha: 0 } })
  .png()
  .toBuffer();
writeFileSync(join(root, "media/icon.png"), png);
console.log(`media/icon.png escrito (${png.length} bytes)`);
