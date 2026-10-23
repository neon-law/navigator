// Runs only in the viewer's opaque-origin sandbox with network access denied.
(function () {
  "use strict";
  var rendering = false;
  window.addEventListener("message", async function (event) {
    if (event.source !== window.parent || rendering || !event.data ||
        event.data.type !== "navigator-mermaid-source") return;
    rendering = true;
    try {
      var source = event.data.source;
      if (typeof source !== "string" || source.length > 50000) throw new Error("Source limit");
      mermaid.initialize({ startOnLoad: false, securityLevel: "strict", suppressErrorRendering: true,
        maxTextSize: 50000, maxEdges: 500, htmlLabels: false,
        secure: ["secure", "securityLevel", "startOnLoad", "maxTextSize", "maxEdges",
          "suppressErrorRendering", "htmlLabels", "dompurifyConfig"],
        flowchart: { htmlLabels: false }, theme: "neutral" });
      var result = await mermaid.render("navigatorDiagram", source);
      document.getElementById("diagram").innerHTML = result.svg;
      var svg = document.querySelector("svg");
      if (!svg) throw new Error("No diagram");
      svg.setAttribute("role", "img");
      if (!svg.getAttribute("aria-label") && !svg.getAttribute("aria-labelledby")) {
        svg.setAttribute("aria-label", "Mermaid diagram");
      }
      function reportSize() {
        window.parent.postMessage({ type: "navigator-mermaid", ok: true,
          height: Math.ceil(svg.getBoundingClientRect().height) + 16 }, "*");
      }
      new ResizeObserver(reportSize).observe(svg);
      reportSize();
    } catch (_) {
      // Parser errors may contain authored text. Return only a bounded status.
      window.parent.postMessage({ type: "navigator-mermaid", ok: false }, "*");
    }
  });
  window.parent.postMessage({ type: "navigator-mermaid", ready: true }, "*");
})();
