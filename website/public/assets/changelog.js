// The changelog's before/after pictures: drag the divider, or focus the
// picture and use the arrow keys. Without this script the divider stays in
// the middle.
"use strict";

for (const $compare of document.querySelectorAll(".compare")) {
  const set_split = (percent) => {
    const clamped = Math.min(100, Math.max(0, percent));
    $compare.style.setProperty("--split", `${clamped}%`);
    $compare.setAttribute("aria-valuenow", String(Math.round(clamped)));
  };
  const follow = (event) => {
    const box = $compare.getBoundingClientRect();
    set_split(((event.clientX - box.left) / box.width) * 100);
  };

  $compare.addEventListener("pointerdown", (event) => {
    $compare.setPointerCapture(event.pointerId);
    $compare.classList.add("dragging");
    follow(event);
  });
  $compare.addEventListener("pointermove", (event) => {
    if ($compare.classList.contains("dragging")) {
      follow(event);
    }
  });
  for (const name of ["pointerup", "pointercancel"]) {
    $compare.addEventListener(name, () => $compare.classList.remove("dragging"));
  }
  $compare.addEventListener("keydown", (event) => {
    const now = Number($compare.getAttribute("aria-valuenow"));
    const steps = { ArrowLeft: -5, ArrowRight: 5, Home: -100, End: 100 };
    if (!(event.key in steps)) {
      return;
    }
    event.preventDefault();
    set_split(now + steps[event.key]);
  });
}
