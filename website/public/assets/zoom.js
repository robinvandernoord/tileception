// Tileception site: scroll-driven recursive zoom.
// Every level is a <section class="lvl">; one block in it (the portal) holds the next level.
// Scrolling moves a camera from level to level. Without this script, or with reduced
// motion, the same markup reads as a stacked page.
(() => {
  "use strict";
  const $root = document.documentElement;

  // Copy buttons work in both modes.
  document.querySelectorAll("button[data-copy]").forEach(($button) => {
    $button.addEventListener("click", () => {
      const $source = document.getElementById($button.dataset.copy);
      const $label = $button.querySelector(".copy-label") || $button;
      function show_done(label) {
        $label.textContent = label;
        setTimeout(() => {
          $label.textContent = "Copy";
        }, 1600);
      }
      function select_source() {
        const range = document.createRange();
        range.selectNodeContents($source);
        const selection = getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        show_done("Selected");
      }
      if (navigator.clipboard) {
        navigator.clipboard
          .writeText($source.textContent)
          .then(() => show_done("Copied"), select_source);
      } else {
        select_source();
      }
    });
  });

  // Shows a command on one line as far as it fits. A long URL may wrap only after
  // a slash, never inside a word, and the text that is copied stays one line.
  function show_command($code, command) {
    $code.replaceChildren();
    command.split(/(?<=\/)/).forEach((part, index) => {
      if (index > 0) {
        $code.appendChild(document.createElement("wbr"));
      }
      $code.appendChild(document.createTextNode(part));
    });
  }

  // Install variants swap the command in place, so the block keeps its size.
  document.querySelectorAll(".cmd-tabs").forEach(($tabs) => {
    const $$buttons = [...$tabs.querySelectorAll('[role="tab"]')];
    function select_tab($button) {
      $$buttons.forEach(($other) =>
        $other.setAttribute("aria-selected", String($other === $button)),
      );
      show_command(
        document.getElementById($button.getAttribute("aria-controls")),
        $button.dataset.cmd,
      );
    }
    $$buttons.forEach(($button) =>
      $button.addEventListener("click", () => select_tab($button)),
    );
    select_tab(
      $$buttons.find(
        ($button) => $button.getAttribute("aria-selected") === "true",
      ) || $$buttons[0],
    );
    $tabs.hidden = false;
  });

  const reduced_motion = matchMedia("(prefers-reduced-motion: reduce)").matches;

  // The promo video: a silent preview loops while it is on screen (never with
  // reduced motion), and its tile opens the whole video with sound in a dialog.
  // Without this script the tile is a plain link to the video file.
  const $watch = document.getElementById("watch");
  document.querySelectorAll("[data-watch]").forEach(($link) => {
    const $preview = $link.querySelector("video");
    if ($preview && !reduced_motion && "IntersectionObserver" in window) {
      $preview.preload = "auto";
      new IntersectionObserver((entries) => {
        entries.forEach((entry) => {
          if (entry.isIntersecting) {
            $preview.play().catch(() => {});
          } else {
            $preview.pause();
          }
        });
      }).observe($preview);
    }
    if (!$watch || !$watch.showModal) {
      return;
    }
    $link.addEventListener("click", (event) => {
      event.preventDefault();
      const $video = $watch.querySelector("video");
      $watch.showModal();
      $video.currentTime = 0;
      $video.play().catch(() => {});
    });
  });
  if ($watch) {
    $watch.addEventListener("close", () =>
      $watch.querySelector("video").pause(),
    );
    // A click on the dark backdrop closes it, as Escape does.
    $watch.addEventListener("click", (event) => {
      if (event.target === $watch) {
        $watch.close();
      }
    });
  }

  if (reduced_motion) {
    return;
  }

  const $scroller = document.getElementById("scroller");
  const $stage = document.getElementById("stage");
  const $world = document.getElementById("world");
  const $hud = document.getElementById("hud-levels");
  const $$levels = [...$world.querySelectorAll(".lvl")]; // document order is depth order
  const steps = $$levels.length - 1;
  if (!$scroller || steps < 1) {
    return;
  }
  $root.classList.add("zoom");

  // The last level ends on a small copy of the first one.
  const $loop = document.getElementById("loop");
  const $mini = $$levels[0].cloneNode(true);
  $mini.querySelector(".portal > .lvl")?.remove();
  $mini.removeAttribute("id");
  $mini
    .querySelectorAll("[id]")
    .forEach(($element) => $element.removeAttribute("id"));
  // The copy shows the preview's still instead of playing it a second time.
  $mini.querySelectorAll("video").forEach(($video) => {
    $video.removeAttribute("src");
    $video.preload = "none";
  });
  $mini.setAttribute("aria-hidden", "true");
  $mini.setAttribute("data-nosnippet", "");
  $mini.inert = true;
  $loop.appendChild($mini);

  // Level links at the bottom of the screen
  const colors = {
    r: "var(--red)",
    y: "var(--yellow)",
    b: "var(--blue)",
    ink: "var(--canvas)",
  };
  const $$hud_links = $$levels.map(($level) => {
    const $link = document.createElement("a");
    $link.href = "#" + $level.id;
    $link.style.setProperty("--c", colors[$level.dataset.color] || colors.ink);
    const $label = document.createElement("span");
    $label.textContent = $level.dataset.name;
    $link.appendChild($label);
    $hud.appendChild($link);
    return $link;
  });

  // Each snap point gets its place before it joins the page. Unplaced, they would all sit
  // at the same spot, the browser could pick the last one as the point the page is snapped
  // to, and once layout() spreads them out it would follow that one down to the last level.
  const $$snaps = $$levels.map((_level, index) => {
    const $snap = document.createElement("div");
    $snap.className = "snap";
    $snap.style.top = `calc(${index} * 100svh)`;
    $scroller.appendChild($snap);
    return $snap;
  });

  const $hud_top = document.querySelector(".hud-top");
  let canvas_width = 1600,
    canvas_height = 1000,
    step_height = 1,
    rects = [],
    current = -1;
  let band_top = 0,
    band_bottom = 0,
    view_width = 0,
    view_height = 0,
    scroller_top = 0;

  function clamp(value, low, high) {
    return Math.min(Math.max(value, low), high);
  }

  // Fit a level into a portal cell; returns its offset and scale inside the parent level.
  function place($child, $cell) {
    const cell_width = $cell.clientWidth,
      cell_height = $cell.clientHeight;
    const scale = Math.min(
      cell_width / canvas_width,
      cell_height / canvas_height,
    );
    const offset_x = (cell_width - canvas_width * scale) / 2,
      offset_y = (cell_height - canvas_height * scale) / 2;
    $child.style.transform = `translate(${offset_x}px, ${offset_y}px) scale(${scale})`;
    return {
      x: $cell.offsetLeft + offset_x,
      y: $cell.offsetTop + offset_y,
      scale,
    };
  }

  function layout() {
    // The stage is 100svh, so a mobile URL bar sliding in or out changes nothing here.
    // Skip those resizes instead of rebuilding everything in the middle of a scroll.
    if (
      $stage.clientWidth === view_width &&
      $stage.clientHeight === view_height
    ) {
      return;
    }
    view_width = $stage.clientWidth;
    view_height = $stage.clientHeight;
    const portrait = view_width / view_height < 0.85;
    $root.classList.toggle("portrait", portrait);
    // Keep the level bar (and on wide screens the top bar) on black, outside the picture.
    band_bottom = view_height - $hud.offsetTop + 8;
    band_top = portrait ? 0 : $hud_top.offsetHeight + 8;
    canvas_width = portrait ? 900 : 1600;
    // On phones the canvas takes the screen's own proportions, within what the content needs.
    canvas_height = portrait
      ? Math.round(
          clamp(
            (canvas_width * (view_height - band_top - band_bottom)) /
              view_width,
            1500,
            1900,
          ),
        )
      : 1000;
    $root.style.setProperty("--H", canvas_height + "px");
    $root.style.setProperty("--hr", String(canvas_height / canvas_width));
    rects = [{ x: 0, y: 0, width: canvas_width, height: canvas_height }];
    for (let depth = 0; depth < steps; depth++) {
      const placed = place(
        $$levels[depth + 1],
        $$levels[depth].querySelector(":scope > .portal"),
      );
      const parent = rects[depth],
        parent_scale = parent.width / canvas_width;
      rects.push({
        x: parent.x + placed.x * parent_scale,
        y: parent.y + placed.y * parent_scale,
        width: parent.width * placed.scale,
        height: parent.height * placed.scale,
      });
    }
    place($mini, $loop);
    step_height = view_height;
    $scroller.style.height = steps * step_height + view_height + "px";
    $$snaps.forEach(($snap, index) => {
      $snap.style.top = index * step_height + "px";
    });
    scroller_top = $scroller.getBoundingClientRect().top + scrollY;
    frame();
  }

  function ease(progress) {
    return progress * progress * (3 - 2 * progress);
  }

  // While the camera moves, the world is one composited layer that the GPU only scales;
  // re-rendering all of its text at a new size every frame is what makes phones stutter.
  // Once scrolling stops the hint is dropped so the browser redraws it sharp.
  let settle_timer = 0,
    settled = false;
  function settle() {
    $world.classList.remove("moving");
    settled = true;
  }

  function frame() {
    const picture_height = view_height - band_top - band_bottom;
    const position = clamp((scrollY - scroller_top) / step_height, 0, steps);
    const depth = Math.min(Math.floor(position), steps - 1);
    // Scrolling snaps to one level at a time, so the zoom fills the whole step between them.
    const progress = ease(position - depth);
    const from = rects[depth],
      to = rects[depth + 1],
      ratio = to.width / from.width;
    // A zoom between two nested frames is a pure scale about one fixed point.
    const fixed_x = (to.x - ratio * from.x) / (1 - ratio),
      fixed_y = (to.y - ratio * from.y) / (1 - ratio);
    const zoom = Math.pow(ratio, progress);
    const camera_x = fixed_x + (from.x - fixed_x) * zoom,
      camera_y = fixed_y + (from.y - fixed_y) * zoom;
    const camera_width = from.width * zoom,
      camera_height = from.height * zoom;
    const scale = Math.min(
      view_width / camera_width,
      picture_height / camera_height,
    );
    const shift_x = (view_width - camera_width * scale) / 2 - camera_x * scale;
    const shift_y =
      band_top +
      (picture_height - camera_height * scale) / 2 -
      camera_y * scale;
    $world.style.transform = `translate(${shift_x}px, ${shift_y}px) scale(${scale})`;

    const level = progress > 0.5 ? depth + 1 : depth;
    if (position > 0.35) {
      $root.classList.add("zoomed");
    }
    if (level !== current) {
      // Halfway through a zoom, let the layer redraw once at the new scale so the
      // level coming in is never stretched far beyond its last rendering.
      if (current >= 0) {
        settle();
      }
      $$levels[current]?.classList.remove("current");
      current = level;
      $$levels[current].classList.add("current");
      $$hud_links.forEach(($link, index) => {
        if (index === level) {
          $link.setAttribute("aria-current", "step");
        } else {
          $link.removeAttribute("aria-current");
        }
      });
    }
    $root.classList.toggle("hud-on", position > 0.35);
  }

  function scroll_to_level(index, smooth) {
    scrollTo({
      top: scroller_top + index * step_height,
      behavior: smooth ? "smooth" : "auto",
    });
  }

  function level_of($element) {
    return $$levels.indexOf($element.closest(".lvl"));
  }

  function level_for_hash(hash) {
    return $$levels.findIndex(($level) => "#" + $level.id === hash);
  }

  // In-page links go to a level's resting point instead of the element's box.
  document.addEventListener("click", (event) => {
    // A pointer click anywhere in a deeper level, even on a link or a button in
    // its small preview, zooms in by one level, however deep the click landed.
    // Keyboard activation (detail 0) acts on the focused element as usual.
    const $level = event.target.closest(".lvl");
    if (event.detail > 0 && $level && $world.contains($level)) {
      if (level_of($level) > current) {
        event.preventDefault();
        scroll_to_level(current + 1, true);
        return;
      }
    }
    const $link = event.target.closest('a[href^="#"]');
    if ($link) {
      const index = level_for_hash($link.getAttribute("href"));
      if (index < 0) {
        return;
      }
      event.preventDefault();
      scroll_to_level(index, true);
      history.replaceState(
        null,
        "",
        index === 0 ? location.pathname : "#" + $$levels[index].id,
      );
      return;
    }
  });

  // Keyboard focus on something in another level brings that level into view.
  document.addEventListener("focusin", (event) => {
    if (
      !$world.contains(event.target) ||
      !event.target.matches(":focus-visible")
    ) {
      return;
    }
    const index = level_of(event.target);
    if (index >= 0 && index !== current) {
      scroll_to_level(index, false);
    }
  });

  // Runs the handler at most once per animation frame, however many events arrive.
  let queued = false;
  function once_per_frame(handler) {
    return () => {
      if (queued) {
        return;
      }
      queued = true;
      requestAnimationFrame(() => {
        queued = false;
        handler();
      });
    };
  }

  addEventListener(
    "scroll",
    once_per_frame(() => {
      settled = false;
      frame();
      // Re-adding the hint in the frame that dropped it would undo the redraw.
      if (!settled) {
        $world.classList.add("moving");
      }
      clearTimeout(settle_timer);
      settle_timer = setTimeout(settle, 150);
    }),
    { passive: true },
  );
  addEventListener("resize", once_per_frame(layout));

  layout();
  const start = level_for_hash(location.hash);
  if (start > 0) {
    scroll_to_level(start, false);
  }
})();
