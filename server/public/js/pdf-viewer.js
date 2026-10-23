// Render a filed PDF in place with the vendored PDF.js.
//
// The document page renders `.pdf-viewer[data-pdf-src]` with a fingerprinted,
// one-hour same-origin link. This script imports PDF.js from `/public/vendor`,
// fetches that link, and draws each page onto its own canvas. Everything stays
// same-origin, so the page's `script-src 'self'` and `connect-src` (via
// `default-src 'self'`) need nothing new. If anything fails, the status line
// says so and the Download button above still works.
(function () {
  "use strict";
  if (window.__navPdfViewer) return;
  window.__navPdfViewer = true;

  function setStatus(mount, text) {
    var status = mount.querySelector(".pdf-viewer__status");
    if (!status) {
      status = document.createElement("p");
      status.className = "muted pdf-viewer__status";
      mount.appendChild(status);
    }
    status.textContent = text;
    status.hidden = !text;
  }

  async function renderPage(pdf, number, mount) {
    var page = await pdf.getPage(number);
    var width = Math.max(mount.clientWidth - 32, 320);
    var base = page.getViewport({ scale: 1 });
    var scale = width / base.width;
    var ratio = window.devicePixelRatio || 1;
    var viewport = page.getViewport({ scale: scale * ratio });
    var canvas = document.createElement("canvas");
    canvas.className = "pdf-viewer__page";
    canvas.width = Math.floor(viewport.width);
    canvas.height = Math.floor(viewport.height);
    canvas.setAttribute("role", "img");
    canvas.setAttribute("aria-label", "Page " + number + " of " + pdf.numPages);
    mount.appendChild(canvas);
    await page.render({ canvasContext: canvas.getContext("2d"), viewport: viewport }).promise;
    page.cleanup();
  }

  async function mountViewer(mount) {
    if (mount.dataset.pdfMounted) return;
    mount.dataset.pdfMounted = "1";
    try {
      var pdfjs = await import(mount.dataset.pdfModule);
      pdfjs.GlobalWorkerOptions.workerSrc = mount.dataset.pdfWorker;
      var pdf = await pdfjs.getDocument({
        url: mount.dataset.pdfSrc,
        // `script-src` admits no `'unsafe-eval'`; PDF.js would otherwise
        // probe `new Function` and trip a CSP violation report.
        isEvalSupported: false,
        withCredentials: true,
      }).promise;
      setStatus(mount, "");
      for (var n = 1; n <= pdf.numPages; n++) {
        await renderPage(pdf, n, mount);
      }
    } catch (error) {
      var expired = error && (error.status === 403 || error.status === 410);
      setStatus(
        mount,
        expired
          ? "This preview link has expired. Reload the page for a fresh one."
          : "The preview could not be displayed. Use Download to open the file."
      );
    }
  }

  function enhance() {
    document.querySelectorAll(".pdf-viewer[data-pdf-src]").forEach(mountViewer);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", enhance);
  } else {
    enhance();
  }
})();
