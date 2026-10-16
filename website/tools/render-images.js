// Renders the social preview image and the PNG icons into ../public.
// Run from the repository root: node website/tools/render-images.js
// Needs Playwright with Chromium (npm i -g playwright, or npx playwright install chromium).
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

  const svg = 'file://' + path.join(pub, 'favicon.svg');
  for (const [name, size] of [['favicon-32.png', 32], ['apple-touch-icon.png', 180], ['icon-192.png', 192], ['icon-512.png', 512]]) {
    await page.setViewportSize({ width: size, height: size });
    await page.setContent(`<style>*{margin:0}img{display:block;width:${size}px;height:${size}px}</style><img src="${svg}">`);
    await page.waitForLoadState('load');
    await page.screenshot({ path: path.join(pub, name) });
  }
  await browser.close();
})();
