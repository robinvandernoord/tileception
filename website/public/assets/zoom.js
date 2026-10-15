// Tileception site: scroll-driven recursive zoom.
// Every level is a <section class="lvl">; one block in it (the portal) holds the next level.
// Scrolling moves a camera from level to level. Without this script, or with reduced
// motion, the same markup reads as a stacked page.
(() => {
  'use strict';
  const root = document.documentElement;

  // Copy buttons work in both modes.
  document.querySelectorAll('button[data-copy]').forEach(btn => {
    btn.addEventListener('click', () => {
      const el = document.getElementById(btn.dataset.copy);
      const done = label => { btn.textContent = label; setTimeout(() => { btn.textContent = 'Copy'; }, 1600); };
      const select = () => { const r = document.createRange(); r.selectNodeContents(el); const s = getSelection(); s.removeAllRanges(); s.addRange(r); done('Selected'); };
      if (navigator.clipboard) navigator.clipboard.writeText(el.textContent).then(() => done('Copied'), select);
      else select();
    });
  });

  if (matchMedia('(prefers-reduced-motion: reduce)').matches) return;

  const scroller = document.getElementById('scroller');
  const stage = document.getElementById('stage');
  const world = document.getElementById('world');
  const hud = document.getElementById('hud-levels');
  const levels = [...world.querySelectorAll('.lvl')];   // document order is depth order
  const steps = levels.length - 1;
  if (!scroller || steps < 1) return;
  root.classList.add('zoom');

  // The last level ends on a small copy of the first one.
  const loop = document.getElementById('loop');
  const mini = levels[0].cloneNode(true);
  mini.querySelector('.portal > .lvl')?.remove();
  mini.removeAttribute('id');
  mini.querySelectorAll('[id]').forEach(el => el.removeAttribute('id'));
  mini.setAttribute('aria-hidden', 'true');
  mini.setAttribute('data-nosnippet', '');
  mini.inert = true;
  loop.appendChild(mini);

  // Level links at the bottom of the screen
  const colors = { r: 'var(--red)', y: 'var(--yellow)', b: 'var(--blue)', ink: 'var(--canvas)' };
  const hudLinks = levels.map(lvl => {
    const a = document.createElement('a');
    a.href = '#' + lvl.id;
    a.style.setProperty('--c', colors[lvl.dataset.color] || colors.ink);
    const label = document.createElement('span'); label.textContent = lvl.dataset.name;
    a.appendChild(label);
    hud.appendChild(a);
    return a;
  });

  const snaps = levels.map(() => { const s = document.createElement('div'); s.className = 'snap'; scroller.appendChild(s); return s; });

  const hudTop = document.querySelector('.hud-top');
  const clamp = (v, a, b) => Math.min(Math.max(v, a), b);
  let W = 1600, H = 1000, seg = 1, rects = [], current = -1, bandTop = 0, bandBottom = 0;

  // Fit a level into a portal cell; returns its offset and scale inside the parent level.
  function place(child, cell) {
    const cw = cell.clientWidth, ch = cell.clientHeight, s = Math.min(cw / W, ch / H);
    const ox = (cw - W * s) / 2, oy = (ch - H * s) / 2;
    child.style.transform = `translate(${ox}px, ${oy}px) scale(${s})`;
    return { x: cell.offsetLeft + ox, y: cell.offsetTop + oy, s };
  }

  function layout() {
    const portrait = innerWidth / innerHeight < 0.85;
    root.classList.toggle('portrait', portrait);
    // Keep the level bar (and on wide screens the top bar) on black, outside the picture.
    const vw = stage.clientWidth, vh = stage.clientHeight;
    bandBottom = vh - hud.offsetTop + 8;
    bandTop = portrait ? 0 : hudTop.offsetHeight + 8;
    W = portrait ? 900 : 1600;
    // On phones the canvas takes the screen's own proportions, within what the content needs.
    H = portrait ? Math.round(clamp(W * (vh - bandTop - bandBottom) / vw, 1500, 1900)) : 1000;
    root.style.setProperty('--H', H + 'px');
    root.style.setProperty('--hr', String(H / W));
    rects = [{ x: 0, y: 0, w: W, h: H }];
    for (let k = 0; k < steps; k++) {
      const p = place(levels[k + 1], levels[k].querySelector(':scope > .portal'));
      const r = rects[k], sc = r.w / W;
      rects.push({ x: r.x + p.x * sc, y: r.y + p.y * sc, w: r.w * p.s, h: r.h * p.s });
    }
    place(mini, loop);
    seg = Math.round(vh * 1.3);
    scroller.style.height = (steps * seg + vh) + 'px';
    snaps.forEach((s, i) => { s.style.top = (i * seg) + 'px'; });
    frame();
  }

  const ease = t => t < .5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;

  function frame() {
    const vw = stage.clientWidth, vh = stage.clientHeight - bandTop - bandBottom;
    const p = clamp(-scroller.getBoundingClientRect().top / seg, 0, steps);
    const k = Math.min(Math.floor(p), steps - 1);
    const t = ease(clamp((p - k - 0.45) / 0.55, 0, 1));    // rest on each level, then zoom
    const a = rects[k], b = rects[k + 1], r = b.w / a.w;
    // A zoom between two nested frames is a pure scale about one fixed point.
    const fx = (b.x - r * a.x) / (1 - r), fy = (b.y - r * a.y) / (1 - r), m = Math.pow(r, t);
    const cx = fx + (a.x - fx) * m, cy = fy + (a.y - fy) * m, cw = a.w * m, ch = a.h * m;
    const S = Math.min(vw / cw, vh / ch);
    world.style.transform = `translate(${(vw - cw * S) / 2 - cx * S}px, ${bandTop + (vh - ch * S) / 2 - cy * S}px) scale(${S})`;

    const lv = t > 0.5 ? k + 1 : k;
    if (lv !== current) {
      current = lv;
      hudLinks.forEach((a, i) => { if (i === lv) a.setAttribute('aria-current', 'step'); else a.removeAttribute('aria-current'); });
    }
    root.classList.toggle('hud-on', p > 0.35);
  }

  function scrollToLevel(i, smooth) {
    const top = scroller.getBoundingClientRect().top + scrollY + i * seg;
    scrollTo({ top, behavior: smooth ? 'smooth' : 'auto' });
  }
  const levelOf = el => levels.indexOf(el.closest('.lvl'));

  // In-page links go to a level's resting point instead of the element's box.
  document.addEventListener('click', e => {
    const a = e.target.closest('a[href^="#"]');
    if (a) {
      const i = levels.findIndex(l => '#' + l.id === a.getAttribute('href'));
      if (i < 0) return;
      e.preventDefault();
      scrollToLevel(i, true);
      history.replaceState(null, '', i === 0 ? location.pathname : '#' + levels[i].id);
      return;
    }
    // Clicking a deeper level zooms to it.
    if (e.target.closest('button, a, code, kbd')) return;
    const lvl = e.target.closest('.lvl');
    if (lvl && world.contains(lvl)) { const i = levelOf(lvl); if (i > current) scrollToLevel(i, true); }
  });

  // Keyboard focus on something in another level brings that level into view.
  document.addEventListener('focusin', e => {
    if (!world.contains(e.target) || !e.target.matches(':focus-visible')) return;
    const i = levelOf(e.target);
    if (i >= 0 && i !== current) scrollToLevel(i, false);
  });

  let queued = false;
  const request = fn => () => { if (!queued) { queued = true; requestAnimationFrame(() => { queued = false; fn(); }); } };
  addEventListener('scroll', request(frame), { passive: true });
  addEventListener('resize', request(layout));

  layout();
  const start = levels.findIndex(l => '#' + l.id === location.hash);
  if (start > 0) scrollToLevel(start, false);
})();
