/* Git Rustle website — vanilla JS, no dependencies. */
(function () {
  "use strict";

  document.documentElement.classList.remove("no-js");

  var reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  /* ── Scroll reveal ─────────────────────────────────────────── */
  var revealTargets = document.querySelectorAll(".reveal, .app");
  if ("IntersectionObserver" in window && !reduceMotion) {
    var io = new IntersectionObserver(function (entries) {
      for (var i = 0; i < entries.length; i++) {
        if (entries[i].isIntersecting) {
          entries[i].target.classList.add("in");
          io.unobserve(entries[i].target);
        }
      }
    }, { rootMargin: "0px 0px -8% 0px", threshold: 0.08 });
    revealTargets.forEach(function (el) { io.observe(el); });
  } else {
    revealTargets.forEach(function (el) { el.classList.add("in"); });
  }

  /* ── Hero parallax: icon tilts and streaks shift with the pointer ── */
  var hero = document.querySelector(".hero");
  var icon = document.querySelector(".hero-icon");
  var streaks = document.querySelectorAll(".streaks span");
  if (hero && icon && !reduceMotion && window.matchMedia("(pointer: fine)").matches) {
    var raf = 0, px = 0, py = 0;
    var apply = function () {
      raf = 0;
      icon.style.transform = "rotate3d(" + (-py).toFixed(3) + "," + px.toFixed(3) + ",0," + (Math.hypot(px, py) * 7).toFixed(2) + "deg)";
      for (var i = 0; i < streaks.length; i++) {
        var depth = (i % 4 + 1) * 6;
        streaks[i].style.setProperty("--px", (px * depth).toFixed(1) + "px");
      }
    };
    hero.addEventListener("pointermove", function (e) {
      var r = hero.getBoundingClientRect();
      px = (e.clientX - r.left) / r.width * 2 - 1;
      py = (e.clientY - r.top) / r.height * 2 - 1;
      if (!raf) raf = requestAnimationFrame(apply);
    }, { passive: true });
    hero.addEventListener("pointerleave", function () {
      px = 0; py = 0;
      if (!raf) raf = requestAnimationFrame(apply);
    });
  }

  /* ── Card hover spotlight follows the cursor ───────────────── */
  var cards = document.querySelectorAll(".card");
  if (window.matchMedia("(pointer: fine)").matches) {
    cards.forEach(function (card) {
      card.addEventListener("pointermove", function (e) {
        var r = card.getBoundingClientRect();
        card.style.setProperty("--mx", (e.clientX - r.left) + "px");
        card.style.setProperty("--my", (e.clientY - r.top) + "px");
      }, { passive: true });
    });
  }

  /* ── Latest version: progressive enhancement via GitHub API ── */
  var versionEls = document.querySelectorAll("[data-version]");
  if (versionEls.length && typeof fetch === "function") {
    var key = "git-rustle:latest";
    var cached = null;
    try { cached = JSON.parse(sessionStorage.getItem(key) || "null"); } catch (_) { /* ignore */ }
    var setVersion = function (tag) {
      if (!tag || !/^v?\d+\.\d+\.\d+/.test(tag)) return;
      versionEls.forEach(function (el) { el.textContent = tag; });
    };
    if (cached && cached.tag) {
      setVersion(cached.tag);
    } else {
      fetch("https://api.github.com/repos/pascaloon/git-rustle/releases/latest", {
        headers: { Accept: "application/vnd.github+json" }
      }).then(function (res) {
        return res.ok ? res.json() : null;
      }).then(function (json) {
        if (json && json.tag_name) {
          setVersion(json.tag_name);
          try { sessionStorage.setItem(key, JSON.stringify({ tag: json.tag_name })); } catch (_) { /* ignore */ }
        }
      }).catch(function () { /* keep static fallback */ });
    }
  }
})();
