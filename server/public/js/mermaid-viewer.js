// Enhance Markdown fences while keeping their source readable and copyable.
(function () {
  "use strict";
  if (window.__navMermaidViewer) return;
  window.__navMermaidViewer = true;

  var frames = new Map();
  var nextId = 0;

  window.addEventListener("message", function (event) {
    // The diagram frame is sandboxed without allow-same-origin, so its origin
    // is the opaque "null"; the frame registry below is the identity check.
    if (event.origin !== "null") return;
    var entry = frames.get(event.source);
    if (!entry || !event.data || event.data.type !== "navigator-mermaid") return;
    if (event.data.ready) {
      event.source.postMessage({ type: "navigator-mermaid-source", source: entry.source }, "*");
    } else if (event.data.ok) {
      clearTimeout(entry.timeout);
      entry.frame.hidden = false;
      entry.frame.style.visibility = "visible";
      entry.frame.style.height = Math.min(2400, Math.max(160, Number(event.data.height) || 160)) + "px";
      if (!entry.rendered) entry.details.open = false;
      entry.rendered = true;
      entry.status.textContent = "";
    } else {
      clearTimeout(entry.timeout);
      entry.frame.hidden = true;
      entry.details.open = true;
      entry.status.textContent = "Diagram unavailable. The Mermaid source is shown below.";
    }
  });

  function enhance() {
    document.querySelectorAll("pre > code.language-mermaid").forEach(function (code) {
      if (code.closest(".nav-mermaid")) return;
      var pre = code.parentElement;
      var block = pre.closest(".nav-code") || pre;
      var figure = document.createElement("figure");
      figure.className = "nav-mermaid";
      var caption = document.createElement("figcaption");
      caption.textContent = "Mermaid diagram";
      var status = document.createElement("p");
      status.setAttribute("role", "status");
      status.textContent = "Loading diagram…";
      var details = document.createElement("details");
      details.open = true;
      var summary = document.createElement("summary");
      summary.textContent = "Diagram source";
      details.appendChild(summary);
      block.before(figure);
      details.appendChild(block);
      var frame = document.createElement("iframe");
      frame.title = "Mermaid diagram " + (++nextId);
      frame.setAttribute("sandbox", "allow-scripts");
      frame.setAttribute("referrerpolicy", "no-referrer");
      // Keep layout measurable while rendering: display:none gives SVG text
      // zero dimensions and breaks Mermaid's edge-label positioning.
      frame.style.cssText = "width:100%;border:0;height:160px;visibility:hidden";
      // No diagram input is interpolated into this document. The opaque-origin
      // frame receives only source, and its CSP blocks network calls and images.
      var origin = window.location.origin;
      frame.srcdoc = '<!doctype html><html lang="en"><head><meta charset="utf-8">' +
        '<meta http-equiv="Content-Security-Policy" content="default-src \'none\'; ' +
        'script-src ' + origin + '; style-src \'unsafe-inline\'; img-src \'none\'; ' +
        'connect-src \'none\'; base-uri \'none\'; form-action \'none\'">' +
        '<title>Mermaid diagram</title></head><body style="margin:0">' +
        '<div id="diagram"></div><script src="' + origin + '/public/vendor/mermaid/mermaid.min.js"></script>' +
        '<script src="' + origin + '/public/js/mermaid-frame.js"></script></body></html>';
      figure.append(caption, status, frame, details);
      var entry = { frame: frame, source: code.textContent, details: details, status: status };
      entry.timeout = setTimeout(function () {
        if (entry.rendered || !frame.isConnected) return;
        frame.hidden = true;
        status.textContent = "Diagram unavailable. The Mermaid source is shown below.";
      }, 20000);
      frames.set(frame.contentWindow, entry);
    });
    // Hydration/navigation may replace a Markdown body; release removed frames.
    frames.forEach(function (entry, key) {
      if (!entry.frame.isConnected) {
        clearTimeout(entry.timeout);
        frames.delete(key);
      }
    });
  }

  new MutationObserver(enhance).observe(document.documentElement, { childList: true, subtree: true });
  enhance();
})();
