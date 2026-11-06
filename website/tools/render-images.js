// Renders the social preview image, the PNG icons and favicon.ico into ../public.
// Run from the repository root: node website/tools/render-images.js
// Needs Playwright with Chromium (npm i -g playwright, or npx playwright install chromium).
const fs = require('fs');
const path = require('path');
const { chromium } = require('playwright');

const tools = __dirname;
const pub = path.join(tools, '..', 'public');

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1200, height: 630 } });
  await page.goto('file://' + path.join(tools, 'og.html'));
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({ path: path.join(pub, 'og.png') });

  const svg = fs.readFileSync(path.join(pub, 'favicon.svg'), 'utf8');
  const render = async (size) => {
    await page.setViewportSize({ width: size, height: size });
    await page.setContent(`<style>*{margin:0}svg{display:block}</style>${snap(svg, size)}`);
    return page.screenshot();
  };
  for (const [name, size] of [['apple-touch-icon.png', 180], ['icon-192.png', 192], ['icon-512.png', 512]]) {
    fs.writeFileSync(path.join(pub, name), await render(size));
  }
  // favicon.ico for browsers and crawlers that ask for /favicon.ico; 48 px
  // is the size Google Search wants for the icon next to results.
  const sizes = [16, 32, 48];
  const pngs = [];
  for (const size of sizes) pngs.push(await render(size));
  fs.writeFileSync(path.join(pub, 'favicon.ico'), ico(sizes, pngs));
  await browser.close();
})();

// favicon.svg scaled to size px with every rectangle edge rounded to a whole
// pixel, so the tiles stay sharp at sizes its 32-unit grid does not divide.
function snap(svg, size) {
  const k = size / 32;
  return svg
    .replace('viewBox="0 0 32 32"', `width="${size}" height="${size}" viewBox="0 0 ${size} ${size}" shape-rendering="crispEdges"`)
    .replace(/<rect([^>]*)\/>/g, (rect, attrs) => {
      const get = (name, fallback) => Number((attrs.match(new RegExp(` ${name}="([^"]*)"`)) || [, fallback])[1]);
      const x = get('x', 0), y = get('y', 0), w = get('width'), h = get('height');
      const x0 = Math.round(x * k), y0 = Math.round(y * k);
      const box = ` x="${x0}" y="${y0}" width="${Math.round((x + w) * k) - x0}" height="${Math.round((y + h) * k) - y0}"`;
      return `<rect${box}${attrs.replace(/ (x|y|width|height)="[^"]*"/g, '')}/>`;
    });
}

// An ICO file whose images are PNGs, which every current browser reads.
function ico(sizes, pngs) {
  const header = Buffer.alloc(6 + 16 * pngs.length);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(pngs.length, 4);
  let offset = header.length;
  pngs.forEach((png, i) => {
    const entry = 6 + 16 * i;
    header.writeUInt8(sizes[i] % 256, entry);
    header.writeUInt8(sizes[i] % 256, entry + 1);
    header.writeUInt16LE(1, entry + 4);
    header.writeUInt16LE(32, entry + 6);
    header.writeUInt32LE(png.length, entry + 8);
    header.writeUInt32LE(offset, entry + 12);
    offset += png.length;
  });
  return Buffer.concat([header, ...pngs]);
}
