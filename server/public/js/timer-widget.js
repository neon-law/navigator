// The matter header's running-timer display — first-party, same-origin,
// zero telemetry.
//
// The timer's durable state is the `timer` row in PostgreSQL; the page is
// only an animation of it. This script animates the elapsed label from the
// `data-timer-started-at` epoch-milliseconds attribute the server renders;
// before it loads (or with JavaScript off) the server-rendered label stands
// in, so the recovered state is correct however the tab was left. Stopping
// and discarding stay native form POSTs — this script never submits anything.
//
// Inert unless a `[data-timer-started-at]` node is on the page, so it is
// safe to load on any page — like the other first-party scripts.
//
// Expected markup (see `webapp/src/project_billing.rs`):
//   <span class="matter-timer__elapsed" data-timer-started-at="1730000000000"
//         role="timer">0:00:12</span>
(function () {
  "use strict";

  // Same shape as the server's `webapp::project_billing::format_elapsed` —
  // hours unbounded, minutes and seconds zero-padded — so the animated label
  // never jumps when it takes over from the rendered one.
  function formatElapsed(ms) {
    var total = Math.max(0, Math.floor(ms / 1000));
    var h = Math.floor(total / 3600);
    var m = Math.floor((total % 3600) / 60);
    var s = total % 60;
    return h + ":" + String(m).padStart(2, "0") + ":" + String(s).padStart(2, "0");
  }

  function init() {
    var nodes = document.querySelectorAll("[data-timer-started-at]");
    if (!nodes.length) {
      return;
    }
    var started = [];
    nodes.forEach(function (node) {
      var millis = Number(node.getAttribute("data-timer-started-at"));
      if (Number.isFinite(millis)) {
        started.push([node, millis]);
      }
    });
    if (!started.length) {
      return;
    }
    function tick() {
      var now = Date.now();
      started.forEach(function (pair) {
        pair[0].textContent = formatElapsed(now - pair[1]);
      });
    }
    tick();
    setInterval(tick, 1000);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
